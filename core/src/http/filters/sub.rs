//! nginx `ngx_http_sub_filter_module`: replace strings in text responses.
//!
//! Only uncompressed responses with a matching Content-Type and at most
//! `MAX_BODY` bytes are rewritten; anything else passes through untouched.

use axum::body::Body;
use axum::http::{header, HeaderValue, Response};

use crate::config::SubFilterConfig;

const MAX_BODY: usize = 8 << 20;

fn applies(cfg: &SubFilterConfig, resp: &Response<Body>) -> bool {
    if cfg.replacements.is_empty() || resp.headers().contains_key(header::CONTENT_ENCODING) {
        return false;
    }
    let ct = resp
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    let mime = ct.split(';').next().unwrap_or("").trim();
    if mime.is_empty() {
        return false;
    }
    mime.eq_ignore_ascii_case("text/html")
        || cfg
            .types
            .iter()
            .any(|t| t == "*" || t.eq_ignore_ascii_case(mime))
}

pub fn replace(cfg: &SubFilterConfig, text: &str) -> String {
    let mut out = text.to_string();
    for [from, to] in &cfg.replacements {
        if from.is_empty() {
            continue;
        }
        out = if cfg.once {
            out.replacen(from.as_str(), to, 1)
        } else {
            out.replace(from.as_str(), to)
        };
    }
    out
}

pub async fn apply(cfg: &SubFilterConfig, resp: Response<Body>) -> Response<Body> {
    if !applies(cfg, &resp) {
        return resp;
    }
    let (mut parts, body) = resp.into_parts();
    let bytes = match axum::body::to_bytes(body, MAX_BODY).await {
        Ok(b) => b,
        Err(e) => {
            tracing::warn!(error = %e, "sub_filter: body too large or failed");
            return Response::from_parts(parts, Body::empty());
        }
    };
    let Ok(text) = std::str::from_utf8(&bytes) else {
        return Response::from_parts(parts, Body::from(bytes));
    };
    let out = replace(cfg, text);
    parts.headers.remove(header::ETAG);
    parts.headers.remove(header::LAST_MODIFIED);
    parts
        .headers
        .insert(header::CONTENT_LENGTH, HeaderValue::from(out.len()));
    Response::from_parts(parts, Body::from(out))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(once: bool) -> SubFilterConfig {
        SubFilterConfig {
            replacements: vec![["http://a".into(), "https://b".into()]],
            once,
            types: vec![],
        }
    }

    #[tokio::test]
    async fn rewrites_html_only() {
        let resp = Response::builder()
            .header(header::CONTENT_TYPE, "text/html; charset=utf-8")
            .body(Body::from("x http://a y http://a"))
            .unwrap();
        let out = apply(&cfg(false), resp).await;
        let b = axum::body::to_bytes(out.into_body(), 1024).await.unwrap();
        assert_eq!(&b[..], b"x https://b y https://b");

        let resp = Response::builder()
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from("http://a"))
            .unwrap();
        let out = apply(&cfg(false), resp).await;
        let b = axum::body::to_bytes(out.into_body(), 1024).await.unwrap();
        assert_eq!(&b[..], b"http://a");
    }

    #[test]
    fn once_replaces_first_only() {
        assert_eq!(
            replace(&cfg(true), "http://a http://a"),
            "https://b http://a"
        );
    }
}
