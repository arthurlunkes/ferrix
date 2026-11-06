use axum::http::{header, HeaderMap, HeaderValue, Method, StatusCode, Version};

use crate::config::HttpConfig;

pub const DEFAULT_METHODS: &[&str] = &[
    "GET",
    "HEAD",
    "POST",
    "PUT",
    "DELETE",
    "PATCH",
    "OPTIONS",
    "CONNECT",
    "PROPFIND",
    "PROPPATCH",
    "MKCOL",
    "COPY",
    "MOVE",
    "LOCK",
    "UNLOCK",
    "SEARCH",
    "REPORT",
    "MKCALENDAR",
];

pub fn method_token_ok(m: &[u8]) -> bool {
    !m.is_empty()
        && m.len() <= 32
        && m.iter()
            .all(|b| b.is_ascii_uppercase() || *b == b'_' || *b == b'-')
}

pub fn method_allowed(http: &HttpConfig, m: &Method) -> bool {
    match &http.allowed_methods {
        Some(list) => list.iter().any(|a| a == m.as_str()),
        None => DEFAULT_METHODS.contains(&m.as_str()),
    }
}

pub fn check(
    http: &HttpConfig,
    method: &Method,
    version: Version,
    headers: &mut HeaderMap,
) -> Result<(), StatusCode> {
    if !method_token_ok(method.as_str().as_bytes()) {
        return Err(StatusCode::BAD_REQUEST);
    }
    if !method_allowed(http, method) {
        return Err(StatusCode::METHOD_NOT_ALLOWED);
    }
    let max = http.max_header_line_bytes.max(256);
    let mut hosts = 0;
    let mut cl: Option<&HeaderValue> = None;
    let mut te = false;
    let mut underscore = false;
    for (k, v) in headers.iter() {
        if k.as_str().len() + v.len() + 2 > max {
            return Err(StatusCode::BAD_REQUEST);
        }
        if *k == header::HOST {
            hosts += 1;
        } else if *k == header::CONTENT_LENGTH {
            if cl.is_some_and(|c| c != v)
                || v.to_str()
                    .ok()
                    .and_then(|s| s.trim().parse::<u64>().ok())
                    .is_none()
            {
                return Err(StatusCode::BAD_REQUEST);
            }
            cl = Some(v);
        } else if *k == header::TRANSFER_ENCODING {
            te = true;
        }
        underscore |= k.as_str().contains('_');
    }
    if hosts > 1 || (hosts == 0 && version == Version::HTTP_11) || (te && cl.is_some()) {
        return Err(StatusCode::BAD_REQUEST);
    }
    if underscore && !http.underscores_in_headers {
        let names: Vec<_> = headers
            .keys()
            .filter(|k| k.as_str().contains('_'))
            .cloned()
            .collect();
        for n in names {
            headers.remove(n);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn h(pairs: &[(&str, &str)]) -> HeaderMap {
        let mut m = HeaderMap::new();
        for (k, v) in pairs {
            m.append(
                axum::http::HeaderName::from_bytes(k.as_bytes()).unwrap(),
                v.parse().unwrap(),
            );
        }
        m
    }

    #[test]
    fn nginx_rules() {
        let http = HttpConfig::default();
        let get = Method::GET;
        let v11 = Version::HTTP_11;
        assert!(check(&http, &get, v11, &mut h(&[("host", "a")])).is_ok());
        assert_eq!(
            check(&http, &get, v11, &mut h(&[])),
            Err(StatusCode::BAD_REQUEST)
        );
        assert!(check(&http, &get, Version::HTTP_10, &mut h(&[])).is_ok());
        assert_eq!(
            check(&http, &get, v11, &mut h(&[("host", "a"), ("host", "b")])),
            Err(StatusCode::BAD_REQUEST)
        );
        assert_eq!(
            check(
                &http,
                &Method::POST,
                v11,
                &mut h(&[
                    ("host", "a"),
                    ("content-length", "3"),
                    ("transfer-encoding", "chunked")
                ])
            ),
            Err(StatusCode::BAD_REQUEST)
        );
        assert_eq!(
            check(
                &http,
                &Method::POST,
                v11,
                &mut h(&[
                    ("host", "a"),
                    ("content-length", "3"),
                    ("content-length", "4")
                ])
            ),
            Err(StatusCode::BAD_REQUEST)
        );
        let lower = Method::from_bytes(b"get").unwrap();
        assert_eq!(
            check(&http, &lower, v11, &mut h(&[("host", "a")])),
            Err(StatusCode::BAD_REQUEST)
        );
        assert_eq!(
            check(&http, &Method::TRACE, v11, &mut h(&[("host", "a")])),
            Err(StatusCode::METHOD_NOT_ALLOWED)
        );
        let foo = Method::from_bytes(b"FOO").unwrap();
        assert_eq!(
            check(&http, &foo, v11, &mut h(&[("host", "a")])),
            Err(StatusCode::METHOD_NOT_ALLOWED)
        );
        let mut hm = h(&[("host", "a"), ("x_forwarded_for", "1.1.1.1"), ("x-ok", "1")]);
        check(&http, &get, v11, &mut hm).unwrap();
        assert!(hm.get("x_forwarded_for").is_none() && hm.get("x-ok").is_some());
    }
}
