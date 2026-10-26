pub mod access;
pub mod cache;
pub mod default_page;
pub mod filters;
pub mod limit_conn;
pub mod limit_req;
pub mod matcher;
mod proxy;
pub mod realip;
pub mod server;
pub mod static_files;
pub mod upstream;
pub mod websocket;

use axum::body::{Body, Bytes};
use axum::extract::ws::WebSocketUpgrade;
use axum::extract::{FromRequestParts, Request};
use axum::http::header::{HeaderName, HeaderValue};
use axum::http::{header, HeaderMap, Method, StatusCode};
use axum::response::{IntoResponse, Response};
use futures_util::{StreamExt, TryStreamExt};
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tracing::warn;

use crate::config::store::ConfigStore;
use crate::config::{
    NextUpstreamCondition, RouteRule, RouteTargetType, UpstreamConfig, UpstreamTarget, VirtualHost,
};
use crate::health::HealthChecker;
use crate::metrics::logger::RequestLog;
use crate::metrics::MetricsCollector;
use access::{AccessControl, AccessDecision};
use cache::{build_cached_response, cache_bypass_matches, cache_key_for, ResponseCache};
use limit_conn::ConnLimiter;
use limit_req::{RateLimitDecision, RateLimiter};
use matcher::CompiledConfig;
use proxy::scheme_str;
pub use proxy::X_FERRIX_UPSTREAM;
use realip::RealIpResolver;
use static_files::{StaticFileServer, StaticRequest};
use upstream::balancer::UpstreamPoolManager;
use upstream::{OutgoingBody, UpstreamClient, UpstreamError};
use websocket::WebSocketProxy;

#[derive(Debug, Clone, Copy)]
struct SkipAccessLog;

#[derive(Clone)]
pub struct ProxyEngine {
    pub store: Arc<ConfigStore>,
    pub metrics: Arc<MetricsCollector>,
    pub health_checker: Arc<HealthChecker>,
    pub pool_manager: Arc<UpstreamPoolManager>,
    pub rate_limiter: Arc<RateLimiter>,
    pub conn_limiter: Arc<ConnLimiter>,
    pub response_cache: Arc<ResponseCache>,
    pub acme_challenges: Arc<crate::tls::acme::AcmeChallenges>,
}

impl ProxyEngine {
    pub fn new(
        store: Arc<ConfigStore>,
        metrics: Arc<MetricsCollector>,
        health_checker: Arc<HealthChecker>,
    ) -> Self {
        let pool_manager = Arc::new(UpstreamPoolManager::new(health_checker.clone()));
        let rate_limiter = Arc::new(RateLimiter::new());
        let conn_limiter = Arc::new(ConnLimiter::new());

        Self {
            store,
            metrics,
            health_checker,
            pool_manager,
            rate_limiter,
            conn_limiter,
            response_cache: ResponseCache::new(),
            acme_challenges: Arc::default(),
        }
    }

    pub fn start_background_sweeper(&self) {
        let rate_limiter = self.rate_limiter.clone();
        let conn_limiter = self.conn_limiter.clone();
        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(Duration::from_secs(60));
            loop {
                ticker.tick().await;
                rate_limiter.sweep();
                conn_limiter.sweep();
            }
        });
    }

    pub async fn handle_request(
        &self,
        listen_port: u16,
        is_tls: bool,
        client_addr: SocketAddr,
        req: Request,
    ) -> Response {
        let compiled = self.store.compiled();
        let metrics_on = compiled.config.metrics.enabled;
        // A precise clock read only when someone will look at the latency.
        let start_time = if metrics_on {
            Instant::now()
        } else {
            crate::event::clock::now()
        };
        if metrics_on {
            self.metrics.inc_active_connections();
        }

        let response = self
            .route_and_serve(
                &compiled,
                listen_port,
                is_tls,
                client_addr,
                req,
                start_time,
                metrics_on,
            )
            .await;

        if metrics_on {
            self.metrics.dec_active_connections();
        }
        // Like nginx, Ferrix answers with its own Server header (replacing an upstream's).
        let mut response = response;
        filters::headers::server(&compiled, response.headers_mut());
        response
    }

    #[allow(clippy::too_many_arguments)]
    async fn route_and_serve(
        &self,
        compiled: &CompiledConfig,
        listen_port: u16,
        is_tls: bool,
        client_addr: SocketAddr,
        req: Request,
        start_time: Instant,
        metrics_on: bool,
    ) -> Response {
        let method = req.method().clone();
        let uri = req.uri().clone();
        let req_headers = req.headers();

        let client_ip = compiled.real_ip.resolve(client_addr.ip(), req_headers);

        let host_header = req_headers
            .get(header::HOST)
            .cloned()
            .unwrap_or_else(|| HeaderValue::from_static(""));
        let user_agent = req_headers.get(header::USER_AGENT).cloned();

        let is_ws = WebSocketProxy::is_websocket_request(req_headers);

        let host = host_header
            .to_str()
            .unwrap_or("")
            .split(':')
            .next()
            .unwrap_or("");
        let acme_token = uri
            .path()
            .strip_prefix(crate::tls::acme::CHALLENGE_PREFIX)
            .and_then(|t| self.acme_challenges.key_authorization(t));
        let response = match compiled.find_vhost(host, listen_port) {
            _ if acme_token.is_some() => (
                [(header::CONTENT_TYPE, "application/octet-stream")],
                acme_token.unwrap_or_default(),
            )
                .into_response(),
            Some(m) => {
                let vhost = &compiled.config.vhosts[m.vhost_index];
                let resp = self
                    .serve_vhost(
                        compiled,
                        m.vhost_index,
                        vhost,
                        is_tls,
                        client_ip,
                        req,
                        is_ws,
                    )
                    .await;
                Box::pin(filters::error_page::apply(vhost, resp)).await
            }
            None if compiled.config.http.default_page => {
                default_page::render(compiled.config.http.server_tokens)
            }
            None => (
                StatusCode::NOT_FOUND,
                "404 Not Found - No matching virtual host configured in Ferrix",
            )
                .into_response(),
        };

        if !metrics_on {
            return response;
        }

        let elapsed_ms = start_time.elapsed().as_secs_f64() * 1000.0;
        let status_code = response.status().as_u16();
        let skip_log = response.extensions().get::<SkipAccessLog>().is_some();

        let log = RequestLog {
            id: next_request_id(),
            timestamp: crate::event::clock::utc_now(),
            client_ip,
            method,
            uri,
            host: host_header,
            status: status_code,
            latency_ms: (elapsed_ms * 100.0).round() / 100.0,

            bytes_sent: response
                .headers()
                .get(header::CONTENT_LENGTH)
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.parse().ok())
                .unwrap_or(0),
            upstream: response.headers().get(X_FERRIX_UPSTREAM).cloned(),
            user_agent,
            is_websocket: is_ws,
        };

        self.metrics.record_request(log, !skip_log);
        self.count_response_bytes(response)
    }

    fn count_response_bytes(&self, response: Response) -> Response {
        if response.status() == StatusCode::SWITCHING_PROTOCOLS {
            return response;
        }

        if let Some(len) = axum::body::HttpBody::size_hint(response.body()).exact() {
            self.metrics.add_bytes_sent(len);
            return response;
        }

        let (parts, body) = response.into_parts();
        let metrics = self.metrics.clone();
        let counted = body
            .into_data_stream()
            .inspect_ok(move |chunk| metrics.add_bytes_sent(chunk.len() as u64))
            .map_err(std::io::Error::other);

        Response::from_parts(parts, Body::from_stream(counted))
    }

    #[allow(clippy::too_many_arguments)]
    async fn serve_vhost(
        &self,
        compiled: &CompiledConfig,
        vhost_index: usize,
        vhost: &VirtualHost,
        is_tls: bool,
        client_ip: IpAddr,
        req: Request,
        is_ws: bool,
    ) -> Response {
        let headers = req.headers();

        let ip_ok = match compiled.vhost_acl(vhost_index) {
            Some((allow, deny)) => AccessControl::is_ip_allowed(client_ip, allow, deny),
            None => true,
        };

        match AccessControl::evaluate(vhost.satisfy, ip_ok, vhost.basic_auth.as_ref(), headers) {
            AccessDecision::Allow => {}
            AccessDecision::Forbidden => {
                warn!(ip = %client_ip, "IP recusado pelo controle de acesso");
                return (
                    StatusCode::FORBIDDEN,
                    "403 Forbidden - Access Denied by Ferrix ACL",
                )
                    .into_response();
            }
            AccessDecision::Unauthorized => {
                let mut resp = (StatusCode::UNAUTHORIZED, "401 Unauthorized").into_response();
                resp.headers_mut().insert(
                    header::WWW_AUTHENTICATE,
                    HeaderValue::from_static("Basic realm=\"Ferrix Protected Area\""),
                );
                return resp;
            }
        }

        let _vhost_conn_guard = match &vhost.limit_conn {
            Some(cfg) => {
                let key = ("vhost", vhost.id.as_str(), client_ip);
                match self.conn_limiter.acquire(key, cfg) {
                    Some(g) => Some(g),
                    None => return limit_response(cfg.status, "connection limit"),
                }
            }
            None => None,
        };

        if let Some(rl) = &vhost.rate_limit {
            let key = ("vhost", vhost.id.as_str(), client_ip);
            match self.rate_limiter.check(key, rl) {
                RateLimitDecision::Allow => {}
                RateLimitDecision::Delay(d) => tokio::time::sleep(d).await,
                RateLimitDecision::Reject => {
                    warn!(ip = %client_ip, "limite de taxa do host excedido");
                    return rate_limit_response(rl);
                }
            }
        }

        self.process_vhost_routes(compiled, vhost_index, vhost, is_tls, client_ip, req, is_ws)
            .await
    }

    #[allow(clippy::too_many_arguments)]
    async fn process_vhost_routes(
        &self,
        compiled: &CompiledConfig,
        vhost_index: usize,
        vhost: &VirtualHost,
        is_tls: bool,
        client_ip: IpAddr,
        req: Request,
        is_ws: bool,
    ) -> Response {
        let req_path = req.uri().path().to_string();

        let Some(matched) = compiled.match_route(vhost_index, &req_path) else {
            // A host with no routes at all is a fresh setup: greet like nginx's default site.
            if vhost.routes.is_empty() && compiled.config.http.default_page {
                return default_page::render(compiled.config.http.server_tokens);
            }
            return (StatusCode::NOT_FOUND, "404 Not Found - No route matched").into_response();
        };
        let route = &vhost.routes[matched.route_index];

        if !route.allowed_ips.is_empty()
            || !route.denied_ips.is_empty()
            || route.basic_auth.is_some()
        {
            let route_ip_ok = match compiled.route_acl(vhost_index, matched.route_index) {
                Some((allow, deny)) => AccessControl::is_ip_allowed(client_ip, allow, deny),
                None => true,
            };

            match AccessControl::evaluate(
                route.satisfy,
                route_ip_ok,
                route.basic_auth.as_ref(),
                req.headers(),
            ) {
                AccessDecision::Allow => {}
                AccessDecision::Forbidden => {
                    return (
                        StatusCode::FORBIDDEN,
                        "403 Forbidden - Access Denied by Ferrix ACL",
                    )
                        .into_response();
                }
                AccessDecision::Unauthorized => {
                    let mut resp = (StatusCode::UNAUTHORIZED, "401 Unauthorized").into_response();
                    resp.headers_mut().insert(
                        header::WWW_AUTHENTICATE,
                        HeaderValue::from_static("Basic realm=\"Ferrix Protected Area\""),
                    );
                    return resp;
                }
            }
        }

        let _route_conn_guard = match &route.limit_conn {
            Some(cfg) => {
                let key = ("route", route.id.as_str(), client_ip);
                match self.conn_limiter.acquire(key, cfg) {
                    Some(g) => Some(g),
                    None => return limit_response(cfg.status, "connection limit"),
                }
            }
            None => None,
        };

        if let Some(rl) = &route.rate_limit {
            let key = ("route", route.id.as_str(), client_ip);
            match self.rate_limiter.check(key, rl) {
                RateLimitDecision::Allow => {}
                RateLimitDecision::Delay(d) => tokio::time::sleep(d).await,
                RateLimitDecision::Reject => return rate_limit_response(rl),
            }
        }

        let (mut parts, body) = req.into_parts();

        let info = match filters::RequestInfo::capture(compiled, route, &mut parts) {
            Ok(info) => info,
            Err(preflight) => return *preflight,
        };
        let compression = &compiled.config.http.compression;

        let resp = match route.target_type {
            RouteTargetType::Redirect => {
                let code = route.redirect_code.unwrap_or(302);
                let template = route.redirect_url.as_deref().unwrap_or("/");
                let status = StatusCode::from_u16(code).unwrap_or(StatusCode::FOUND);
                let target_url = interpolate_redirect_url(template, &parts, is_tls);

                let mut r = (status, "").into_response();
                if let Ok(val) = HeaderValue::from_str(&target_url) {
                    r.headers_mut().insert(header::LOCATION, val);
                }
                r
            }
            RouteTargetType::StaticFiles => {
                let root_dir = route.static_root.as_deref().unwrap_or("./public");
                let file_path = if route.strip_prefix {
                    req_path
                        .strip_prefix(&route.path_prefix)
                        .unwrap_or(&req_path)
                } else {
                    &req_path
                };

                let static_req = StaticRequest {
                    headers: &parts.headers,
                    encoding: info.encoding.map(|a| (a, compression)),
                };
                // Boxed (like the cache and error-page paths) so a proxied request's
                // state machine doesn't carry the size of the paths it doesn't take.
                match Box::pin(StaticFileServer::serve_file(
                    root_dir,
                    file_path,
                    route.try_files.as_deref(),
                    &static_req,
                ))
                .await
                {
                    Ok(r) => r,
                    Err(code) => {
                        (code, format!("{} Static File Error", code.as_u16())).into_response()
                    }
                }
            }
            RouteTargetType::ProxyUpstream => {
                let Some(upstream_name) = &route.upstream_name else {
                    return (
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "500 Misconfigured Upstream Route",
                    )
                        .into_response();
                };

                let Some(upstream_cfg) = compiled.config.upstreams.get(upstream_name) else {
                    return (
                        StatusCode::BAD_GATEWAY,
                        format!("502 Bad Gateway - Upstream '{}' not found", upstream_name),
                    )
                        .into_response();
                };

                let targets = match compiled.hash_ring(upstream_name) {
                    Some(ring) => {
                        let hash = ring.key.hash(&parts, client_ip);
                        self.pool_manager
                            .select_targets_hashed(upstream_cfg, ring, hash)
                    }
                    None => self.pool_manager.select_targets(upstream_cfg, client_ip),
                };
                if targets.is_empty() {
                    return (
                        StatusCode::BAD_GATEWAY,
                        "502 Bad Gateway - No upstream targets available",
                    )
                        .into_response();
                }

                if is_ws && route.websocket_enabled {
                    let target = &targets[0];
                    let ws_path = build_target_path(&parts.uri, &req_path, route);
                    let target_url = target.url.clone();

                    if let Ok(ws_upgrade) =
                        Box::pin(WebSocketUpgrade::from_request_parts(&mut parts, &())).await
                    {
                        return ws_upgrade
                            .on_upgrade(move |socket| async move {
                                WebSocketProxy::tunnel(socket, &target_url, &ws_path).await;
                            })
                            .into_response();
                    }
                }

                let cacheable_method = parts.method == Method::GET || parts.method == Method::HEAD;
                let cache_cfg = route
                    .cache
                    .as_ref()
                    .filter(|_| cacheable_method)
                    .filter(|cfg| !cache_bypass_matches(cfg, &parts));

                if let Some(cfg) = cache_cfg {
                    let key = cache_key_for(vhost, &parts);
                    if let Some(cached) = self.response_cache.get(&key) {
                        let mut r = build_cached_response(&cached);
                        if let Some(v) = crate::http::cache::header_value("HIT") {
                            r.headers_mut().insert("x-cache-status", v);
                        }
                        r
                    } else {
                        let restored_req = Request::from_parts(parts, body);
                        let inner = self
                            .proxy_http_request(
                                compiled,
                                route,
                                upstream_name,
                                upstream_cfg,
                                targets,
                                is_tls,
                                client_ip,
                                restored_req,
                            )
                            .await;
                        Box::pin(self.store_if_cacheable(key, cfg, inner)).await
                    }
                } else {
                    let restored_req = Request::from_parts(parts, body);
                    self.proxy_http_request(
                        compiled,
                        route,
                        upstream_name,
                        upstream_cfg,
                        targets,
                        is_tls,
                        client_ip,
                        restored_req,
                    )
                    .await
                }
            }
        };

        let mut resp = filters::run(
            compiled,
            vhost_index,
            matched.route_index,
            route,
            info,
            resp,
        );

        if !route.access_log {
            resp.extensions_mut().insert(SkipAccessLog);
        }

        resp
    }
}

fn build_target_path(uri: &axum::http::Uri, req_path: &str, route: &RouteRule) -> String {
    let base = if route.strip_prefix {
        req_path
            .strip_prefix(&route.path_prefix)
            .unwrap_or(req_path)
            .to_string()
    } else {
        req_path.to_string()
    };
    let base = if base.starts_with('/') {
        base
    } else {
        format!("/{}", base)
    };
    match uri.query() {
        Some(q) => format!("{}?{}", base, q),
        None => base,
    }
}

// Unique per process without a shared atomic: the high bits identify the thread, the low
// bits count requests on it, so workers never contend on the same cache line.
fn next_request_id() -> u64 {
    use std::cell::Cell;
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT_THREAD: AtomicU64 = AtomicU64::new(1);
    thread_local! {
        static NEXT: Cell<u64> = Cell::new(NEXT_THREAD.fetch_add(1, Ordering::Relaxed) << 40);
    }
    NEXT.with(|n| {
        let id = n.get();
        n.set(id + 1);
        id
    })
}

fn interpolate_redirect_url(
    template: &str,
    parts: &axum::http::request::Parts,
    is_tls: bool,
) -> String {
    if !template.contains('$') {
        return template.to_string();
    }

    let host = parts
        .headers
        .get(header::HOST)
        .and_then(|h| h.to_str().ok())
        .unwrap_or("")
        .split(':')
        .next()
        .unwrap_or("");
    let request_uri = parts
        .uri
        .path_and_query()
        .map(|pq| pq.as_str())
        .unwrap_or("/");

    template
        .replace("$scheme", scheme_str(is_tls))
        .replace("$request_uri", request_uri)
        .replace("$host", host)
}

fn rate_limit_response(cfg: &crate::config::RateLimitConfig) -> Response {
    let status = StatusCode::from_u16(cfg.status).unwrap_or(StatusCode::TOO_MANY_REQUESTS);
    let mut resp = (status, "429 Too Many Requests - Rate Limit Exceeded").into_response();
    if let Some(seconds) = cfg.retry_after {
        if let Ok(v) = HeaderValue::from_str(&seconds.to_string()) {
            resp.headers_mut().insert(header::RETRY_AFTER, v);
        }
    }
    resp
}

fn limit_response(status: u16, what: &str) -> Response {
    let status = StatusCode::from_u16(status).unwrap_or(StatusCode::TOO_MANY_REQUESTS);
    (status, format!("{} - Ferrix {}", status.as_u16(), what)).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn route_with_prefix(prefix: &str, strip: bool) -> RouteRule {
        RouteRule {
            id: "r".into(),
            path_prefix: prefix.into(),
            strip_prefix: strip,
            upstream_name: Some("b".into()),
            ..Default::default()
        }
    }

    fn parts_with(host: &str, uri: &str) -> axum::http::request::Parts {
        let req = Request::builder()
            .uri(uri)
            .header(header::HOST, host)
            .body(Body::empty())
            .unwrap();
        req.into_parts().0
    }

    #[test]
    fn scheme_str_reflects_tls() {
        assert_eq!(scheme_str(true), "https");
        assert_eq!(scheme_str(false), "http");
    }

    #[test]
    fn redirect_without_variables_is_returned_unchanged() {
        let parts = parts_with("exemplo.com", "/qualquer");
        assert_eq!(
            interpolate_redirect_url("https://outro.com/fixo", &parts, false),
            "https://outro.com/fixo"
        );
    }

    #[test]
    fn redirect_interpolates_scheme_host_and_request_uri() {
        let parts = parts_with("exemplo.com", "/caminho?x=1");
        assert_eq!(
            interpolate_redirect_url("$scheme://$host$request_uri", &parts, true),
            "https://exemplo.com/caminho?x=1"
        );
    }

    #[test]
    fn redirect_host_variable_strips_the_port() {
        let parts = parts_with("exemplo.com:8080", "/x");
        assert_eq!(
            interpolate_redirect_url("$scheme://$host$request_uri", &parts, true),
            "https://exemplo.com/x"
        );
    }

    #[test]
    fn redirect_scheme_reflects_the_actual_connection_not_a_fixed_value() {
        let parts = parts_with("exemplo.com", "/x");
        assert_eq!(
            interpolate_redirect_url("$scheme://$host$request_uri", &parts, false),
            "http://exemplo.com/x"
        );
    }

    #[test]
    fn target_path_keeps_query_string() {
        let uri: axum::http::Uri = "/api/users?ativo=1".parse().unwrap();
        let route = route_with_prefix("/api", false);
        assert_eq!(
            build_target_path(&uri, "/api/users", &route),
            "/api/users?ativo=1"
        );
    }

    #[test]
    fn strip_prefix_keeps_a_leading_slash() {
        let uri: axum::http::Uri = "/api/users".parse().unwrap();
        let route = route_with_prefix("/api", true);
        assert_eq!(build_target_path(&uri, "/api/users", &route), "/users");
    }

    #[test]
    fn strip_prefix_of_the_whole_path_yields_root() {
        let uri: axum::http::Uri = "/api".parse().unwrap();
        let route = route_with_prefix("/api", true);
        assert_eq!(build_target_path(&uri, "/api", &route), "/");
    }
}
