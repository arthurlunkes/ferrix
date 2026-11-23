pub mod handlers;
pub mod sse;

use axum::body::Body;
use axum::http::{header, HeaderMap, HeaderValue, Method, Request, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use http_body_util::BodyExt;
use serde::de::DeserializeOwned;
use serde::Serialize;
use std::net::SocketAddr;
use tokio::net::TcpListener;
use tracing::{debug, error, info, warn};

use crate::config::{AdminAuthMode, FerrixConfig};
use crate::http::access::AccessControl;
use handlers::AdminState;

const MAX_BODY_BYTES: usize = 16 * 1024 * 1024;
const DASHBOARD_COOKIE: &str = "ferrix_admin";
const DASHBOARD_COOKIE_MAX_AGE: u32 = 86_400;

pub struct Json<T>(pub T);

impl<T: Serialize> IntoResponse for Json<T> {
    fn into_response(self) -> Response {
        match serde_json::to_vec(&self.0) {
            Ok(body) => (
                [(
                    header::CONTENT_TYPE,
                    HeaderValue::from_static("application/json"),
                )],
                body,
            )
                .into_response(),
            Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
        }
    }
}

pub async fn serve(listen: String, state: AdminState) {
    let listener = match TcpListener::bind(&listen).await {
        Ok(l) => l,
        Err(e) => return error!("Failed to bind Admin listener to {}: {}", listen, e),
    };
    info!("Control Plane API listening on http://{}", listen);
    loop {
        let (tcp, addr) = match listener.accept().await {
            Ok(c) => c,
            Err(e) => {
                warn!(error = %e, "admin accept failed");
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                continue;
            }
        };
        let state = state.clone();
        tokio::spawn(async move {
            let service = hyper::service::service_fn(move |req: Request<hyper::body::Incoming>| {
                let state = state.clone();
                async move {
                    Ok::<_, std::convert::Infallible>(
                        handle(&state, req.map(Body::new), addr).await,
                    )
                }
            });
            if let Err(e) = hyper::server::conn::http1::Builder::new()
                .serve_connection(hyper_util::rt::TokioIo::new(tcp), service)
                .await
            {
                debug!(%addr, error = %e, "admin connection closed with error");
            }
        });
    }
}

pub async fn handle(state: &AdminState, req: Request<Body>, _peer: SocketAddr) -> Response {
    let cfg = state.store.config();
    let origin = req.headers().get(header::ORIGIN).cloned();

    if req.method() == Method::OPTIONS {
        let mut resp = StatusCode::NO_CONTENT.into_response();
        cors(&cfg.admin.cors_origins, origin.as_ref(), &mut resp, true);
        return resp;
    }

    let path = req.uri().path().to_string();
    let mut resp = if let Some(api_path) = path.strip_prefix("/api") {
        if authorized(state, req.headers(), req.uri()) {
            api(state, req, api_path).await
        } else {
            unauthorized(cfg.admin.auth.mode)
        }
    } else if path == "/metrics" {
        if cfg.admin.metrics_public || authorized(state, req.headers(), req.uri()) {
            prometheus_metrics(state)
        } else {
            unauthorized(cfg.admin.auth.mode)
        }
    } else if let Some(dir) = cfg.admin.dashboard_dir.as_deref() {
        if !dashboard_authorized(&cfg, req.headers(), req.uri()) {
            unauthorized(cfg.admin.auth.mode)
        } else {
            let bootstrap = dashboard_bootstrap_token(&cfg, req.uri());
            let (parts, _) = req.into_parts();
            let mut resp = dashboard(dir, &parts).await;
            if let Some(token) = bootstrap {
                set_dashboard_cookie(&mut resp, &token);
            }
            protect_dashboard_response(&mut resp, cfg.admin.auth.mode);
            resp
        }
    } else if path == "/" {
        Json(serde_json::json!({
            "service": "Ferrix Core Control Plane",
            "version": env!("CARGO_PKG_VERSION"),
            "status": "online"
        }))
        .into_response()
    } else {
        StatusCode::NOT_FOUND.into_response()
    };
    cors(&cfg.admin.cors_origins, origin.as_ref(), &mut resp, false);
    resp
}

async fn api(state: &AdminState, req: Request<Body>, path: &str) -> Response {
    use handlers as h;
    let method = req.method().clone();
    let query = req.uri().query().unwrap_or("").to_string();
    let segments: Vec<&str> = path.trim_matches('/').split('/').collect();

    match (method, segments.as_slice()) {
        (Method::GET, ["status"]) => h::get_status(state).await.into_response(),
        (Method::GET, ["metrics"]) => h::get_metrics(state).await.into_response(),
        (Method::GET, ["logs"]) => h::get_logs(state, logs_query(&query)).await.into_response(),
        (Method::GET, ["logs", "stream"]) => sse::stream_logs(state.metrics.clone()),
        (Method::GET, ["config"]) => h::get_config(state).await.into_response(),
        (Method::POST, ["config"]) => match json_body(req).await {
            Ok(cfg) => h::update_config(state, cfg).await.into_response(),
            Err(r) => *r,
        },
        (Method::GET, ["config", "yaml"]) => h::get_config_yaml(state).await.into_response(),
        (Method::POST, ["config", "yaml"]) => match text_body(req).await {
            Ok(text) => h::update_config_yaml(state, text).await.into_response(),
            Err(r) => *r,
        },
        (Method::POST, ["config", "validate"]) => match json_body(req).await {
            Ok(cfg) => h::validate_config(cfg).await.into_response(),
            Err(r) => *r,
        },
        (Method::POST, ["config", "yaml", "validate"]) => match text_body(req).await {
            Ok(text) => h::validate_config_yaml(text).await,
            Err(r) => *r,
        },
        (Method::POST, ["route-test"]) => match json_body(req).await {
            Ok(body) => h::route_test(state, body).await.into_response(),
            Err(r) => *r,
        },
        #[cfg(feature = "nginx-import")]
        (Method::POST, ["import", "nginx"]) => match json_body(req).await {
            Ok(body) => h::import_nginx(body).await.into_response(),
            Err(r) => *r,
        },
        #[cfg(not(feature = "nginx-import"))]
        (Method::POST, ["import", "nginx"]) => (
            StatusCode::NOT_FOUND,
            "this Ferrix was built without the nginx-import feature",
        )
            .into_response(),
        (Method::GET, ["vhosts"]) => h::get_vhosts(state).await.into_response(),
        (Method::POST, ["vhosts"]) => match json_body(req).await {
            Ok(vhost) => h::save_vhost(state, vhost).await.into_response(),
            Err(r) => *r,
        },
        (Method::DELETE, ["vhosts", id]) => {
            h::delete_vhost(state, decode(id)).await.into_response()
        }
        (Method::GET, ["upstreams"]) => h::get_upstreams(state).await.into_response(),
        (Method::POST, ["upstreams"]) => match json_body(req).await {
            Ok(up) => h::save_upstream(state, up).await.into_response(),
            Err(r) => *r,
        },
        (Method::DELETE, ["upstreams", name]) => h::delete_upstream(state, decode(name))
            .await
            .into_response(),
        (
            _,
            ["status" | "metrics" | "logs" | "config" | "route-test" | "vhosts" | "upstreams", ..],
        )
        | (_, ["import", "nginx"]) => StatusCode::METHOD_NOT_ALLOWED.into_response(),
        _ => StatusCode::NOT_FOUND.into_response(),
    }
}

fn decode(segment: &str) -> String {
    url::form_urlencoded::parse(format!("x={}", segment).as_bytes())
        .next()
        .map(|(_, v)| v.into_owned())
        .unwrap_or_else(|| segment.to_string())
}

fn logs_query(query: &str) -> handlers::LogsQuery {
    let mut q = handlers::LogsQuery {
        limit: None,
        status: None,
        q: None,
    };
    for (k, v) in url::form_urlencoded::parse(query.as_bytes()) {
        match k.as_ref() {
            "limit" => q.limit = v.parse().ok(),
            "status" => q.status = v.parse().ok(),
            "q" => q.q = Some(v.into_owned()),
            _ => {}
        }
    }
    q
}

async fn read_body(req: Request<Body>) -> Result<axum::body::Bytes, Box<Response>> {
    let limited = http_body_util::Limited::new(req.into_body(), MAX_BODY_BYTES);
    match limited.collect().await {
        Ok(collected) => Ok(collected.to_bytes()),
        Err(_) => Err(Box::new(
            (StatusCode::PAYLOAD_TOO_LARGE, "request body too large").into_response(),
        )),
    }
}

async fn json_body<T: DeserializeOwned>(req: Request<Body>) -> Result<T, Box<Response>> {
    let bytes = read_body(req).await?;
    serde_json::from_slice(&bytes).map_err(|e| {
        Box::new(
            (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "status": "invalid", "message": format!("Invalid JSON: {}", e) })),
        )
            .into_response(),
        )
    })
}

async fn text_body(req: Request<Body>) -> Result<String, Box<Response>> {
    let bytes = read_body(req).await?;
    String::from_utf8(bytes.to_vec())
        .map_err(|_| Box::new((StatusCode::BAD_REQUEST, "body must be UTF-8 text").into_response()))
}

async fn dashboard(dir: &str, req: &axum::http::request::Parts) -> Response {
    let cache = crate::config::StaticCacheConfig::default();
    let static_req = crate::http::static_files::StaticRequest {
        headers: &req.headers,
        encoding: None,
        cache: &cache,
        types: Default::default(),
        opts: Default::default(),
    };
    match crate::http::static_files::StaticFileServer::serve_file(
        dir,
        req.uri.path(),
        Some("$uri /index.html"),
        &static_req,
    )
    .await
    {
        Ok(resp) => resp,
        Err(code) => code.into_response(),
    }
}

fn cors(allowed: &[String], origin: Option<&HeaderValue>, resp: &mut Response, preflight: bool) {
    let Some(origin) = origin else { return };
    let ok = origin
        .to_str()
        .is_ok_and(|o| allowed.iter().any(|a| a.eq_ignore_ascii_case(o)));
    if !ok {
        return;
    }
    let h = resp.headers_mut();
    h.insert(header::ACCESS_CONTROL_ALLOW_ORIGIN, origin.clone());
    h.insert(
        header::ACCESS_CONTROL_ALLOW_CREDENTIALS,
        HeaderValue::from_static("true"),
    );
    h.append(header::VARY, HeaderValue::from_static("Origin"));
    if preflight {
        h.insert(
            header::ACCESS_CONTROL_ALLOW_METHODS,
            HeaderValue::from_static("GET, POST, DELETE, OPTIONS"),
        );
        h.insert(
            header::ACCESS_CONTROL_ALLOW_HEADERS,
            HeaderValue::from_static("authorization, content-type"),
        );
    }
}

fn prometheus_metrics(state: &AdminState) -> Response {
    let config = state.store.config();
    if !config.metrics.enabled {
        return (
            StatusCode::NOT_FOUND,
            "metrics are disabled; set metrics.enabled: true in ferrix.yaml",
        )
            .into_response();
    }
    let mut upstreams: Vec<(String, String, bool)> = config
        .upstreams
        .values()
        .flat_map(|u| {
            u.targets.iter().map(|t| {
                (
                    u.name.clone(),
                    t.url.clone(),
                    !t.down && state.health_checker.is_healthy(&t.url),
                )
            })
        })
        .collect();
    upstreams.sort();
    (
        [(
            header::CONTENT_TYPE,
            HeaderValue::from_static("text/plain; version=0.0.4; charset=utf-8"),
        )],
        state.metrics.prometheus(&upstreams),
    )
        .into_response()
}

fn authorized(state: &AdminState, headers: &HeaderMap, uri: &Uri) -> bool {
    authorized_with_config(&state.store.config(), headers, uri)
}

fn authorized_with_config(cfg: &FerrixConfig, headers: &HeaderMap, uri: &Uri) -> bool {
    let auth = &cfg.admin.auth;
    match auth.mode {
        AdminAuthMode::None => true,
        AdminAuthMode::Token => {
            let expected = auth.token.as_deref().unwrap_or("");
            !expected.is_empty()
                && presented_token(headers, uri).is_some_and(|t| constant_time_eq(&t, expected))
        }
        AdminAuthMode::Basic => match (&auth.username, &auth.password) {
            (Some(u), Some(p)) if !u.is_empty() => AccessControl::verify_basic_auth(
                headers,
                &crate::config::BasicAuthConfig {
                    username: u.clone(),
                    password: p.clone(),
                    ..Default::default()
                },
            ),
            _ => false,
        },
    }
}

fn dashboard_authorized(cfg: &FerrixConfig, headers: &HeaderMap, uri: &Uri) -> bool {
    if authorized_with_config(cfg, headers, uri) {
        return true;
    }
    if cfg.admin.auth.mode != AdminAuthMode::Token {
        return false;
    }
    let expected = cfg.admin.auth.token.as_deref().unwrap_or("");
    !expected.is_empty()
        && cookie_value(headers, DASHBOARD_COOKIE)
            .is_some_and(|value| constant_time_eq(value, &dashboard_cookie_value(expected)))
}

fn dashboard_bootstrap_token(cfg: &FerrixConfig, uri: &Uri) -> Option<String> {
    if cfg.admin.auth.mode != AdminAuthMode::Token {
        return None;
    }
    let expected = cfg.admin.auth.token.as_deref().unwrap_or("");
    if expected.is_empty() {
        return None;
    }
    query_token(uri)
        .filter(|token| constant_time_eq(token, expected))
        .map(|_| expected.to_string())
}

fn query_token(uri: &Uri) -> Option<String> {
    url::form_urlencoded::parse(uri.query().unwrap_or("").as_bytes())
        .find(|(key, value)| key == "token" && !value.is_empty())
        .map(|(_, value)| value.trim().to_string())
}

fn cookie_value<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    for header in headers.get_all(header::COOKIE) {
        let Ok(raw) = header.to_str() else { continue };
        for pair in raw.split(';') {
            let Some((key, value)) = pair.trim().split_once('=') else {
                continue;
            };
            if key == name {
                return Some(value);
            }
        }
    }
    None
}

fn dashboard_cookie_value(token: &str) -> String {
    use std::fmt::Write as _;
    let digest = ring::digest::digest(&ring::digest::SHA256, token.as_bytes());
    let mut value = String::with_capacity(3 + digest.as_ref().len() * 2);
    value.push_str("v1.");
    for byte in digest.as_ref() {
        let _ = write!(value, "{byte:02x}");
    }
    value
}

fn set_dashboard_cookie(resp: &mut Response, token: &str) {
    let value = dashboard_cookie_value(token);
    let cookie = format!(
        "{DASHBOARD_COOKIE}={value}; Path=/; HttpOnly; SameSite=Lax; Max-Age={DASHBOARD_COOKIE_MAX_AGE}"
    );
    if let Ok(header_value) = HeaderValue::from_str(&cookie) {
        resp.headers_mut().append(header::SET_COOKIE, header_value);
    }
}

fn protect_dashboard_response(resp: &mut Response, mode: AdminAuthMode) {
    if mode == AdminAuthMode::None {
        return;
    }
    let headers = resp.headers_mut();
    headers.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("private, no-store"),
    );
    headers.append(header::VARY, HeaderValue::from_static("Cookie"));
    headers.insert(
        header::HeaderName::from_static("x-content-type-options"),
        HeaderValue::from_static("nosniff"),
    );
    headers.insert(
        header::HeaderName::from_static("referrer-policy"),
        HeaderValue::from_static("no-referrer"),
    );
}

fn unauthorized(mode: AdminAuthMode) -> Response {
    let mut resp = (
        StatusCode::UNAUTHORIZED,
        "401 Unauthorized - The Ferrix Admin API requires authentication",
    )
        .into_response();
    resp.headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    if mode == AdminAuthMode::Basic {
        resp.headers_mut().insert(
            header::WWW_AUTHENTICATE,
            HeaderValue::from_static("Basic realm=\"Ferrix Admin\""),
        );
    }
    resp
}

fn presented_token(headers: &HeaderMap, uri: &Uri) -> Option<String> {
    if let Some(value) = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
    {
        if let Some(token) = value
            .strip_prefix("Bearer ")
            .or_else(|| value.strip_prefix("bearer "))
        {
            return Some(token.trim().to_string());
        }
    }
    if let Some(token) = headers
        .get("x-ferrix-token")
        .and_then(|v| v.to_str().ok())
        .map(|v| v.trim().to_string())
    {
        return Some(token);
    }
    url::form_urlencoded::parse(uri.query().unwrap_or("").as_bytes())
        .find(|(k, v)| k == "token" && !v.is_empty())
        .map(|(_, v)| v.trim().to_string())
}

fn constant_time_eq(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::config::store::ConfigStore;
    use crate::config::AdminAuthConfig;
    use crate::health::HealthChecker;
    use crate::metrics::MetricsCollector;
    use base64::Engine as _;
    use std::path::Path;
    use std::sync::Arc;

    #[test]
    fn constant_time_eq_matches_string_equality() {
        assert!(constant_time_eq("segredo", "segredo"));
        assert!(!constant_time_eq("segredo", "segred0"));
        assert!(!constant_time_eq("segredo", "segredo-maior"));
        assert!(!constant_time_eq("", "x"));
        assert!(constant_time_eq("", ""));
    }

    fn headers(pairs: &[(&str, &str)]) -> HeaderMap {
        let mut h = HeaderMap::new();
        for (k, v) in pairs {
            h.insert(
                header::HeaderName::from_bytes(k.as_bytes()).unwrap(),
                HeaderValue::from_str(v).unwrap(),
            );
        }
        h
    }

    fn uri(s: &str) -> Uri {
        s.parse().unwrap()
    }

    #[test]
    fn token_is_read_from_bearer_header() {
        let t = presented_token(
            &headers(&[("authorization", "Bearer abc123")]),
            &uri("/api/status"),
        );
        assert_eq!(t.as_deref(), Some("abc123"));
    }

    #[test]
    fn token_is_read_from_custom_header() {
        let t = presented_token(
            &headers(&[("x-ferrix-token", "abc123")]),
            &uri("/api/status"),
        );
        assert_eq!(t.as_deref(), Some("abc123"));
    }

    #[test]
    fn token_is_read_from_query_param() {
        let t = presented_token(&HeaderMap::new(), &uri("/api/logs/stream?token=abc123"));
        assert_eq!(t.as_deref(), Some("abc123"));
    }

    #[test]
    fn no_credential_yields_no_token() {
        assert!(presented_token(&HeaderMap::new(), &uri("/api/status")).is_none());
        assert!(presented_token(&headers(&[("authorization", "Basic eA==")]), &uri("/")).is_none());
    }

    #[test]
    fn logs_query_parses_and_decodes() {
        let q = logs_query("limit=20&status=404&q=%2Fapi%20x");
        assert_eq!(q.limit, Some(20));
        assert_eq!(q.status, Some(404));
        assert_eq!(q.q.as_deref(), Some("/api x"));
        assert_eq!(decode("my%20host"), "my host");
    }

    fn dashboard_dir() -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("ferrix-admin-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(dir.join("assets")).unwrap();
        std::fs::write(dir.join("index.html"), "<html>dashboard</html>").unwrap();
        std::fs::write(dir.join("assets/app.js"), "console.log('ok')").unwrap();
        dir
    }

    fn admin_state(mode: AdminAuthMode, dashboard: Option<&Path>) -> AdminState {
        let mut cfg = FerrixConfig::default();
        cfg.admin.auth = AdminAuthConfig {
            mode,
            token: Some("segredo".to_string()),
            username: Some("admin".to_string()),
            password: Some("pw".to_string()),
        };
        cfg.admin.dashboard_dir = dashboard.map(|dir| dir.to_string_lossy().into_owned());
        AdminState {
            store: Arc::new(ConfigStore::new(cfg, "ferrix.yaml")),
            metrics: MetricsCollector::new(1000),
            health_checker: Arc::new(HealthChecker::new()),
        }
    }

    fn peer() -> SocketAddr {
        "127.0.0.1:1234".parse().unwrap()
    }

    fn admin_request(method: Method, target: &str, headers: &[(&str, &str)]) -> Request<Body> {
        let mut request = Request::builder().method(method).uri(target);
        for (name, value) in headers {
            request = request.header(*name, *value);
        }
        request.body(Body::empty()).unwrap()
    }

    fn cookie_header(resp: &Response) -> String {
        resp.headers()
            .get(header::SET_COOKIE)
            .unwrap()
            .to_str()
            .unwrap()
            .split(';')
            .next()
            .unwrap()
            .to_string()
    }

    async fn body_text(resp: Response) -> String {
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        String::from_utf8(bytes.to_vec()).unwrap()
    }

    #[tokio::test]
    async fn dashboard_requires_authentication() {
        let dir = dashboard_dir();
        let state = admin_state(AdminAuthMode::Token, Some(&dir));
        for path in ["/", "/index.html", "/assets/app.js", "/missing"] {
            let resp = handle(&state, admin_request(Method::GET, path, &[]), peer()).await;
            assert_eq!(resp.status(), StatusCode::UNAUTHORIZED, "{path}");
            assert_eq!(
                resp.headers()
                    .get(header::CACHE_CONTROL)
                    .unwrap()
                    .to_str()
                    .unwrap(),
                "no-store"
            );
        }
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[tokio::test]
    async fn dashboard_is_served_with_a_bearer_token() {
        let dir = dashboard_dir();
        let state = admin_state(AdminAuthMode::Token, Some(&dir));
        let resp = handle(
            &state,
            admin_request(Method::GET, "/", &[("authorization", "Bearer segredo")]),
            peer(),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(
            resp.headers()
                .get(header::CACHE_CONTROL)
                .unwrap()
                .to_str()
                .unwrap(),
            "private, no-store"
        );
        assert_eq!(body_text(resp).await, "<html>dashboard</html>");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[tokio::test]
    async fn query_token_sets_a_cookie_for_the_dashboard_assets() {
        let dir = dashboard_dir();
        let state = admin_state(AdminAuthMode::Token, Some(&dir));
        let resp = handle(
            &state,
            admin_request(Method::GET, "/?token=segredo", &[]),
            peer(),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::OK);
        let set_cookie = resp
            .headers()
            .get(header::SET_COOKIE)
            .unwrap()
            .to_str()
            .unwrap()
            .to_string();
        assert!(set_cookie.contains("HttpOnly"));
        assert!(set_cookie.contains("SameSite=Lax"));
        let cookie = cookie_header(&resp);

        let asset = handle(
            &state,
            admin_request(
                Method::GET,
                "/assets/app.js",
                &[("cookie", cookie.as_str())],
            ),
            peer(),
        )
        .await;
        assert_eq!(asset.status(), StatusCode::OK);
        assert_eq!(body_text(asset).await, "console.log('ok')");

        let api = handle(
            &state,
            admin_request(Method::GET, "/api/status", &[("cookie", cookie.as_str())]),
            peer(),
        )
        .await;
        assert_eq!(api.status(), StatusCode::UNAUTHORIZED);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[tokio::test]
    async fn wrong_query_token_is_rejected_without_a_cookie() {
        let dir = dashboard_dir();
        let state = admin_state(AdminAuthMode::Token, Some(&dir));
        let resp = handle(
            &state,
            admin_request(Method::GET, "/?token=errado", &[]),
            peer(),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
        assert!(resp.headers().get(header::SET_COOKIE).is_none());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[tokio::test]
    async fn basic_mode_protects_the_dashboard() {
        let dir = dashboard_dir();
        let state = admin_state(AdminAuthMode::Basic, Some(&dir));
        let denied = handle(&state, admin_request(Method::GET, "/", &[]), peer()).await;
        assert_eq!(denied.status(), StatusCode::UNAUTHORIZED);
        assert!(denied.headers().get(header::WWW_AUTHENTICATE).is_some());

        let creds = base64::engine::general_purpose::STANDARD.encode("admin:pw");
        let auth = format!("Basic {creds}");
        let allowed = handle(
            &state,
            admin_request(Method::GET, "/", &[("authorization", auth.as_str())]),
            peer(),
        )
        .await;
        assert_eq!(allowed.status(), StatusCode::OK);
        assert!(allowed.headers().get(header::SET_COOKIE).is_none());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[tokio::test]
    async fn dashboard_is_public_when_auth_is_disabled() {
        let dir = dashboard_dir();
        let state = admin_state(AdminAuthMode::None, Some(&dir));
        let resp = handle(&state, admin_request(Method::GET, "/", &[]), peer()).await;
        assert_eq!(resp.status(), StatusCode::OK);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[tokio::test]
    async fn rotating_the_token_invalidates_the_dashboard_cookie() {
        let dir = dashboard_dir();
        let state = admin_state(AdminAuthMode::Token, Some(&dir));
        let resp = handle(
            &state,
            admin_request(Method::GET, "/?token=segredo", &[]),
            peer(),
        )
        .await;
        let cookie = cookie_header(&resp);

        let mut next = (*state.store.config()).clone();
        next.admin.auth.token = Some("novo".to_string());
        state.store.apply(next);

        let asset = handle(
            &state,
            admin_request(
                Method::GET,
                "/assets/app.js",
                &[("cookie", cookie.as_str())],
            ),
            peer(),
        )
        .await;
        assert_eq!(asset.status(), StatusCode::UNAUTHORIZED);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
