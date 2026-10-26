//! Per-route CORS handling: answers preflight requests and decorates responses.

use axum::body::Body;
use axum::http::{header, HeaderMap, HeaderValue, Method, Response, StatusCode};

use crate::config::CorsConfig;

fn allowed_origin(cfg: &CorsConfig, origin: &HeaderValue) -> Option<HeaderValue> {
    let o = origin.to_str().ok()?;
    if cfg.allow_origins.iter().any(|a| a == "*") {
        // With credentials the spec forbids `*`, so echo the origin back instead.
        return Some(if cfg.allow_credentials {
            origin.clone()
        } else {
            HeaderValue::from_static("*")
        });
    }
    cfg.allow_origins
        .iter()
        .any(|a| a.eq_ignore_ascii_case(o))
        .then(|| origin.clone())
}

fn join(values: &[String]) -> Option<HeaderValue> {
    if values.is_empty() {
        return None;
    }
    HeaderValue::from_str(&values.join(", ")).ok()
}

fn base_headers(cfg: &CorsConfig, allow: HeaderValue, h: &mut HeaderMap) {
    let wildcard = allow == "*";
    h.insert(header::ACCESS_CONTROL_ALLOW_ORIGIN, allow);
    if !wildcard {
        h.append(header::VARY, HeaderValue::from_static("Origin"));
    }
    if cfg.allow_credentials {
        h.insert(
            header::ACCESS_CONTROL_ALLOW_CREDENTIALS,
            HeaderValue::from_static("true"),
        );
    }
}

/// Response for a CORS preflight (`OPTIONS` with `Access-Control-Request-Method`), or
/// `None` when the request isn't one and should be routed normally.
pub fn preflight(cfg: &CorsConfig, method: &Method, headers: &HeaderMap) -> Option<Response<Body>> {
    if method != Method::OPTIONS || !headers.contains_key(header::ACCESS_CONTROL_REQUEST_METHOD) {
        return None;
    }
    let origin = headers.get(header::ORIGIN)?;
    let mut resp = Response::new(Body::empty());
    *resp.status_mut() = StatusCode::NO_CONTENT;
    let Some(allow) = allowed_origin(cfg, origin) else {
        return Some(resp);
    };
    let h = resp.headers_mut();
    base_headers(cfg, allow, h);
    let methods = if cfg.allow_methods.is_empty() {
        HeaderValue::from_static("GET, POST, PUT, PATCH, DELETE, HEAD, OPTIONS")
    } else {
        join(&cfg.allow_methods).unwrap_or(HeaderValue::from_static("GET"))
    };
    h.insert(header::ACCESS_CONTROL_ALLOW_METHODS, methods);
    match join(&cfg.allow_headers) {
        Some(v) => {
            h.insert(header::ACCESS_CONTROL_ALLOW_HEADERS, v);
        }
        None => {
            // No explicit list: allow whatever the browser asked for.
            if let Some(req) = headers.get(header::ACCESS_CONTROL_REQUEST_HEADERS) {
                h.insert(header::ACCESS_CONTROL_ALLOW_HEADERS, req.clone());
            }
        }
    }
    if let Some(age) = cfg.max_age_secs {
        h.insert(header::ACCESS_CONTROL_MAX_AGE, HeaderValue::from(age));
    }
    Some(resp)
}

pub fn decorate(cfg: &CorsConfig, origin: Option<&HeaderValue>, resp: &mut Response<Body>) {
    let Some(allow) = origin.and_then(|o| allowed_origin(cfg, o)) else {
        return;
    };
    let h = resp.headers_mut();
    base_headers(cfg, allow, h);
    if let Some(v) = join(&cfg.expose_headers) {
        h.insert(header::ACCESS_CONTROL_EXPOSE_HEADERS, v);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(origins: &[&str], creds: bool) -> CorsConfig {
        CorsConfig {
            allow_origins: origins.iter().map(|s| s.to_string()).collect(),
            allow_credentials: creds,
            max_age_secs: Some(600),
            ..Default::default()
        }
    }

    fn preflight_headers(origin: &str) -> HeaderMap {
        let mut h = HeaderMap::new();
        h.insert(header::ORIGIN, HeaderValue::from_str(origin).unwrap());
        h.insert(
            header::ACCESS_CONTROL_REQUEST_METHOD,
            HeaderValue::from_static("PUT"),
        );
        h.insert(
            header::ACCESS_CONTROL_REQUEST_HEADERS,
            HeaderValue::from_static("x-token"),
        );
        h
    }

    #[test]
    fn preflight_for_allowed_origin() {
        let c = cfg(&["https://app.example.com"], true);
        let r = preflight(
            &c,
            &Method::OPTIONS,
            &preflight_headers("https://app.example.com"),
        )
        .unwrap();
        assert_eq!(r.status(), StatusCode::NO_CONTENT);
        let h = r.headers();
        assert_eq!(
            h[header::ACCESS_CONTROL_ALLOW_ORIGIN],
            "https://app.example.com"
        );
        assert_eq!(h[header::ACCESS_CONTROL_ALLOW_CREDENTIALS], "true");
        assert_eq!(h[header::ACCESS_CONTROL_ALLOW_HEADERS], "x-token");
        assert_eq!(h[header::ACCESS_CONTROL_MAX_AGE], "600");
    }

    #[test]
    fn disallowed_origin_gets_no_cors_headers() {
        let c = cfg(&["https://app.example.com"], false);
        let r = preflight(
            &c,
            &Method::OPTIONS,
            &preflight_headers("https://evil.test"),
        )
        .unwrap();
        assert!(!r
            .headers()
            .contains_key(header::ACCESS_CONTROL_ALLOW_ORIGIN));
    }

    #[test]
    fn wildcard_without_credentials_is_star() {
        let c = cfg(&["*"], false);
        let mut resp = Response::new(Body::empty());
        decorate(
            &c,
            Some(&HeaderValue::from_static("https://x.test")),
            &mut resp,
        );
        assert_eq!(resp.headers()[header::ACCESS_CONTROL_ALLOW_ORIGIN], "*");
    }

    #[test]
    fn plain_options_is_not_a_preflight() {
        let c = cfg(&["*"], false);
        assert!(preflight(&c, &Method::OPTIONS, &HeaderMap::new()).is_none());
    }
}
