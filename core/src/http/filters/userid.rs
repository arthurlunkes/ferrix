//! nginx `ngx_http_userid_filter_module`: give each visitor a long-lived
//! identifying cookie. A new id is also added to the request's Cookie header so
//! the backend sees it on the very first request.

use axum::http::{header, HeaderMap, HeaderValue};
use base64::Engine;

use crate::config::UserIdConfig;

/// Returns the Set-Cookie to send when the visitor has no id yet.
pub fn ensure(cfg: &UserIdConfig, req_headers: &mut HeaderMap) -> Option<HeaderValue> {
    let name = cfg.name();
    let has = req_headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .any(|kv| kv.trim().split_once('=').is_some_and(|(k, _)| k == name));
    if has {
        return None;
    }
    let id =
        base64::engine::general_purpose::STANDARD_NO_PAD.encode(uuid::Uuid::new_v4().as_bytes());
    let pair = format!("{name}={id}");
    let cookie = match req_headers
        .get(header::COOKIE)
        .and_then(|v| v.to_str().ok())
    {
        Some(c) if !c.is_empty() => format!("{c}; {pair}"),
        _ => pair.clone(),
    };
    if let Ok(v) = HeaderValue::from_str(&cookie) {
        req_headers.insert(header::COOKIE, v);
    }
    let mut set = format!("{pair}; Path={}", cfg.path.as_deref().unwrap_or("/"));
    if let Some(d) = &cfg.domain {
        set.push_str(&format!("; Domain={d}"));
    }
    match cfg.expires_secs {
        Some(0) | None => {}
        Some(s) => set.push_str(&format!("; Max-Age={s}")),
    }
    if let Some(flags) = &cfg.flags {
        for f in flags.split_whitespace() {
            set.push_str("; ");
            set.push_str(f);
        }
    }
    HeaderValue::from_str(&set).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sets_once() {
        let cfg = UserIdConfig {
            name: Some("uid".into()),
            expires_secs: Some(3600),
            flags: Some("Secure HttpOnly".into()),
            ..Default::default()
        };
        let mut h = HeaderMap::new();
        h.insert(header::COOKIE, "a=1".parse().unwrap());
        let set = ensure(&cfg, &mut h).unwrap();
        let set = set.to_str().unwrap();
        assert!(
            set.starts_with("uid=") && set.contains("; Max-Age=3600; Secure; HttpOnly"),
            "{set}"
        );
        assert!(h[header::COOKIE].to_str().unwrap().starts_with("a=1; uid="));
        assert!(ensure(&cfg, &mut h).is_none());
    }
}
