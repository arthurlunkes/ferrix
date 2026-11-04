use base64::Engine;
use md5::{Digest, Md5};
use std::net::IpAddr;

use crate::config::SecureLinkConfig;

pub enum Check {
    Ok,
    Invalid,
    Expired,
}

fn arg<'a>(query: &'a str, name: &str) -> Option<&'a str> {
    query
        .split('&')
        .filter_map(|kv| kv.split_once('=').or(Some((kv, ""))))
        .find(|(k, _)| *k == name)
        .map(|(_, v)| v)
}

fn expand(template: &str, expires: &str, path: &str, ip: IpAddr, host: &str, q: &str) -> String {
    let mut out = String::new();
    let mut rest = template;
    while let Some(i) = rest.find('$') {
        out.push_str(&rest[..i]);
        let after = &rest[i + 1..];
        let e = after
            .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
            .unwrap_or(after.len());
        let name = &after[..e];
        match name {
            "secure_link_expires" => out.push_str(expires),
            "uri" => out.push_str(path),
            "remote_addr" => out.push_str(&ip.to_string()),
            "host" => out.push_str(host),
            n if n.starts_with("arg_") => out.push_str(arg(q, &n[4..]).unwrap_or("")),
            n => {
                out.push('$');
                out.push_str(n);
            }
        }
        rest = &after[e..];
    }
    out.push_str(rest);
    out
}

pub fn hash(input: &str) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(Md5::digest(input.as_bytes()))
}

pub fn check(cfg: &SecureLinkConfig, uri: &axum::http::Uri, ip: IpAddr, host: &str) -> Check {
    let q = uri.query().unwrap_or("");
    let Some(given) = arg(q, cfg.hash_arg()) else {
        return Check::Invalid;
    };
    let expires = arg(q, cfg.expires_arg()).unwrap_or("");
    let expected = hash(&expand(&cfg.md5, expires, uri.path(), ip, host, q));
    if given != expected {
        return Check::Invalid;
    }
    if !expires.is_empty() {
        let Ok(t) = expires.parse::<i64>() else {
            return Check::Invalid;
        };
        if t < chrono::Utc::now().timestamp() {
            return Check::Expired;
        }
    }
    Check::Ok
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nginx_docs_example() {
        assert_eq!(
            hash("2147483647/s/link127.0.0.1 secret"),
            "_e4Nc3iduzkWRm01TBBNYw"
        );
        let cfg = SecureLinkConfig {
            md5: "$secure_link_expires$uri$remote_addr secret".into(),
            ..Default::default()
        };
        let ip: IpAddr = "127.0.0.1".parse().unwrap();
        let ok: axum::http::Uri = "/s/link?md5=_e4Nc3iduzkWRm01TBBNYw&expires=2147483647"
            .parse()
            .unwrap();
        assert!(matches!(check(&cfg, &ok, ip, ""), Check::Ok));
        let bad: axum::http::Uri = "/s/link?md5=x&expires=2147483647".parse().unwrap();
        assert!(matches!(check(&cfg, &bad, ip, ""), Check::Invalid));
        let past = hash("1/s/link127.0.0.1 secret");
        let old: axum::http::Uri = format!("/s/link?md5={past}&expires=1").parse().unwrap();
        assert!(matches!(check(&cfg, &old, ip, ""), Check::Expired));
    }
}
