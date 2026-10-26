pub mod compress;
pub mod cors;
pub mod h1;
pub mod hashring;
pub mod matcher;
pub mod middleware;
pub mod realip;
pub mod static_files;
pub mod upstream;
pub mod upstream_client;
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

use crate::cache::ResponseCache;
use crate::config::store::ConfigStore;
use crate::config::{
    CacheConfig, NextUpstreamCondition, RouteRule, RouteTargetType, UpstreamConfig, UpstreamTarget,
    VirtualHost,
};
use crate::health::HealthChecker;
use crate::metrics::logger::RequestLog;
use crate::metrics::MetricsCollector;
use matcher::CompiledConfig;
use middleware::{AccessControl, AccessDecision, ConnLimiter, RateLimitDecision, RateLimiter};
use realip::RealIpResolver;
use static_files::{StaticFileServer, StaticRequest};
use upstream::UpstreamPoolManager;
use upstream_client::{OutgoingBody, UpstreamClient, UpstreamError};
use websocket::WebSocketProxy;

#[derive(Debug, Clone, Copy)]
struct SkipAccessLog;

#[derive(Debug)]
enum RequestBody {
    Buffered(Bytes),

    Streaming(Body),
}

#[derive(Clone)]
pub struct ProxyEngine {
    pub store: Arc<ConfigStore>,
    pub metrics: Arc<MetricsCollector>,
    pub health_checker: Arc<HealthChecker>,
    pub pool_manager: Arc<UpstreamPoolManager>,
    pub rate_limiter: Arc<RateLimiter>,
    pub conn_limiter: Arc<ConnLimiter>,
    pub response_cache: Arc<ResponseCache>,
    pub acme_challenges: Arc<crate::acme::AcmeChallenges>,
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
            crate::clock::now()
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
            .strip_prefix(crate::acme::CHALLENGE_PREFIX)
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
                apply_error_page(vhost, resp).await
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
            timestamp: crate::clock::utc_now(),
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

        if let Some(cors_cfg) = &route.cors {
            if let Some(r) = cors::preflight(cors_cfg, &parts.method, &parts.headers) {
                return r;
            }
        }
        let origin = route
            .cors
            .as_ref()
            .and_then(|_| parts.headers.get(header::ORIGIN).cloned());

        let request_id = if compiled.config.http.request_id {
            let id = parts
                .headers
                .get(X_REQUEST_ID)
                .filter(|v| !v.is_empty() && v.len() <= 200)
                .cloned()
                .unwrap_or_else(new_request_id);
            parts.headers.insert(X_REQUEST_ID, id.clone());
            Some(id)
        } else {
            None
        };

        let compression = &compiled.config.http.compression;
        let encoding =
            if route.compression.unwrap_or(compression.enabled) && parts.method != Method::HEAD {
                compress::negotiate(&parts.headers, compression)
            } else {
                None
            };

        let mut resp = match route.target_type {
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
                    encoding: encoding.map(|a| (a, compression)),
                };
                match StaticFileServer::serve_file(
                    root_dir,
                    file_path,
                    route.try_files.as_deref(),
                    &static_req,
                )
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
                        WebSocketUpgrade::from_request_parts(&mut parts, &()).await
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
                        if let Some(v) = crate::cache::header_value("HIT") {
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
                        self.store_if_cacheable(key, cfg, inner).await
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

        if let Some(algo) = encoding {
            if compress::should_compress(&resp, compression) {
                resp = compress::compress(resp, algo, compression.level);
            }
        }

        for (name, value) in compiled.route_response_headers(vhost_index, matched.route_index) {
            resp.headers_mut().insert(name.clone(), value.clone());
        }

        if let Some(cors_cfg) = &route.cors {
            cors::decorate(cors_cfg, origin.as_ref(), &mut resp);
        }
        if let Some(id) = request_id {
            resp.headers_mut().insert(X_REQUEST_ID, id);
        }

        if !route.access_log {
            resp.extensions_mut().insert(SkipAccessLog);
        }

        resp
    }

    #[allow(clippy::too_many_arguments)]
    async fn proxy_http_request(
        &self,
        compiled: &CompiledConfig,
        route: &RouteRule,
        upstream_name: &str,
        upstream_cfg: &UpstreamConfig,
        targets: Vec<&UpstreamTarget>,
        is_tls: bool,
        client_ip: IpAddr,
        req: Request,
    ) -> Response {
        let http = &compiled.config.http;
        let (mut parts, body) = req.into_parts();
        let method = &parts.method;
        let path_and_query = parts
            .uri
            .path_and_query()
            .map(|pq| pq.as_str())
            .unwrap_or("/");

        let target_path: std::borrow::Cow<str> = if route.strip_prefix {
            let stripped = path_and_query
                .strip_prefix(&route.path_prefix)
                .unwrap_or(path_and_query);
            if stripped.starts_with('/') {
                stripped.into()
            } else {
                format!("/{}", stripped).into()
            }
        } else {
            path_and_query.into()
        };

        if let Some(declared) = parts
            .headers
            .get(header::CONTENT_LENGTH)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse::<u64>().ok())
        {
            if declared > http.client_max_body_size {
                return payload_too_large(http.client_max_body_size);
            }
        }

        let request_body = match read_request_body(
            body,
            http.proxy_request_buffer_size,
            http.client_max_body_size,
        )
        .await
        {
            Ok(b) => b,
            Err(status) => {
                if status == StatusCode::PAYLOAD_TOO_LARGE {
                    return payload_too_large(http.client_max_body_size);
                }
                return (status, "400 Bad Request").into_response();
            }
        };

        let next_upstream = route.next_upstream();

        let can_retry = matches!(request_body, RequestBody::Buffered(_))
            && !next_upstream.conditions.is_empty();

        let max_tries = if can_retry {
            next_upstream
                .tries
                .map(|t| t.max(1) as usize)
                .unwrap_or(targets.len())
                .min(targets.len())
        } else {
            1
        };

        let client = UpstreamClient::new(
            Duration::from_millis(
                route
                    .proxy_connect_timeout_ms
                    .unwrap_or(http.proxy_connect_timeout_ms),
            ),
            Duration::from_millis(
                route
                    .proxy_read_timeout_ms
                    .unwrap_or(http.proxy_read_timeout_ms),
            ),
            upstream_cfg.verify_tls,
        );

        let attempt_deadline = next_upstream
            .timeout_ms
            .map(|ms| crate::clock::now() + Duration::from_millis(ms));

        let (buffered_body, mut streaming_body) = match request_body {
            RequestBody::Buffered(b) => (Some(b), None),
            RequestBody::Streaming(b) => (None, Some(b)),
        };

        prepare_forwarded_headers(&mut parts.headers, route, client_ip, is_tls);
        let headers = &parts.headers;
        let track_conns = upstream_cfg.algorithm == crate::config::LoadBalanceAlgorithm::LeastConn;

        let mut last_error: Option<String> = None;

        for (attempt, target) in targets.iter().enumerate() {
            if attempt >= max_tries {
                break;
            }
            if let Some(deadline) = attempt_deadline {
                if crate::clock::now() >= deadline {
                    last_error = Some("tempo de failover esgotado".to_string());
                    break;
                }
            }

            // Connection counts only matter to least_conn and max_conns; skip the shared
            // counters otherwise.
            let counted = track_conns || target.max_conns.is_some();
            if counted {
                self.pool_manager.inc_conn(&target.url);
            }
            let start_ms = crate::clock::mono_ms();

            let outgoing = match (&buffered_body, streaming_body.take()) {
                (Some(bytes), _) if !bytes.is_empty() => OutgoingBody::Buffered(bytes.clone()),
                (_, Some(body)) => OutgoingBody::Streaming(body),
                _ => OutgoingBody::Empty,
            };

            let result = client
                .send(method, &target.url, &target_path, headers, outgoing)
                .await;
            if counted {
                self.pool_manager.dec_conn(&target.url);
            }
            let elapsed_ms = crate::clock::mono_ms().saturating_sub(start_ms) as f64;

            match result {
                Ok(upstream_res) => {
                    let status = upstream_res.status();

                    if should_retry_status(&next_upstream.conditions, status.as_u16())
                        && attempt + 1 < max_tries
                    {
                        self.health_checker.mark_failure(
                            upstream_name,
                            &target.url,
                            target.max_fails,
                        );
                        last_error = Some(format!("upstream respondeu {}", status));
                        continue;
                    }

                    self.health_checker
                        .mark_success(upstream_name, &target.url, elapsed_ms);

                    let target_index = upstream_cfg
                        .targets
                        .iter()
                        .position(|t| std::ptr::eq(t, *target))
                        .unwrap_or(0);
                    return build_upstream_response(
                        upstream_res,
                        compiled.target_header(upstream_name, target_index),
                    );
                }
                Err(err) => {
                    let retryable = should_retry_error(&next_upstream.conditions, &err);
                    warn!(target = %target.url, error = %err, retryable, "failed to communicate with upstream");
                    self.health_checker
                        .mark_failure(upstream_name, &target.url, target.max_fails);
                    last_error = Some(err.to_string());

                    if !retryable {
                        break;
                    }
                }
            }
        }

        let err_msg = last_error.unwrap_or_else(|| "all upstream targets failed".to_string());
        Response::builder()
            .status(StatusCode::BAD_GATEWAY)
            .header(header::CONTENT_TYPE, "text/plain")
            .body(Body::from(format!(
                "502 Bad Gateway - Ferrix Proxy Error: {}",
                err_msg
            )))
            .unwrap()
    }

    async fn store_if_cacheable(&self, key: String, cfg: &CacheConfig, resp: Response) -> Response {
        let status = resp.status();
        let cacheable = status == StatusCode::OK
            && !crate::cache::has_set_cookie(resp.headers())
            && !crate::cache::upstream_forbids_cache(resp.headers())
            && resp
                .headers()
                .get(header::CONTENT_LENGTH)
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.parse::<u64>().ok())
                .is_some_and(|len| len <= crate::cache::MAX_CACHEABLE_BODY_BYTES);

        if !cacheable {
            let mut r = resp;
            if let Some(v) = crate::cache::header_value("MISS") {
                r.headers_mut().insert("x-cache-status", v);
            }
            return r;
        }

        let headers = crate::cache::cacheable_headers(resp.headers());
        let (parts, body) = resp.into_parts();
        match axum::body::to_bytes(body, crate::cache::MAX_CACHEABLE_BODY_BYTES as usize).await {
            Ok(bytes) => {
                self.response_cache.put(
                    key,
                    parts.status.as_u16(),
                    headers,
                    bytes.clone(),
                    Duration::from_secs(cfg.valid_secs),
                );
                let mut r = Response::from_parts(parts, Body::from(bytes));
                if let Some(v) = crate::cache::header_value("MISS") {
                    r.headers_mut().insert("x-cache-status", v);
                }
                r
            }
            Err(_) => (
                StatusCode::BAD_GATEWAY,
                "502 Bad Gateway - Ferrix Proxy Error: failed to read response body from upstream",
            )
                .into_response(),
        }
    }
}

fn cache_bypass_matches(cfg: &CacheConfig, parts: &axum::http::request::Parts) -> bool {
    for name in &cfg.bypass_headers {
        if let Some(v) = parts.headers.get(name.as_str()) {
            if !v.is_empty() {
                return true;
            }
        }
    }

    if !cfg.bypass_cookies.is_empty() {
        if let Some(cookie_header) = parts
            .headers
            .get(header::COOKIE)
            .and_then(|v| v.to_str().ok())
        {
            for name in &cfg.bypass_cookies {
                let hit = cookie_header
                    .split(';')
                    .filter_map(|kv| kv.trim().split_once('='))
                    .any(|(k, _)| k == name);
                if hit {
                    return true;
                }
            }
        }
    }

    if !cfg.bypass_query_params.is_empty() {
        if let Some(query) = parts.uri.query() {
            for name in &cfg.bypass_query_params {
                let hit = query
                    .split('&')
                    .filter_map(|kv| kv.split_once('='))
                    .any(|(k, _)| k == name);
                if hit {
                    return true;
                }
            }
        }
    }

    false
}

fn cache_key_for(vhost: &VirtualHost, parts: &axum::http::request::Parts) -> String {
    let host = parts
        .headers
        .get(header::HOST)
        .and_then(|v| v.to_str().ok())
        .unwrap_or(&vhost.name);
    let path_and_query = parts
        .uri
        .path_and_query()
        .map(|pq| pq.as_str())
        .unwrap_or("/");
    crate::cache::cache_key(parts.method.as_str(), host, path_and_query)
}

fn build_cached_response(cached: &crate::cache::CachedResponse) -> Response {
    let mut builder = Response::builder().status(cached.status);
    for (k, v) in &cached.headers {
        builder = builder.header(k, v);
    }
    builder
        .body(Body::from(cached.body.clone()))
        .unwrap_or_else(|_| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                "500 Internal Server Error",
            )
                .into_response()
        })
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

/// Swaps the body of an error response for the vhost's custom page (`error_pages`),
/// keeping the status. Responses that came from an upstream are passed through, like
/// nginx without `proxy_intercept_errors`.
async fn apply_error_page(vhost: &VirtualHost, resp: Response) -> Response {
    let status = resp.status();
    if status.as_u16() < 400
        || vhost.error_pages.is_empty()
        || resp.headers().contains_key(X_FERRIX_UPSTREAM)
    {
        return resp;
    }
    let code = status.as_u16().to_string();
    let class = format!("{}xx", status.as_u16() / 100);
    let Some(path) = vhost
        .error_pages
        .get(&code)
        .or_else(|| vhost.error_pages.get(&class))
    else {
        return resp;
    };
    let page = match tokio::fs::read(path).await {
        Ok(p) => p,
        Err(e) => {
            warn!(path = %path, error = %e, "custom error page not readable");
            return resp;
        }
    };
    let (mut parts, _) = resp.into_parts();
    parts.headers.remove(header::CONTENT_ENCODING);
    parts.headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/html; charset=utf-8"),
    );
    parts
        .headers
        .insert(header::CONTENT_LENGTH, HeaderValue::from(page.len()));
    Response::from_parts(parts, Body::from(page))
}

const X_REQUEST_ID: HeaderName = HeaderName::from_static("x-request-id");

fn new_request_id() -> HeaderValue {
    let id = uuid::Uuid::new_v4();
    HeaderValue::from_str(id.simple().encode_lower(&mut uuid::Uuid::encode_buffer()))
        .unwrap_or_else(|_| HeaderValue::from_static("0"))
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

const HOP_BY_HOP: [HeaderName; 9] = [
    header::CONNECTION,
    HeaderName::from_static("keep-alive"),
    header::PROXY_AUTHENTICATE,
    header::PROXY_AUTHORIZATION,
    header::TE,
    header::TRAILER,
    HeaderName::from_static("trailers"),
    header::TRANSFER_ENCODING,
    header::UPGRADE,
];

fn strip_hop_by_hop(headers: &mut HeaderMap) {
    // Usually none are present; one pass over the (few) names beats nine hashed removals.
    if !headers.keys().any(|k| HOP_BY_HOP.contains(k)) {
        return;
    }
    for name in HOP_BY_HOP {
        headers.remove(name);
    }
}

const X_REAL_IP: HeaderName = HeaderName::from_static("x-real-ip");
const X_FORWARDED_FOR: HeaderName = HeaderName::from_static("x-forwarded-for");
const X_FORWARDED_PROTO: HeaderName = HeaderName::from_static("x-forwarded-proto");
pub const X_FERRIX_UPSTREAM: HeaderName = HeaderName::from_static("x-ferrix-upstream");

fn build_upstream_response(
    upstream_res: Response,
    upstream_header: Option<&HeaderValue>,
) -> Response {
    let (mut parts, body) = upstream_res.into_parts();
    strip_hop_by_hop(&mut parts.headers);
    if let Some(v) = upstream_header {
        parts.headers.insert(X_FERRIX_UPSTREAM, v.clone());
    }
    Response::from_parts(parts, body)
}

/// Turns the client's request headers into the ones sent upstream, in place (the
/// request is consumed by the proxy anyway, so there's no need to copy the map).
fn prepare_forwarded_headers(
    headers: &mut HeaderMap,
    route: &RouteRule,
    client_ip: IpAddr,
    is_tls: bool,
) {
    let scheme = scheme_str(is_tls);
    let host = headers.remove(header::HOST);
    strip_hop_by_hop(headers);

    let ip = client_ip.to_string();
    let xff = RealIpResolver::proxy_add_x_forwarded_for(headers, &ip);
    let ip_value = HeaderValue::from_str(&ip).ok();
    let xff_value = HeaderValue::from_str(&xff).ok();

    if let Some(v) = &ip_value {
        headers.insert(X_REAL_IP, v.clone());
    }
    if let Some(v) = &xff_value {
        headers.insert(X_FORWARDED_FOR, v.clone());
    }
    headers.insert(X_FORWARDED_PROTO, HeaderValue::from_static(scheme));

    for (k, v) in &route.proxy_set_headers {
        let Ok(name) = HeaderName::from_bytes(k.as_bytes()) else {
            continue;
        };
        let value = match v.as_str() {
            "$host" => host.clone(),
            "$remote_addr" => ip_value.clone(),
            "$proxy_add_x_forwarded_for" => xff_value.clone(),
            "$scheme" => Some(HeaderValue::from_static(scheme)),
            other => HeaderValue::from_str(other).ok(),
        };
        if let Some(value) = value {
            headers.insert(name, value);
        }
    }
}

fn scheme_str(is_tls: bool) -> &'static str {
    if is_tls {
        "https"
    } else {
        "http"
    }
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

async fn read_request_body(
    body: Body,
    buffer_limit: u64,
    max_size: u64,
) -> Result<RequestBody, StatusCode> {
    let mut stream = body.into_data_stream();
    let mut buffered: Vec<Bytes> = Vec::new();
    let mut total: u64 = 0;

    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|_| StatusCode::BAD_REQUEST)?;
        total += chunk.len() as u64;

        if total > max_size {
            return Err(StatusCode::PAYLOAD_TOO_LARGE);
        }

        buffered.push(chunk);

        if total > buffer_limit {
            let prefix =
                futures_util::stream::iter(buffered.into_iter().map(Ok::<Bytes, std::io::Error>));
            let rest = stream.map(|r| r.map_err(std::io::Error::other));
            let joined = prefix.chain(rest);
            return Ok(RequestBody::Streaming(Body::from_stream(joined)));
        }
    }

    let mut flat = Vec::with_capacity(total as usize);
    for chunk in buffered {
        flat.extend_from_slice(&chunk);
    }
    Ok(RequestBody::Buffered(Bytes::from(flat)))
}

fn should_retry_status(conditions: &[NextUpstreamCondition], status: u16) -> bool {
    conditions.iter().any(|c| match c {
        NextUpstreamCondition::Http500 => status == 500,
        NextUpstreamCondition::Http502 => status == 502,
        NextUpstreamCondition::Http503 => status == 503,
        NextUpstreamCondition::Http504 => status == 504,
        NextUpstreamCondition::Http403 => status == 403,
        NextUpstreamCondition::Http404 => status == 404,
        _ => false,
    })
}

fn should_retry_error(conditions: &[NextUpstreamCondition], err: &UpstreamError) -> bool {
    if err.is_timeout() {
        return conditions.contains(&NextUpstreamCondition::Timeout)
            || conditions.contains(&NextUpstreamCondition::Error);
    }
    conditions.contains(&NextUpstreamCondition::Error)
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

fn payload_too_large(limit: u64) -> Response {
    (
        StatusCode::PAYLOAD_TOO_LARGE,
        format!(
            "413 Payload Too Large - o corpo excede client_max_body_size ({} bytes)",
            limit
        ),
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ProxyNextUpstreamConfig;

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
    fn hop_by_hop_headers_are_stripped() {
        let mut h = HeaderMap::new();
        h.insert("Connection", HeaderValue::from_static("keep-alive"));
        h.insert("transfer-encoding", HeaderValue::from_static("chunked"));
        h.insert("UPGRADE", HeaderValue::from_static("websocket"));
        h.insert("content-type", HeaderValue::from_static("text/plain"));
        h.insert("x-real-ip", HeaderValue::from_static("1.2.3.4"));
        strip_hop_by_hop(&mut h);
        assert!(!h.contains_key("connection"));
        assert!(!h.contains_key("transfer-encoding"));
        assert!(!h.contains_key("upgrade"));
        assert!(h.contains_key("content-type"));
        assert!(h.contains_key("x-real-ip"));
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

    #[test]
    fn default_conditions_retry_502_and_504_but_not_500() {
        let c = ProxyNextUpstreamConfig::default().conditions;
        assert!(should_retry_status(&c, 502));
        assert!(should_retry_status(&c, 504));
        assert!(!should_retry_status(&c, 500));
        assert!(!should_retry_status(&c, 200));
    }

    #[test]
    fn empty_conditions_never_retry() {
        assert!(!should_retry_status(&[], 502));
    }

    #[test]
    fn explicit_conditions_are_honoured() {
        let c = vec![
            NextUpstreamCondition::Http500,
            NextUpstreamCondition::Http503,
        ];
        assert!(should_retry_status(&c, 500));
        assert!(should_retry_status(&c, 503));
        assert!(!should_retry_status(&c, 502));
    }

    #[tokio::test]
    async fn small_body_is_buffered_and_therefore_retryable() {
        let body = Body::from("payload pequeno");
        match read_request_body(body, 1024, 1_000_000).await.unwrap() {
            RequestBody::Buffered(b) => assert_eq!(&b[..], b"payload pequeno"),
            RequestBody::Streaming(_) => panic!("corpo pequeno deveria ser bufferizado"),
        }
    }

    #[tokio::test]
    async fn empty_body_is_buffered_as_empty() {
        match read_request_body(Body::empty(), 1024, 1_000_000)
            .await
            .unwrap()
        {
            RequestBody::Buffered(b) => assert!(b.is_empty()),
            RequestBody::Streaming(_) => panic!("corpo vazio deveria ser bufferizado"),
        }
    }

    #[tokio::test]
    async fn body_above_the_buffer_limit_switches_to_streaming() {
        let big = vec![b'x'; 5000];
        let body = Body::from(big);
        match read_request_body(body, 1024, 1_000_000).await.unwrap() {
            RequestBody::Streaming(_) => {}
            RequestBody::Buffered(_) => panic!("corpo grande deveria ir em streaming"),
        }
    }

    #[tokio::test]
    async fn body_above_the_hard_limit_is_rejected() {
        let big = vec![b'x'; 5000];
        let body = Body::from(big);
        let err = read_request_body(body, 1024, 2000).await.unwrap_err();
        assert_eq!(err, StatusCode::PAYLOAD_TOO_LARGE);
    }

    #[tokio::test]
    async fn streamed_body_preserves_every_byte() {
        let original: Vec<u8> = (0..5000u32).map(|i| (i % 251) as u8).collect();
        let body = Body::from(original.clone());

        let RequestBody::Streaming(streamed) =
            read_request_body(body, 1024, 1_000_000).await.unwrap()
        else {
            panic!("esperava streaming");
        };

        let collected = axum::body::to_bytes(streamed, usize::MAX).await.unwrap();
        assert_eq!(collected.len(), original.len());
        assert_eq!(&collected[..], &original[..]);
    }

    fn parts_full(uri: &str, headers: &[(&str, &str)]) -> axum::http::request::Parts {
        let mut builder = Request::builder().uri(uri);
        for (k, v) in headers {
            builder = builder.header(*k, *v);
        }
        builder.body(Body::empty()).unwrap().into_parts().0
    }

    #[test]
    fn no_bypass_configured_never_bypasses() {
        let cfg = crate::config::CacheConfig::default();
        let parts = parts_full("/x", &[("cookie", "a=1"), ("upgrade", "websocket")]);
        assert!(!cache_bypass_matches(&cfg, &parts));
    }

    #[test]
    fn a_non_empty_bypass_header_skips_the_cache() {
        let cfg = crate::config::CacheConfig {
            bypass_headers: vec!["Upgrade".to_string()],
            ..Default::default()
        };
        let with = parts_full("/x", &[("upgrade", "websocket")]);
        assert!(cache_bypass_matches(&cfg, &with));

        let without = parts_full("/x", &[]);
        assert!(!cache_bypass_matches(&cfg, &without));
    }

    #[test]
    fn a_matching_bypass_cookie_skips_the_cache() {
        let cfg = crate::config::CacheConfig {
            bypass_cookies: vec!["session".to_string()],
            ..Default::default()
        };
        let with = parts_full("/x", &[("cookie", "theme=dark; session=abc123")]);
        assert!(cache_bypass_matches(&cfg, &with));

        let without = parts_full("/x", &[("cookie", "theme=dark")]);
        assert!(!cache_bypass_matches(&cfg, &without));
    }

    #[test]
    fn a_matching_bypass_query_param_skips_the_cache() {
        let cfg = crate::config::CacheConfig {
            bypass_query_params: vec!["nocache".to_string()],
            ..Default::default()
        };
        let with = parts_full("/x?nocache=1", &[]);
        assert!(cache_bypass_matches(&cfg, &with));

        let without = parts_full("/x?other=1", &[]);
        assert!(!cache_bypass_matches(&cfg, &without));
    }

    #[test]
    fn cache_key_distinguishes_method_host_and_path() {
        let vhost = VirtualHost {
            id: "v".into(),
            name: "fallback".into(),
            server_names: vec!["*".into()],
            listen_port: 80,
            headers: std::collections::HashMap::new(),
            rate_limit: None,
            limit_conn: None,
            allowed_ips: Vec::new(),
            denied_ips: Vec::new(),
            basic_auth: None,
            satisfy: crate::config::Satisfy::All,
            tls: None,
            routes: Vec::new(),
            error_pages: Default::default(),
        };
        let get_x = parts_full("/x", &[("host", "a.com")]);
        let head_x = Request::builder()
            .uri("/x")
            .method("HEAD")
            .header("host", "a.com")
            .body(Body::empty())
            .unwrap()
            .into_parts()
            .0;
        let get_y = parts_full("/y", &[("host", "a.com")]);
        let get_x_other_host = parts_full("/x", &[("host", "b.com")]);

        assert_ne!(
            cache_key_for(&vhost, &get_x),
            cache_key_for(&vhost, &head_x)
        );
        assert_ne!(cache_key_for(&vhost, &get_x), cache_key_for(&vhost, &get_y));
        assert_ne!(
            cache_key_for(&vhost, &get_x),
            cache_key_for(&vhost, &get_x_other_host)
        );
    }

    #[tokio::test]
    async fn store_if_cacheable_tags_miss_and_skips_storage_without_content_length() {
        let store = ConfigStore::new(crate::config::FerrixConfig::default(), "test.json");
        let metrics = MetricsCollector::new(10);
        let health = Arc::new(HealthChecker::new());
        let engine = ProxyEngine::new(Arc::new(store), metrics, health);

        let resp = Response::builder()
            .status(200)
            .body(Body::from_stream(futures_util::stream::iter([Ok::<
                _,
                std::io::Error,
            >(
                Bytes::from_static(b"chunk"),
            )])))
            .unwrap();
        let cfg = crate::config::CacheConfig::default();

        let out = engine.store_if_cacheable("k".to_string(), &cfg, resp).await;
        assert_eq!(out.headers().get("x-cache-status").unwrap(), "MISS");
        assert!(engine.response_cache.get("k").is_none());
    }

    #[tokio::test]
    async fn store_if_cacheable_stores_and_replays_a_small_response_with_content_length() {
        let store = ConfigStore::new(crate::config::FerrixConfig::default(), "test.json");
        let metrics = MetricsCollector::new(10);
        let health = Arc::new(HealthChecker::new());
        let engine = ProxyEngine::new(Arc::new(store), metrics, health);

        let body_bytes = b"hello cache".to_vec();
        let resp = Response::builder()
            .status(200)
            .header(header::CONTENT_LENGTH, body_bytes.len())
            .body(Body::from(body_bytes.clone()))
            .unwrap();
        let cfg = crate::config::CacheConfig::default();

        let out = engine
            .store_if_cacheable("k2".to_string(), &cfg, resp)
            .await;
        assert_eq!(out.headers().get("x-cache-status").unwrap(), "MISS");

        let cached = engine
            .response_cache
            .get("k2")
            .expect("deveria ter guardado");
        assert_eq!(&cached.body[..], &body_bytes[..]);
        let replayed = build_cached_response(&cached);
        assert_eq!(replayed.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn store_if_cacheable_never_stores_a_response_with_set_cookie() {
        let store = ConfigStore::new(crate::config::FerrixConfig::default(), "test.json");
        let metrics = MetricsCollector::new(10);
        let health = Arc::new(HealthChecker::new());
        let engine = ProxyEngine::new(Arc::new(store), metrics, health);

        let body_bytes = b"secret".to_vec();
        let resp = Response::builder()
            .status(200)
            .header(header::CONTENT_LENGTH, body_bytes.len())
            .header(header::SET_COOKIE, "sess=abc")
            .body(Body::from(body_bytes))
            .unwrap();
        let cfg = crate::config::CacheConfig::default();

        engine
            .store_if_cacheable("k3".to_string(), &cfg, resp)
            .await;
        assert!(engine.response_cache.get("k3").is_none());
    }
}
