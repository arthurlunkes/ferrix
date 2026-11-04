use axum::http::{header, HeaderMap, HeaderValue};

use crate::config::RouteRule;

pub fn needed(route: &RouteRule) -> bool {
    !route.proxy_redirect.is_empty()
        || !route.proxy_cookie_path.is_empty()
        || !route.proxy_cookie_domain.is_empty()
        || !route.proxy_cookie_flags.is_empty()
}

fn redirect_one(
    value: &str,
    from: &str,
    to: &str,
    target_url: &str,
    prefix: &str,
) -> Option<String> {
    if from == "default" {
        let base = target_url.trim_end_matches('/');
        let rest = value.strip_prefix(base)?;
        let rest = rest.strip_prefix('/').unwrap_or(rest);
        let p = prefix.trim_end_matches('/');
        return Some(format!("{p}/{rest}"));
    }
    if let Some(re) = from
        .strip_prefix("~*")
        .map(|r| (r, true))
        .or_else(|| from.strip_prefix('~').map(|r| (r, false)))
    {
        let re = crate::http::vars::regex(re.0, re.1)?;
        let caps = re.captures(value)?;
        let mut out = String::new();
        caps.expand(to, &mut out);
        let m = caps.get(0)?;
        return Some(format!(
            "{}{}{}",
            &value[..m.start()],
            out,
            &value[m.end()..]
        ));
    }
    value.strip_prefix(from).map(|rest| format!("{to}{rest}"))
}

pub fn rewrite_location(value: &str, route: &RouteRule, target_url: &str) -> Option<String> {
    route
        .proxy_redirect
        .iter()
        .find_map(|[from, to]| redirect_one(value, from, to, target_url, &route.path_prefix))
}

pub fn rewrite_cookie(value: &str, route: &RouteRule) -> String {
    let mut parts: Vec<String> = value.split(';').map(|s| s.trim().to_string()).collect();
    let name = parts
        .first()
        .and_then(|p| p.split_once('='))
        .map(|(n, _)| n.trim().to_string())
        .unwrap_or_default();

    for p in parts.iter_mut().skip(1) {
        let lower = p.to_ascii_lowercase();
        if let Some(v) = lower.strip_prefix("path=").map(|_| p[5..].to_string()) {
            for [from, to] in &route.proxy_cookie_path {
                if let Some(rest) = v.strip_prefix(from.as_str()) {
                    *p = format!("Path={to}{rest}");
                    break;
                }
            }
        } else if let Some(v) = lower.strip_prefix("domain=").map(|_| p[7..].to_string()) {
            let bare = v.trim_start_matches('.');
            for [from, to] in &route.proxy_cookie_domain {
                if bare.eq_ignore_ascii_case(from.trim_start_matches('.')) {
                    *p = format!("Domain={to}");
                    break;
                }
            }
        }
    }

    for [pat, flags] in &route.proxy_cookie_flags {
        let hit = if pat == "*" {
            true
        } else if let Some(re) = pat.strip_prefix('~') {
            crate::http::vars::regex(re, false).is_some_and(|r| r.is_match(&name))
        } else {
            pat == &name
        };
        if !hit {
            continue;
        }
        for flag in flags.split_whitespace() {
            let f = flag.to_ascii_lowercase();
            let has = |parts: &Vec<String>, k: &str| {
                parts
                    .iter()
                    .skip(1)
                    .position(|p| p.to_ascii_lowercase().split('=').next() == Some(k))
            };
            match f.as_str() {
                "secure" | "httponly" => {
                    if has(&parts, &f).is_none() {
                        parts.push(if f == "secure" {
                            "Secure".into()
                        } else {
                            "HttpOnly".into()
                        });
                    }
                }
                "nosecure" | "nohttponly" | "nosamesite" => {
                    let k = &f[2..];
                    if let Some(i) = has(&parts, k) {
                        parts.remove(i + 1);
                    }
                }
                s if s.starts_with("samesite=") && s.len() > 9 => {
                    let v = &s[9..];
                    let cap = format!("SameSite={}{}", v[..1].to_ascii_uppercase(), &v[1..]);
                    match has(&parts, "samesite") {
                        Some(i) => parts[i + 1] = cap,
                        None => parts.push(cap),
                    }
                }
                _ => {}
            }
        }
    }
    parts.join("; ")
}

pub fn apply(headers: &mut HeaderMap, route: &RouteRule, target_url: &str) {
    if !route.proxy_redirect.is_empty() {
        for name in [header::LOCATION, header::REFRESH] {
            let Some(v) = headers.get(&name).and_then(|v| v.to_str().ok()) else {
                continue;
            };
            let new = if name == header::REFRESH {
                v.split_once("url=").and_then(|(head, url)| {
                    rewrite_location(url, route, target_url).map(|u| format!("{head}url={u}"))
                })
            } else {
                rewrite_location(v, route, target_url)
            };
            if let Some(Ok(hv)) = new.map(|n| HeaderValue::from_str(&n)) {
                headers.insert(name, hv);
            }
        }
    }
    if route.proxy_cookie_path.is_empty()
        && route.proxy_cookie_domain.is_empty()
        && route.proxy_cookie_flags.is_empty()
    {
        return;
    }
    let cookies: Vec<String> = headers
        .get_all(header::SET_COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok().map(String::from))
        .collect();
    if cookies.is_empty() {
        return;
    }
    headers.remove(header::SET_COOKIE);
    for c in cookies {
        if let Ok(v) = HeaderValue::from_str(&rewrite_cookie(&c, route)) {
            headers.append(header::SET_COOKIE, v);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn route() -> RouteRule {
        RouteRule {
            path_prefix: "/app/".into(),
            ..Default::default()
        }
    }

    #[test]
    fn redirects() {
        let mut r = route();
        r.proxy_redirect = vec![["default".into(), String::new()]];
        assert_eq!(
            rewrite_location("http://10.0.0.1:8080/login", &r, "http://10.0.0.1:8080").as_deref(),
            Some("/app/login")
        );
        r.proxy_redirect = vec![["http://internal/".into(), "https://ex.com/".into()]];
        assert_eq!(
            rewrite_location("http://internal/a", &r, "").as_deref(),
            Some("https://ex.com/a")
        );
        assert_eq!(rewrite_location("http://other/a", &r, ""), None);
        r.proxy_redirect = vec![[
            "~^http://([^/]+)/(.*)$".into(),
            "https://$1.ex.com/$2".into(),
        ]];
        assert_eq!(
            rewrite_location("http://api/x", &r, "").as_deref(),
            Some("https://api.ex.com/x")
        );
    }

    #[test]
    fn cookies() {
        let mut r = route();
        r.proxy_cookie_path = vec![["/".into(), "/app/".into()]];
        r.proxy_cookie_domain = vec![["internal.local".into(), "ex.com".into()]];
        r.proxy_cookie_flags = vec![["*".into(), "secure httponly samesite=strict".into()]];
        assert_eq!(
            rewrite_cookie("sid=1; Path=/x; Domain=.internal.local; SameSite=Lax", &r),
            "sid=1; Path=/app/x; Domain=ex.com; SameSite=Strict; Secure; HttpOnly"
        );
        r.proxy_cookie_flags = vec![["~^tmp".into(), "nosecure".into()]];
        assert_eq!(rewrite_cookie("tmp1=a; Secure", &r), "tmp1=a");
    }
}
