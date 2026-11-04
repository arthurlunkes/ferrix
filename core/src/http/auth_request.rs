use axum::body::Body;
use axum::http::{header, HeaderMap, HeaderName, HeaderValue, Method, Response, StatusCode};
use axum::response::IntoResponse;
use std::time::Duration;

use crate::config::AuthRequestConfig;
use crate::http::upstream::{OutgoingBody, UpstreamClient};

pub enum Outcome {
    Allow(Vec<(HeaderName, HeaderValue)>),
    Deny(Response<Body>),
}

fn split_url(url: &str) -> Option<(&str, &str)> {
    let after = url.find("://")? + 3;
    match url[after..].find('/') {
        Some(i) => Some((&url[..after + i], &url[after + i..])),
        None => Some((url, "/")),
    }
}

pub async fn check(
    cfg: &AuthRequestConfig,
    method: &Method,
    uri: &axum::http::Uri,
    headers: &HeaderMap,
) -> Outcome {
    let Some((base, path)) = split_url(&cfg.url) else {
        return Outcome::Deny(StatusCode::INTERNAL_SERVER_ERROR.into_response());
    };
    let mut h = headers.clone();
    for n in [
        header::CONTENT_LENGTH,
        header::TRANSFER_ENCODING,
        header::HOST,
    ] {
        h.remove(n);
    }
    if let Ok(v) = HeaderValue::from_str(uri.path_and_query().map(|p| p.as_str()).unwrap_or("/")) {
        h.insert("x-original-uri", v);
    }
    if let Ok(v) = HeaderValue::from_str(method.as_str()) {
        h.insert("x-original-method", v);
    }

    let timeout = Duration::from_millis(cfg.timeout_ms.unwrap_or(5000));
    let client = UpstreamClient::new(timeout, timeout, true);
    let resp = match client
        .send(&Method::GET, base, path, &h, OutgoingBody::Empty)
        .await
    {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!(url = %cfg.url, error = %e, "auth_request failed");
            return Outcome::Deny(StatusCode::INTERNAL_SERVER_ERROR.into_response());
        }
    };

    let status = resp.status();
    if status.is_success() {
        let pass = cfg
            .set_headers
            .iter()
            .filter_map(|name| {
                let n = HeaderName::from_bytes(name.as_bytes()).ok()?;
                let v = resp.headers().get(&n)?.clone();
                Some((n, v))
            })
            .collect();
        return Outcome::Allow(pass);
    }
    let mut out = match status {
        StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => (
            status,
            format!(
                "{} {}",
                status.as_u16(),
                status.canonical_reason().unwrap_or("")
            ),
        )
            .into_response(),
        _ => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };
    if status == StatusCode::UNAUTHORIZED {
        if let Some(v) = resp.headers().get(header::WWW_AUTHENTICATE) {
            out.headers_mut()
                .insert(header::WWW_AUTHENTICATE, v.clone());
        }
    }
    Outcome::Deny(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_urls() {
        assert_eq!(
            split_url("http://auth:9000/check?x=1"),
            Some(("http://auth:9000", "/check?x=1"))
        );
        assert_eq!(split_url("https://a.b"), Some(("https://a.b", "/")));
        assert_eq!(split_url("auth/check"), None);
    }

    async fn auth_server(status: u16) -> String {
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = l.local_addr().unwrap();
        tokio::spawn(async move {
            use tokio::io::{AsyncReadExt, AsyncWriteExt};
            let (mut s, _) = l.accept().await.unwrap();
            let mut buf = [0u8; 4096];
            let n = s.read(&mut buf).await.unwrap();
            let req = String::from_utf8_lossy(&buf[..n]).to_ascii_lowercase();
            assert!(req.contains("x-original-uri: /secret"));
            let resp = format!(
                "HTTP/1.1 {status} X\r\nx-user: ana\r\ncontent-length: 0\r\nconnection: close\r\n\r\n"
            );
            s.write_all(resp.as_bytes()).await.unwrap();
        });
        format!("http://{addr}/auth")
    }

    fn cfg(url: String) -> AuthRequestConfig {
        AuthRequestConfig {
            url,
            set_headers: vec!["x-user".into()],
            timeout_ms: Some(2000),
        }
    }

    #[tokio::test]
    async fn allow_copies_headers_and_deny_maps_status() {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let uri: axum::http::Uri = "/secret".parse().unwrap();
        let c = cfg(auth_server(200).await);
        match check(&c, &Method::GET, &uri, &HeaderMap::new()).await {
            Outcome::Allow(h) => assert_eq!(h[0].1, "ana"),
            Outcome::Deny(r) => panic!("denied {}", r.status()),
        }
        let c = cfg(auth_server(403).await);
        match check(&c, &Method::GET, &uri, &HeaderMap::new()).await {
            Outcome::Deny(r) => assert_eq!(r.status(), StatusCode::FORBIDDEN),
            Outcome::Allow(_) => panic!("allowed"),
        }
        let c = cfg(auth_server(404).await);
        match check(&c, &Method::GET, &uri, &HeaderMap::new()).await {
            Outcome::Deny(r) => assert_eq!(r.status(), StatusCode::INTERNAL_SERVER_ERROR),
            Outcome::Allow(_) => panic!("allowed"),
        }
    }
}
