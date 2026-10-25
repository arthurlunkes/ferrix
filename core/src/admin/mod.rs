pub mod handlers;
pub mod sse;

use axum::extract::{Request, State};
use axum::http::{header, HeaderValue, Method, StatusCode};
use axum::middleware::{self, Next};
use axum::response::IntoResponse;
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use tower_http::cors::{AllowOrigin, CorsLayer};

use crate::config::AdminAuthMode;
use crate::proxy::middleware::AccessControl;
use handlers::AdminState;

pub fn create_admin_router(state: AdminState) -> Router {
    let api_router = Router::new()
        .route("/status", get(handlers::get_status))
        .route("/metrics", get(handlers::get_metrics))
        .route("/logs", get(handlers::get_logs))
        .route(
            "/logs/stream",
            get(|State(state): State<AdminState>| async move {
                sse::stream_logs(state.metrics.clone()).await
            }),
        )
        .route(
            "/config",
            get(handlers::get_config).post(handlers::update_config),
        )
        .route("/config/validate", post(handlers::validate_config))
        .route("/route-test", post(handlers::route_test))
        .route("/import/nginx", post(handlers::import_nginx))
        .route(
            "/vhosts",
            get(handlers::get_vhosts).post(handlers::save_vhost),
        )
        .route("/vhosts/{id}", delete(handlers::delete_vhost))
        .route(
            "/upstreams",
            get(handlers::get_upstreams).post(handlers::save_upstream),
        )
        .route("/upstreams/{name}", delete(handlers::delete_upstream))
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            require_admin_auth,
        ))
        .with_state(state.clone());

    let mut router = Router::new()
        .route(
            "/",
            get(|| async {
                Json(serde_json::json!({
                    "service": "Ferrix Core Control Plane",
                    "version": env!("CARGO_PKG_VERSION"),
                    "status": "online"
                }))
            }),
        )
        .nest("/api", api_router);

    let origins = &state.store.config().admin.cors_origins;
    if !origins.is_empty() {
        let parsed: Vec<HeaderValue> = origins
            .iter()
            .filter_map(|o| o.parse::<HeaderValue>().ok())
            .collect();
        router = router.layer(
            CorsLayer::new()
                .allow_origin(AllowOrigin::list(parsed))
                .allow_methods([Method::GET, Method::POST, Method::DELETE, Method::OPTIONS])
                .allow_headers([header::AUTHORIZATION, header::CONTENT_TYPE])
                .allow_credentials(true),
        );
    }

    router
}

async fn require_admin_auth(
    State(state): State<AdminState>,
    req: Request,
    next: Next,
) -> axum::response::Response {
    let cfg = state.store.config();
    let auth = &cfg.admin.auth;

    let authorized = match auth.mode {
        AdminAuthMode::None => true,
        AdminAuthMode::Token => {
            let expected = auth.token.as_deref().unwrap_or("");

            !expected.is_empty()
                && presented_token(&req).is_some_and(|t| constant_time_eq(&t, expected))
        }
        AdminAuthMode::Basic => match (&auth.username, &auth.password) {
            (Some(u), Some(p)) if !u.is_empty() => AccessControl::verify_basic_auth(
                req.headers(),
                &crate::config::BasicAuthConfig {
                    username: u.clone(),
                    password: p.clone(),
                },
            ),
            _ => false,
        },
    };

    if authorized {
        return next.run(req).await;
    }

    let mut resp = (
        StatusCode::UNAUTHORIZED,
        "401 Unauthorized - The Ferrix Admin API requires authentication",
    )
        .into_response();

    if auth.mode == AdminAuthMode::Basic {
        resp.headers_mut().insert(
            header::WWW_AUTHENTICATE,
            HeaderValue::from_static("Basic realm=\"Ferrix Admin\""),
        );
    }
    resp
}

fn presented_token(req: &Request) -> Option<String> {
    let headers = req.headers();
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
    if let Some(query) = req.uri().query() {
        for pair in query.split('&') {
            if let Some((k, v)) = pair.split_once('=') {
                if k == "token" && !v.is_empty() {
                    return Some(v.trim().to_string());
                }
            }
        }
    }
    None
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

    #[test]
    fn constant_time_eq_matches_string_equality() {
        assert!(constant_time_eq("segredo", "segredo"));
        assert!(!constant_time_eq("segredo", "segred0"));
        assert!(!constant_time_eq("segredo", "segredo-maior"));
        assert!(!constant_time_eq("", "x"));
        assert!(constant_time_eq("", ""));
    }

    fn request_with(headers: &[(&str, &str)]) -> Request {
        let mut builder = Request::builder().uri("/api/status");
        for (k, v) in headers {
            builder = builder.header(*k, *v);
        }
        builder.body(axum::body::Body::empty()).unwrap()
    }

    #[test]
    fn token_is_read_from_bearer_header() {
        let req = request_with(&[("authorization", "Bearer abc123")]);
        assert_eq!(presented_token(&req).as_deref(), Some("abc123"));
    }

    #[test]
    fn token_is_read_from_custom_header() {
        let req = request_with(&[("x-ferrix-token", "abc123")]);
        assert_eq!(presented_token(&req).as_deref(), Some("abc123"));
    }

    #[test]
    fn token_is_read_from_query_param() {
        let req = Request::builder()
            .uri("/api/logs/stream?token=abc123")
            .body(axum::body::Body::empty())
            .unwrap();
        assert_eq!(presented_token(&req).as_deref(), Some("abc123"));
    }

    #[test]
    fn no_credential_yields_no_token() {
        assert!(presented_token(&request_with(&[])).is_none());

        assert!(presented_token(&request_with(&[("authorization", "Basic eA==")])).is_none());
    }
}
