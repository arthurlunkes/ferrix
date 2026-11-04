//! nginx `sticky cookie <name>`: pin a client to the backend that served it.
//! The cookie holds a short hash of the target URL (not the URL itself).

use axum::http::{header, HeaderMap, HeaderValue};
use std::hash::Hasher;

use crate::config::{StickyConfig, UpstreamTarget};

pub fn target_id(url: &str) -> String {
    let mut h = crate::fasthash::FxHasher::default();
    h.write(url.as_bytes());
    format!("{:016x}", h.finish())
}

pub fn cookie_value<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .filter_map(|kv| kv.trim().split_once('='))
        .find(|(k, _)| *k == name)
        .map(|(_, v)| v)
}

/// Moves the target named by the cookie (if still in the list) to the front.
pub fn prefer(targets: &mut [&UpstreamTarget], headers: &HeaderMap, cfg: &StickyConfig) {
    let Some(id) = cookie_value(headers, &cfg.cookie) else {
        return;
    };
    if let Some(i) = targets.iter().position(|t| target_id(&t.url) == id) {
        targets[..=i].rotate_right(1);
    }
}

/// `Set-Cookie` for `target` unless the request already carries it.
pub fn set_cookie(cfg: &StickyConfig, req: &HeaderMap, target_url: &str) -> Option<HeaderValue> {
    let id = target_id(target_url);
    if cookie_value(req, &cfg.cookie) == Some(id.as_str()) {
        return None;
    }
    let mut c = format!("{}={}; Path={}; HttpOnly", cfg.cookie, id, cfg.path());
    if let Some(secs) = cfg.max_age_secs {
        c.push_str(&format!("; Max-Age={secs}"));
    }
    HeaderValue::from_str(&c).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(url: &str) -> UpstreamTarget {
        UpstreamTarget {
            url: url.into(),
            weight: 1,
            max_fails: 1,
            fail_timeout_secs: 1,
            backup: false,
            down: false,
            max_conns: None,
        }
    }

    #[test]
    fn cookie_pins_target() {
        let cfg = StickyConfig {
            cookie: "srv".into(),
            ..Default::default()
        };
        let (a, b, c) = (t("http://a"), t("http://b"), t("http://c"));
        let mut list = vec![&a, &b, &c];
        let mut h = HeaderMap::new();
        h.insert(
            header::COOKIE,
            format!("x=1; srv={}", target_id("http://c"))
                .parse()
                .unwrap(),
        );
        prefer(&mut list, &h, &cfg);
        assert_eq!(list[0].url, "http://c");
        assert_eq!(list[1].url, "http://a");
        assert!(set_cookie(&cfg, &h, "http://c").is_none());
        let sc = set_cookie(&cfg, &h, "http://a").unwrap();
        assert!(sc.to_str().unwrap().starts_with("srv="));
    }
}
