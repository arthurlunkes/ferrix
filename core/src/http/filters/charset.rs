use axum::body::Body;
use axum::http::{header, HeaderValue, Response};

use crate::config::RouteRule;

const MAX_BODY: usize = 8 << 20;

fn is_text(mime: &str, types: &[String]) -> bool {
    let m = mime.to_ascii_lowercase();
    m.starts_with("text/html")
        || m.starts_with("text/plain")
        || m.starts_with("text/xml")
        || m == "application/javascript"
        || m == "application/rss+xml"
        || types.iter().any(|t| t == "*" || t.eq_ignore_ascii_case(&m))
}

pub async fn apply(route: &RouteRule, resp: Response<Body>) -> Response<Body> {
    let Some(target) = route.charset.as_deref().filter(|c| *c != "off") else {
        return resp;
    };
    if resp.headers().contains_key(header::CONTENT_ENCODING) {
        return resp;
    }
    let ct = resp
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    let mime = ct.split(';').next().unwrap_or("").trim().to_string();
    if !is_text(&mime, &route.charset_types) {
        return resp;
    }
    let declared = ct.split(';').skip(1).find_map(|p| {
        p.trim()
            .strip_prefix("charset=")
            .map(|c| c.trim_matches('"').to_string())
    });

    let source = match (&declared, route.override_charset) {
        (Some(_), false) => return resp,
        (Some(d), true) => Some(d.clone()),
        (None, _) => route.source_charset.clone(),
    };

    let (mut parts, body) = resp.into_parts();
    let set_ct = |parts: &mut axum::http::response::Parts| {
        if let Ok(v) = HeaderValue::from_str(&format!("{mime}; charset={target}")) {
            parts.headers.insert(header::CONTENT_TYPE, v);
        }
    };
    let from = source
        .as_deref()
        .and_then(|s| encoding_rs::Encoding::for_label(s.as_bytes()));
    let to = encoding_rs::Encoding::for_label(target.as_bytes());
    match (from, to) {
        (Some(f), Some(t)) if f != t => {
            let Ok(bytes) = axum::body::to_bytes(body, MAX_BODY).await else {
                return Response::from_parts(parts, Body::empty());
            };
            let (text, _, _) = f.decode(&bytes);
            let (out, _, _) = t.encode(&text);
            let out = out.into_owned();
            set_ct(&mut parts);
            parts.headers.remove(header::ETAG);
            parts.extensions.remove::<crate::http::server::SendFile>();
            parts
                .headers
                .insert(header::CONTENT_LENGTH, HeaderValue::from(out.len()));
            Response::from_parts(parts, Body::from(out))
        }
        _ => {
            set_ct(&mut parts);
            Response::from_parts(parts, body)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn resp(ct: &str, body: &'static [u8]) -> Response<Body> {
        Response::builder()
            .header(header::CONTENT_TYPE, ct)
            .body(Body::from(body))
            .unwrap()
    }

    #[tokio::test]
    async fn declares_and_converts() {
        let mut r = RouteRule {
            charset: Some("utf-8".into()),
            ..Default::default()
        };
        let out = apply(&r, resp("text/html", b"ok")).await;
        assert_eq!(
            out.headers()[header::CONTENT_TYPE],
            "text/html; charset=utf-8"
        );

        r.source_charset = Some("windows-1251".into());
        let out = apply(&r, resp("text/plain", b"\xcf\xf0\xe8\xe2\xe5\xf2")).await;
        let b = axum::body::to_bytes(out.into_body(), 1024).await.unwrap();
        assert_eq!(std::str::from_utf8(&b).unwrap(), "Привет");

        let out = apply(&r, resp("text/html; charset=iso-8859-1", b"x")).await;
        assert_eq!(
            out.headers()[header::CONTENT_TYPE],
            "text/html; charset=iso-8859-1"
        );
        let out = apply(&r, resp("image/png", b"x")).await;
        assert_eq!(out.headers()[header::CONTENT_TYPE], "image/png");
    }
}
