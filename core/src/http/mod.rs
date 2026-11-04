pub mod access;
pub mod access_log;
pub mod auth_request;
pub mod cache;
pub mod dav;
pub mod default_page;
pub mod filters;
#[cfg(feature = "geoip")]
pub mod geoip;
#[cfg(feature = "http3")]
pub mod h3;
pub mod limit_conn;
pub mod limit_req;
pub mod matcher;
pub mod mirror;
mod proxy;
mod raw_proxy;
pub mod realip;
pub mod referer;
pub mod secure_link;
pub mod server;
pub mod static_files;
pub mod tunnel;
pub mod upstream;
pub mod vars;
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
use cache::{cache_bypass_matches, cache_key_for, ResponseCache};
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

        let req = if metrics_on {
            req
        } else {
            match self
                .plain_proxy(&compiled, listen_port, is_tls, client_addr, req)
                .await
            {
                Ok(mut response) => {
                    filters::headers::server(&compiled, response.headers_mut());
                    return response;
                }
                Err(req) => req,
            }
        };
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
        let mut response = response;
        filters::headers::server(&compiled, response.headers_mut());
        response
    }

    #[allow(clippy::result_large_err)]
    async fn plain_proxy(
        &self,
        compiled: &CompiledConfig,
        listen_port: u16,
        is_tls: bool,
        client_addr: SocketAddr,
        req: Request,
    ) -> Result<Response, Request> {
        let headers = req.headers();
        if headers.contains_key(header::UPGRADE)
            || req
                .uri()
                .path()
                .starts_with(crate::tls::acme::CHALLENGE_PREFIX)
        {
            return Err(req);
        }
        let host = headers
            .get(header::HOST)
            .and_then(|h| h.to_str().ok())
            .unwrap_or("");
        let host = host.split(':').next().unwrap_or("");
        let Some(vm) = compiled.find_vhost(host, listen_port) else {
            return Err(req);
        };
        let Some(rm) = compiled.match_route(vm.vhost_index, req.uri().path()) else {
            return Err(req);
        };
        if !compiled.plain_proxy(vm.vhost_index, rm.route_index) {
            return Err(req);
        }
        let route = &compiled.config.vhosts[vm.vhost_index].routes[rm.route_index];
        let Some((name, upstream)) = route
            .upstream_name
            .as_ref()
            .and_then(|n| compiled.config.upstreams.get_key_value(n))
        else {
            return Err(req);
        };
        let client_ip = compiled.real_ip.resolve(client_addr.ip(), headers);
        let targets = self.pool_manager.select_targets(upstream, client_ip);
        if targets.is_empty() {
            return Err(req);
        }
        Ok(self
            .proxy_http_request(
                compiled, route, name, upstream, targets, is_tls, client_ip, req,
            )
            .await)
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
        let access_log = compiled.config.http.access_log.as_ref();
        let referer = access_log.and_then(|_| req_headers.get(header::REFERER).cloned());

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
                let resp =
                    absolutize_redirect(resp, &compiled.config.http, vhost, &host_header, is_tls);
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

        if !metrics_on && access_log.is_none() {
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

        if let Some(al) = access_log.filter(|_| !skip_log) {
            let extra = access_log::Extra {
                referer: referer.as_ref().and_then(|v| v.to_str().ok()).unwrap_or(""),
                remote_user: "",
                scheme: scheme_str(is_tls),
            };
            access_log::write(&al.path, &al.format, &log, &extra);
        }

        if !metrics_on {
            return response;
        }
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
        if let Some(tls) = vhost.tls.as_ref().filter(|_| is_tls) {
            if tls.verify_client == crate::config::ClientVerify::On
                && req
                    .extensions()
                    .get::<Arc<crate::tls::client_cert::ClientCert>>()
                    .is_none()
            {
                return (
                    StatusCode::BAD_REQUEST,
                    "400 Bad Request - No required SSL certificate was sent",
                )
                    .into_response();
            }
        }

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

    async fn serve_cgi(
        &self,
        compiled: &CompiledConfig,
        route: &RouteRule,
        parts: axum::http::request::Parts,
        body: Body,
        client_ip: IpAddr,
        is_tls: bool,
    ) -> Response {
        let Some(cfg) = &route.cgi else {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                "500 Misconfigured CGI Route",
            )
                .into_response();
        };
        let http = &compiled.config.http;
        let body = match axum::body::to_bytes(body, http.client_max_body_size as usize).await {
            Ok(b) => b,
            Err(_) => {
                return (StatusCode::PAYLOAD_TOO_LARGE, "413 Payload Too Large").into_response()
            }
        };
        let addrs: Vec<String> = match compiled.config.upstreams.get(&cfg.pass) {
            Some(up) => self
                .pool_manager
                .select_targets(up, client_ip)
                .into_iter()
                .map(|t| t.url.trim_start_matches("http://").to_string())
                .collect(),
            None => vec![cfg.pass.clone()],
        };
        let root = cfg
            .root
            .as_deref()
            .or(route.static_root.as_deref())
            .unwrap_or("/var/www/html");
        let path = if route.strip_prefix {
            parts
                .uri
                .path()
                .strip_prefix(&route.path_prefix)
                .unwrap_or(parts.uri.path())
        } else {
            parts.uri.path()
        };
        let path = if path.starts_with('/') {
            path.to_string()
        } else {
            format!("/{path}")
        };
        let (script, path_info) = upstream::cgi::split_script(&path, cfg.index.as_deref());
        let params = upstream::cgi::build_params(
            &parts,
            root,
            &script,
            &path_info,
            client_ip,
            is_tls,
            body.len(),
            &cfg.params,
        );
        let connect = Duration::from_millis(
            route
                .proxy_connect_timeout_ms
                .unwrap_or(http.proxy_connect_timeout_ms),
        );
        let read = Duration::from_millis(
            route
                .proxy_read_timeout_ms
                .unwrap_or(http.proxy_read_timeout_ms),
        );
        let mut last = String::from("no CGI backend available");
        for addr in addrs {
            match upstream::cgi::call(cfg.protocol, &addr, &params, &body, connect, read).await {
                Ok(r) => return r,
                Err(e) => {
                    warn!(backend = %addr, error = %e, "CGI backend failed");
                    last = e;
                }
            }
        }
        (StatusCode::BAD_GATEWAY, format!("502 Bad Gateway - {last}")).into_response()
    }

    async fn serve_memcached(
        &self,
        compiled: &CompiledConfig,
        route: &RouteRule,
        parts: &axum::http::request::Parts,
        client_ip: IpAddr,
        is_tls: bool,
    ) -> Response {
        let Some(cfg) = &route.memcached else {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                "500 Misconfigured memcached route",
            )
                .into_response();
        };
        let ctx = vars::Ctx {
            method: &parts.method,
            uri: parts.uri.clone(),
            headers: &parts.headers,
            client_ip,
            is_tls,
            http: &compiled.config.http,
            set: parts
                .extensions
                .get::<vars::SetVars>()
                .map(|s| s.0.clone())
                .unwrap_or_default(),
            captures: Vec::new(),
            client_cert: None,
        };
        let key = ctx.expand(cfg.key.as_deref().unwrap_or("$uri"));
        let http = &compiled.config.http;
        let connect = Duration::from_millis(
            route
                .proxy_connect_timeout_ms
                .unwrap_or(http.proxy_connect_timeout_ms),
        );
        let read = Duration::from_millis(
            route
                .proxy_read_timeout_ms
                .unwrap_or(http.proxy_read_timeout_ms),
        );
        match upstream::memcached::get(&cfg.pass, &key, connect, read).await {
            Ok(upstream::memcached::Reply::Hit { flags, data }) => {
                let mut r = Response::new(Body::from(data));
                let ct = cfg.content_type.as_deref().unwrap_or("text/html");
                if let Ok(v) = HeaderValue::from_str(ct) {
                    r.headers_mut().insert(header::CONTENT_TYPE, v);
                }
                if cfg.gzip_flag.is_some_and(|f| flags & f != 0) {
                    r.headers_mut()
                        .insert(header::CONTENT_ENCODING, HeaderValue::from_static("gzip"));
                }
                r
            }
            Ok(upstream::memcached::Reply::Miss) => {
                (StatusCode::NOT_FOUND, "404 Not Found").into_response()
            }
            Err(e) => {
                warn!(memcached = %cfg.pass, error = %e, "memcached failed");
                (StatusCode::BAD_GATEWAY, "502 Bad Gateway").into_response()
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    async fn subrequest(
        &self,
        compiled: &CompiledConfig,
        vhost_index: usize,
        vhost: &VirtualHost,
        is_tls: bool,
        client_ip: IpAddr,
        uri: &str,
        mut headers: HeaderMap,
    ) -> Option<Bytes> {
        for h in [
            header::RANGE,
            header::ACCEPT_ENCODING,
            header::IF_NONE_MATCH,
            header::IF_MODIFIED_SINCE,
            header::CONTENT_LENGTH,
            header::TRANSFER_ENCODING,
        ] {
            headers.remove(h);
        }
        let resp = if uri.starts_with("http://") || uri.starts_with("https://") {
            let host_at = uri.find("://").map(|i| i + 3).unwrap_or(0);
            let (base, path) = match uri[host_at..].find('/') {
                Some(i) => (&uri[..host_at + i], &uri[host_at + i..]),
                None => (uri, "/"),
            };
            headers.remove(header::HOST);
            UpstreamClient::new(Duration::from_secs(5), Duration::from_secs(10), true)
                .send(&Method::GET, base, path, &headers, OutgoingBody::Empty)
                .await
                .ok()?
        } else {
            let mut req = Request::builder()
                .method(Method::GET)
                .uri(uri)
                .body(Body::empty())
                .ok()?;
            *req.headers_mut() = headers;
            req.extensions_mut().insert(Subrequest);
            Box::pin(self.process_vhost_routes(
                compiled,
                vhost_index,
                vhost,
                is_tls,
                client_ip,
                req,
                false,
            ))
            .await
        };
        if !resp.status().is_success() {
            return None;
        }
        axum::body::to_bytes(resp.into_body(), 4 << 20).await.ok()
    }

    fn rewrite_phase(
        &self,
        compiled: &CompiledConfig,
        vhost_index: usize,
        vhost: &VirtualHost,
        is_tls: bool,
        client_ip: IpAddr,
        req: &mut Request,
    ) -> Result<(), Box<Response>> {
        let (uri, set) = {
            let mut ctx = vars::Ctx {
                method: req.method(),
                uri: req.uri().clone(),
                headers: req.headers(),
                client_ip,
                is_tls,
                http: &compiled.config.http,
                set: req
                    .extensions()
                    .get::<vars::SetVars>()
                    .map(|s| s.0.clone())
                    .unwrap_or_default(),
                captures: Vec::new(),
                client_cert: req
                    .extensions()
                    .get::<Arc<crate::tls::client_cert::ClientCert>>()
                    .cloned(),
            };
            let mut phase =
                vars::run_route_phase(&mut ctx, &vhost.conditions, &[], &vhost.rewrites, None);
            for _ in 0..10 {
                if let vars::Phase::Return {
                    code,
                    location_or_body,
                } = phase
                {
                    return Err(Box::new(phase_response(code, location_or_body)));
                }
                let Some(m) = compiled.match_route(vhost_index, ctx.uri.path()) else {
                    break;
                };
                let route = &vhost.routes[m.route_index];
                phase = vars::run_route_phase(
                    &mut ctx,
                    &route.conditions,
                    &route.set,
                    &route.rewrites,
                    route.static_root.as_deref(),
                );
                if !matches!(phase, vars::Phase::Rematch) {
                    if let vars::Phase::Return {
                        code,
                        location_or_body,
                    } = phase
                    {
                        return Err(Box::new(phase_response(code, location_or_body)));
                    }
                    break;
                }
            }
            (ctx.uri, ctx.set)
        };
        *req.uri_mut() = uri;
        if !set.is_empty() {
            req.extensions_mut().insert(vars::SetVars(set));
        }
        Ok(())
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
        let mut req = req;
        if compiled.config.http.merge_slashes && req.uri().path().contains("//") {
            if let Some(u) = merged_slashes_uri(req.uri()) {
                *req.uri_mut() = u;
            }
        }
        if compiled.buffers_body_for_vars && req.extensions().get::<Subrequest>().is_none() {
            req =
                buffer_request_body_var(req, compiled.config.http.proxy_request_buffer_size).await;
        }
        if compiled.has_rewrite_phase(vhost_index) {
            if let Err(resp) =
                self.rewrite_phase(compiled, vhost_index, vhost, is_tls, client_ip, &mut req)
            {
                return *resp;
            }
        }
        let req_path = if req.method() == Method::CONNECT {
            "/".to_string()
        } else {
            req.uri().path().to_string()
        };

        let Some(matched) = compiled.match_route(vhost_index, &req_path) else {
            if (vhost.routes.is_empty() || req_path == "/") && compiled.config.http.default_page {
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

        if req.method() == Method::CONNECT {
            return match &route.tunnel {
                Some(cfg) => {
                    let connect = Duration::from_millis(
                        route
                            .proxy_connect_timeout_ms
                            .unwrap_or(compiled.config.http.proxy_connect_timeout_ms),
                    );
                    Box::pin(tunnel::handle(cfg, req, connect)).await
                }
                None => (StatusCode::METHOD_NOT_ALLOWED, "405 Method Not Allowed").into_response(),
            };
        }

        if route.empty_gif {
            return static_files::empty_gif();
        }

        if let Some(le) = &route.limit_except {
            let m = req.method().as_str();
            let ok = le.methods.iter().any(|a| {
                a.eq_ignore_ascii_case(m) || (m == "HEAD" && a.eq_ignore_ascii_case("GET"))
            });
            if !ok {
                let (allow, _) = crate::net::cidr::IpMatcher::compile(&le.allow);
                if !allow.matches(client_ip) {
                    return (StatusCode::FORBIDDEN, "403 Forbidden").into_response();
                }
            }
        }

        if !route.valid_referers.is_empty() {
            let referer = req
                .headers()
                .get(header::REFERER)
                .and_then(|v| v.to_str().ok());
            if !referer::is_valid(&route.valid_referers, referer, &vhost.server_names) {
                return (StatusCode::FORBIDDEN, "403 Forbidden - Invalid Referer").into_response();
            }
        }

        if let Some(cfg) = &route.secure_link {
            let host = req
                .headers()
                .get(header::HOST)
                .and_then(|v| v.to_str().ok())
                .unwrap_or("")
                .split(':')
                .next()
                .unwrap_or("");
            match secure_link::check(cfg, req.uri(), client_ip, host) {
                secure_link::Check::Ok => {}
                secure_link::Check::Invalid => {
                    return (StatusCode::FORBIDDEN, "403 Forbidden").into_response()
                }
                secure_link::Check::Expired => {
                    return (StatusCode::GONE, "410 Gone - Link expired").into_response()
                }
            }
        }

        let mut req = req;
        if let Some(cfg) = &route.auth_request {
            match Box::pin(auth_request::check(
                cfg,
                req.method(),
                req.uri(),
                req.headers(),
            ))
            .await
            {
                auth_request::Outcome::Allow(pass) => {
                    for (n, v) in pass {
                        req.headers_mut().insert(n, v);
                    }
                }
                auth_request::Outcome::Deny(resp) => return resp,
            }
        }

        for base in &route.mirror {
            mirror::spawn(base, req.method(), req.uri(), req.headers());
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

        let (req_uri, ssi_method, ssi_headers) = if route.ssi {
            (
                req.uri().clone(),
                req.method().clone(),
                req.headers().clone(),
            )
        } else {
            (axum::http::Uri::default(), Method::GET, HeaderMap::new())
        };
        let has_addition = (route.add_before_body.is_some() || route.add_after_body.is_some())
            && req.extensions().get::<Subrequest>().is_none();
        let addition_headers = if has_addition {
            req.headers().clone()
        } else {
            HeaderMap::new()
        };
        let needs_gunzip = route.gunzip && !filters::gunzip::client_accepts_gzip(req.headers());
        let (mut parts, body) = req.into_parts();
        let userid_cookie = route
            .userid
            .as_ref()
            .and_then(|cfg| filters::userid::ensure(cfg, &mut parts.headers));

        let info = match filters::RequestInfo::capture(compiled, route, &mut parts) {
            Ok(info) => info,
            Err(preflight) => return *preflight,
        };
        let compression = &compiled.config.http.compression;
        if route.sub_filter.is_some() {
            parts.headers.remove(header::ACCEPT_ENCODING);
        }

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
            RouteTargetType::Memcached => {
                Box::pin(self.serve_memcached(compiled, route, &parts, client_ip, is_tls)).await
            }
            RouteTargetType::Cgi => {
                Box::pin(self.serve_cgi(compiled, route, parts, body, client_ip, is_tls)).await
            }
            RouteTargetType::Return => {
                let code = route.redirect_code.unwrap_or(200);
                let status = StatusCode::from_u16(code).unwrap_or(StatusCode::OK);
                let body = route
                    .return_body
                    .as_deref()
                    .map(|b| interpolate_redirect_url(b, &parts, is_tls))
                    .unwrap_or_default();
                let mut r = (status, body).into_response();
                r.headers_mut().insert(
                    header::CONTENT_TYPE,
                    HeaderValue::from_static("text/plain; charset=utf-8"),
                );
                r
            }
            RouteTargetType::StaticFiles
                if route
                    .dav
                    .as_ref()
                    .is_some_and(|d| dav::handles(d, &parts.method)) =>
            {
                let root_dir = route.static_root.as_deref().unwrap_or("./public");
                let path = if route.strip_prefix {
                    req_path
                        .strip_prefix(&route.path_prefix)
                        .unwrap_or(&req_path)
                } else {
                    &req_path
                };
                let dav_cfg = route.dav.as_ref().expect("checked above");
                Box::pin(dav::handle(
                    dav_cfg,
                    root_dir,
                    path,
                    &parts.method,
                    &parts.headers,
                    body,
                    compiled.config.http.client_max_body_size,
                ))
                .await
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
                    cache: &compiled.config.http.static_cache,
                };
                let random_pick;
                let file_path = if route.random_index {
                    random_pick = static_files::random_index(root_dir, file_path);
                    random_pick.as_deref().unwrap_or(file_path)
                } else {
                    file_path
                };
                if route.flv {
                    if let Some(r) =
                        Box::pin(static_files::flv(root_dir, file_path, parts.uri.query())).await
                    {
                        return r;
                    }
                }
                match Box::pin(StaticFileServer::serve_file(
                    root_dir,
                    file_path,
                    route.try_files.as_deref(),
                    &static_req,
                ))
                .await
                {
                    Ok(r) => r,
                    Err(StatusCode::NOT_FOUND) if route.autoindex => {
                        match static_files::autoindex(root_dir, file_path, &req_path) {
                            Some(r) => r,
                            None => {
                                (StatusCode::NOT_FOUND, "404 Static File Error").into_response()
                            }
                        }
                    }
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

                let dynamic_name;
                let upstream_name = if let Some(var) = upstream_name.strip_prefix('$') {
                    let ctx = vars::Ctx {
                        method: &parts.method,
                        uri: parts.uri.clone(),
                        headers: &parts.headers,
                        client_ip,
                        is_tls,
                        http: &compiled.config.http,
                        set: parts
                            .extensions
                            .get::<vars::SetVars>()
                            .map(|s| s.0.clone())
                            .unwrap_or_default(),
                        captures: Vec::new(),
                        client_cert: parts
                            .extensions
                            .get::<Arc<crate::tls::client_cert::ClientCert>>()
                            .cloned(),
                    };
                    dynamic_name = ctx.var(var);
                    &dynamic_name
                } else {
                    upstream_name
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
                let mut targets = targets;
                if let Some(sc) = &upstream_cfg.sticky {
                    upstream::sticky::prefer(&mut targets, &parts.headers, sc);
                }
                if targets.is_empty() {
                    return (
                        StatusCode::BAD_GATEWAY,
                        "502 Bad Gateway - No upstream targets available",
                    )
                        .into_response();
                }

                #[cfg(feature = "http2")]
                if route.grpc {
                    let connect = Duration::from_millis(
                        route
                            .proxy_connect_timeout_ms
                            .unwrap_or(compiled.config.http.proxy_connect_timeout_ms),
                    );
                    let req = Request::from_parts(parts, body);
                    let target = targets[0];
                    let base = upstream::grpc::normalize(&target.url);
                    match Box::pin(upstream::grpc::forward(
                        &base,
                        req,
                        connect,
                        upstream_cfg.verify_tls,
                    ))
                    .await
                    {
                        Ok(r) => {
                            self.health_checker
                                .mark_success(upstream_name, &target.url, 0.0);
                            return r;
                        }
                        Err(e) => {
                            self.health_checker.mark_failure(
                                upstream_name,
                                &target.url,
                                target.max_fails,
                            );
                            warn!(target = %target.url, error = %e, "gRPC upstream failed");
                        }
                    }
                    let mut r = (StatusCode::OK, "").into_response();
                    r.headers_mut().insert(
                        header::CONTENT_TYPE,
                        HeaderValue::from_static("application/grpc"),
                    );
                    r.headers_mut()
                        .insert("grpc-status", HeaderValue::from_static("14"));
                    return r;
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
                    Box::pin(self.serve_with_cache(
                        compiled,
                        route,
                        upstream_name,
                        upstream_cfg,
                        targets,
                        is_tls,
                        client_ip,
                        parts,
                        body,
                        key,
                        cfg,
                    ))
                    .await
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

        #[cfg(feature = "image-filter")]
        let resp = match &route.image_filter {
            Some(cfg) => Box::pin(filters::image::apply(cfg, resp)).await,
            None => resp,
        };

        let resp = if route.ssi && filters::ssi::applies(&resp, &route.ssi_types) {
            let mut ctx = vars::Ctx {
                method: &ssi_method,
                uri: req_uri.clone(),
                headers: &ssi_headers,
                client_ip,
                is_tls,
                http: &compiled.config.http,
                set: std::collections::HashMap::new(),
                captures: Vec::new(),
                client_cert: None,
            };
            let dir = req_uri
                .path()
                .rsplit_once('/')
                .map(|(d, _)| d.to_string())
                .unwrap_or_default();
            Box::pin(filters::ssi::apply(
                resp,
                route.static_root.as_deref(),
                &dir,
                &mut ctx,
            ))
            .await
        } else {
            resp
        };

        let resp = if needs_gunzip {
            filters::gunzip::apply(resp)
        } else {
            resp
        };

        let resp = if route.charset.is_some() {
            Box::pin(filters::charset::apply(route, resp)).await
        } else {
            resp
        };

        let resp = match &route.sub_filter {
            Some(cfg) => Box::pin(filters::sub::apply(cfg, resp)).await,
            None => resp,
        };

        let resp = if has_addition && filters::sub::is_text(&resp, &route.addition_types) {
            let fetch = |uri: Option<String>| {
                let headers = addition_headers.clone();
                async move {
                    let uri = uri?;
                    Box::pin(self.subrequest(
                        compiled,
                        vhost_index,
                        vhost,
                        is_tls,
                        client_ip,
                        &uri,
                        headers,
                    ))
                    .await
                }
            };
            let before = fetch(route.add_before_body.clone()).await;
            let after = fetch(route.add_after_body.clone()).await;
            Box::pin(filters::sub::wrap_body(resp, before, after)).await
        } else {
            resp
        };

        let mut resp = filters::run(
            compiled,
            vhost_index,
            matched.route_index,
            route,
            info,
            resp,
        );

        if let Some(c) = userid_cookie {
            resp.headers_mut().append(header::SET_COOKIE, c);
        }

        if route.limit_rate > 0 {
            resp = filters::limit_rate::apply(resp, route.limit_rate, route.limit_rate_after);
        }

        if !route.access_log {
            resp.extensions_mut().insert(SkipAccessLog);
        }

        resp
    }
}

fn merged_slashes_uri(uri: &axum::http::Uri) -> Option<axum::http::Uri> {
    let mut path = String::with_capacity(uri.path().len());
    for c in uri.path().chars() {
        if !(c == '/' && path.ends_with('/')) {
            path.push(c);
        }
    }
    let pq = match uri.query() {
        Some(q) => format!("{path}?{q}"),
        None => path,
    };
    pq.parse().ok()
}

fn absolutize_redirect(
    mut resp: Response,
    http: &crate::config::HttpConfig,
    vhost: &VirtualHost,
    host_header: &HeaderValue,
    is_tls: bool,
) -> Response {
    if !resp.status().is_redirection()
        || !http.absolute_redirect
        || resp.headers().contains_key(X_FERRIX_UPSTREAM)
    {
        return resp;
    }
    let Some(loc) = resp
        .headers()
        .get(header::LOCATION)
        .and_then(|v| v.to_str().ok())
        .filter(|l| l.starts_with('/') && !l.starts_with("//"))
    else {
        return resp;
    };
    let host_raw = host_header.to_str().unwrap_or("");
    let (host_name, port) = if let Some(rest) = host_raw.strip_prefix('[') {
        match rest.split_once(']') {
            Some((_, after)) => (
                &host_raw[..host_raw.len() - after.len()],
                after.strip_prefix(':'),
            ),
            None => (host_raw, None),
        }
    } else {
        match host_raw.rsplit_once(':') {
            Some((h, p)) => (h, Some(p)),
            None => (host_raw, None),
        }
    };
    let name = if http.server_name_in_redirect {
        vhost
            .server_names
            .iter()
            .find(|n| !n.contains('*') && !n.starts_with('~') && n.as_str() != "_")
            .map(String::as_str)
            .unwrap_or(host_name)
    } else {
        host_name
    };
    if name.is_empty() {
        return resp;
    }
    let port = port.filter(|_| http.port_in_redirect);
    let abs = match port {
        Some(p) => format!("{}://{}:{}{}", scheme_str(is_tls), name, p, loc),
        None => format!("{}://{}{}", scheme_str(is_tls), name, loc),
    };
    if let Ok(v) = HeaderValue::from_str(&abs) {
        resp.headers_mut().insert(header::LOCATION, v);
    }
    resp
}

async fn buffer_request_body_var(req: Request, limit: u64) -> Request {
    let has_body = req
        .headers()
        .get(header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<u64>().ok())
        .is_some_and(|n| n > 0 && n <= limit)
        || req.headers().contains_key(header::TRANSFER_ENCODING);
    if !has_body {
        return req;
    }
    let (mut parts, body) = req.into_parts();
    use http_body_util::BodyExt;
    let mut collected = Vec::new();
    let mut body = body;
    let mut rest: Option<Body> = None;
    while let Some(frame) = body.frame().await {
        let Ok(frame) = frame else { break };
        if let Ok(data) = frame.into_data() {
            collected.extend_from_slice(&data);
            if collected.len() as u64 > limit {
                rest = Some(body);
                break;
            }
        }
    }
    let bytes = Bytes::from(collected);
    let new_body = match rest {
        Some(tail) => {
            let head = futures_util::stream::once(async move { Ok::<_, axum::Error>(bytes) });
            Body::from_stream(head.chain(tail.into_data_stream()))
        }
        None => {
            if let Ok(text) = std::str::from_utf8(&bytes) {
                let mut set = parts
                    .extensions
                    .get::<vars::SetVars>()
                    .map(|s| s.0.clone())
                    .unwrap_or_default();
                set.insert("request_body".into(), text.to_string());
                parts.extensions.insert(vars::SetVars(set));
            }
            Body::from(bytes)
        }
    };
    Request::from_parts(parts, new_body)
}

#[derive(Clone, Copy)]
struct Subrequest;

fn phase_response(code: u16, value: String) -> Response {
    let status = StatusCode::from_u16(code).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    if status.is_redirection() {
        let mut r = (status, "").into_response();
        if let Ok(v) = HeaderValue::from_str(&value) {
            r.headers_mut().insert(header::LOCATION, v);
        }
        r
    } else {
        (status, value).into_response()
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

    #[tokio::test]
    async fn json_set_reads_the_request_body() {
        use crate::config::{
            ConditionConfig, FerrixConfig, RouteRule, RouteTargetType, VariableDef,
        };
        let mut cfg = FerrixConfig::default();
        cfg.http.variables.insert(
            "user".into(),
            VariableDef::Json {
                source: "$request_body".into(),
                path: ".user.name".into(),
            },
        );
        cfg.vhosts[0].routes = vec![RouteRule {
            id: "r".into(),
            path_prefix: "/".into(),
            target_type: RouteTargetType::Return,
            redirect_code: Some(204),
            conditions: vec![ConditionConfig {
                condition: "$user".into(),
                return_code: Some(200),
                return_value: Some("hi $user".into()),
                ..Default::default()
            }],
            ..Default::default()
        }];
        let port = cfg.vhosts[0].listen_port;
        let engine = ProxyEngine::new(
            Arc::new(ConfigStore::new(cfg, "t.json")),
            crate::metrics::MetricsCollector::new(10),
            Arc::new(HealthChecker::new()),
        );
        let body = r#"{"user":{"name":"Ana"}}"#;
        let req = Request::builder()
            .method("POST")
            .uri("/")
            .header("host", "x")
            .header("content-length", body.len())
            .body(Body::from(body))
            .unwrap();
        let r = engine
            .handle_request(port, false, "127.0.0.1:1".parse().unwrap(), req)
            .await;
        assert_eq!(r.status(), 200);
        let b = axum::body::to_bytes(r.into_body(), 1024).await.unwrap();
        assert_eq!(&b[..], b"hi Ana");
    }

    #[tokio::test]
    async fn addition_wraps_html_with_subrequests() {
        use crate::config::{FerrixConfig, RouteRule, RouteTargetType};
        let dir = std::env::temp_dir().join(format!("ferrix-add-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("page.html"), "<p>body</p>").unwrap();
        std::fs::write(dir.join("head.html"), "<h1>top</h1>").unwrap();
        let mut cfg = FerrixConfig::default();
        cfg.vhosts[0].routes = vec![RouteRule {
            id: "s".into(),
            path_prefix: "/".into(),
            target_type: RouteTargetType::StaticFiles,
            static_root: Some(dir.to_string_lossy().into()),
            add_before_body: Some("/head.html".into()),
            add_after_body: Some("/missing.html".into()),
            ..Default::default()
        }];
        let port = cfg.vhosts[0].listen_port;
        let engine = ProxyEngine::new(
            Arc::new(ConfigStore::new(cfg, "t.json")),
            crate::metrics::MetricsCollector::new(10),
            Arc::new(HealthChecker::new()),
        );
        let req = Request::builder()
            .uri("/page.html")
            .header("host", "x")
            .body(Body::empty())
            .unwrap();
        let r = engine
            .handle_request(port, false, "127.0.0.1:1".parse().unwrap(), req)
            .await;
        let b = axum::body::to_bytes(r.into_body(), 1 << 16).await.unwrap();
        assert_eq!(&b[..], b"<h1>top</h1><p>body</p>");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn merges_slashes_keeping_the_query() {
        let u: axum::http::Uri = "//a///b/?x=//y".parse().unwrap();
        assert_eq!(merged_slashes_uri(&u).unwrap().to_string(), "/a/b/?x=//y");
    }

    #[test]
    fn own_relative_redirects_become_absolute() {
        let vhost = crate::config::FerrixConfig::default().vhosts[0].clone();
        let mut http = crate::config::HttpConfig::default();
        let redirect = |loc: &str| {
            let mut r = (StatusCode::MOVED_PERMANENTLY, "").into_response();
            r.headers_mut()
                .insert(header::LOCATION, HeaderValue::from_str(loc).unwrap());
            r
        };
        let host = HeaderValue::from_static("ex.com:8080");
        let loc = |r: Response| r.headers()[header::LOCATION].to_str().unwrap().to_string();
        assert_eq!(
            loc(absolutize_redirect(
                redirect("/d/"),
                &http,
                &vhost,
                &host,
                true
            )),
            "https://ex.com:8080/d/"
        );
        http.port_in_redirect = false;
        assert_eq!(
            loc(absolutize_redirect(
                redirect("/d/"),
                &http,
                &vhost,
                &host,
                false
            )),
            "http://ex.com/d/"
        );
        assert_eq!(
            loc(absolutize_redirect(
                redirect("https://x/"),
                &http,
                &vhost,
                &host,
                false
            )),
            "https://x/"
        );
        http.absolute_redirect = false;
        assert_eq!(
            loc(absolutize_redirect(
                redirect("/d/"),
                &http,
                &vhost,
                &host,
                false
            )),
            "/d/"
        );
        let v6 = HeaderValue::from_static("[::1]:81");
        http.absolute_redirect = true;
        http.port_in_redirect = true;
        assert_eq!(
            loc(absolutize_redirect(
                redirect("/d"),
                &http,
                &vhost,
                &v6,
                false
            )),
            "http://[::1]:81/d"
        );
    }

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
