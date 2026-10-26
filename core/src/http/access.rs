//! Access control (nginx `access` + `auth_basic`): IP allow/deny lists, HTTP Basic
//! authentication and how they combine (`satisfy all | any`).

use axum::http::HeaderMap;
use base64::prelude::*;
use std::net::IpAddr;

use crate::config::BasicAuthConfig;
use crate::net::cidr::IpMatcher;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccessDecision {
    Allow,

    Forbidden,

    Unauthorized,
}

pub struct AccessControl;

impl AccessControl {
    pub fn is_ip_allowed(client_ip: IpAddr, allowed: &IpMatcher, denied: &IpMatcher) -> bool {
        if denied.matches(client_ip) {
            return false;
        }
        if !allowed.is_empty() {
            return allowed.matches(client_ip);
        }
        true
    }

    pub fn verify_basic_auth(headers: &HeaderMap, auth: &BasicAuthConfig) -> bool {
        let Some(value) = headers.get("authorization").and_then(|v| v.to_str().ok()) else {
            return false;
        };
        let Some(encoded) = value
            .strip_prefix("Basic ")
            .or_else(|| value.strip_prefix("basic "))
        else {
            return false;
        };
        let Ok(decoded) = BASE64_STANDARD.decode(encoded.trim()) else {
            return false;
        };
        let Ok(text) = String::from_utf8(decoded) else {
            return false;
        };
        let Some((user, pass)) = text.split_once(':') else {
            return false;
        };
        user == auth.username && pass == auth.password
    }

    pub fn evaluate(
        satisfy: crate::config::Satisfy,
        ip_ok: bool,
        basic_auth: Option<&BasicAuthConfig>,
        headers: &HeaderMap,
    ) -> AccessDecision {
        use crate::config::Satisfy;

        let Some(auth) = basic_auth else {
            return if ip_ok {
                AccessDecision::Allow
            } else {
                AccessDecision::Forbidden
            };
        };

        let auth_ok = Self::verify_basic_auth(headers, auth);

        match satisfy {
            Satisfy::All => {
                if !ip_ok {
                    AccessDecision::Forbidden
                } else if !auth_ok {
                    AccessDecision::Unauthorized
                } else {
                    AccessDecision::Allow
                }
            }
            Satisfy::Any => {
                if ip_ok || auth_ok {
                    AccessDecision::Allow
                } else {
                    AccessDecision::Unauthorized
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Satisfy;
    use std::str::FromStr;

    fn ip(s: &str) -> IpAddr {
        IpAddr::from_str(s).unwrap()
    }

    fn matcher(entries: &[&str]) -> IpMatcher {
        IpMatcher::compile(&entries.iter().map(|s| s.to_string()).collect::<Vec<_>>()).0
    }

    #[test]
    fn cidr_deny_now_actually_blocks() {
        let denied = matcher(&["172.31.0.0/16"]);
        let allowed = matcher(&[]);
        assert!(!AccessControl::is_ip_allowed(
            ip("172.31.5.9"),
            &allowed,
            &denied
        ));
        assert!(AccessControl::is_ip_allowed(
            ip("10.0.0.1"),
            &allowed,
            &denied
        ));
    }

    #[test]
    fn deny_is_evaluated_before_allow() {
        let allowed = matcher(&["172.31.0.0/16"]);
        let denied = matcher(&["172.31.0.1"]);
        assert!(!AccessControl::is_ip_allowed(
            ip("172.31.0.1"),
            &allowed,
            &denied
        ));
        assert!(AccessControl::is_ip_allowed(
            ip("172.31.0.2"),
            &allowed,
            &denied
        ));
    }

    #[test]
    fn non_empty_allow_list_excludes_everything_else() {
        let allowed = matcher(&["10.0.0.0/8"]);
        let denied = matcher(&[]);
        assert!(AccessControl::is_ip_allowed(
            ip("10.1.1.1"),
            &allowed,
            &denied
        ));
        assert!(!AccessControl::is_ip_allowed(
            ip("192.168.0.1"),
            &allowed,
            &denied
        ));
    }

    #[test]
    fn empty_lists_allow_everyone() {
        let empty = matcher(&[]);
        assert!(AccessControl::is_ip_allowed(ip("8.8.8.8"), &empty, &empty));
    }

    fn auth_header(user: &str, pass: &str) -> HeaderMap {
        let token = BASE64_STANDARD.encode(format!("{}:{}", user, pass));
        let mut h = HeaderMap::new();
        h.insert("authorization", format!("Basic {}", token).parse().unwrap());
        h
    }

    #[test]
    fn basic_auth_accepts_correct_credentials() {
        let cfg = BasicAuthConfig {
            username: "admin".into(),
            password: "s3nha".into(),
        };
        assert!(AccessControl::verify_basic_auth(
            &auth_header("admin", "s3nha"),
            &cfg
        ));
        assert!(!AccessControl::verify_basic_auth(
            &auth_header("admin", "errada"),
            &cfg
        ));
        assert!(!AccessControl::verify_basic_auth(&HeaderMap::new(), &cfg));
    }

    #[test]
    fn basic_auth_accepts_password_containing_colon() {
        let cfg = BasicAuthConfig {
            username: "admin".into(),
            password: "a:b:c".into(),
        };
        assert!(AccessControl::verify_basic_auth(
            &auth_header("admin", "a:b:c"),
            &cfg
        ));
    }

    #[test]
    fn satisfy_all_requires_both_checks() {
        let cfg = BasicAuthConfig {
            username: "u".into(),
            password: "p".into(),
        };
        let good = auth_header("u", "p");

        assert_eq!(
            AccessControl::evaluate(Satisfy::All, true, Some(&cfg), &good),
            AccessDecision::Allow
        );
        assert_eq!(
            AccessControl::evaluate(Satisfy::All, false, Some(&cfg), &good),
            AccessDecision::Forbidden
        );
        assert_eq!(
            AccessControl::evaluate(Satisfy::All, true, Some(&cfg), &HeaderMap::new()),
            AccessDecision::Unauthorized
        );
    }

    #[test]
    fn satisfy_any_accepts_either_check() {
        let cfg = BasicAuthConfig {
            username: "u".into(),
            password: "p".into(),
        };

        assert_eq!(
            AccessControl::evaluate(Satisfy::Any, true, Some(&cfg), &HeaderMap::new()),
            AccessDecision::Allow
        );

        assert_eq!(
            AccessControl::evaluate(Satisfy::Any, false, Some(&cfg), &auth_header("u", "p")),
            AccessDecision::Allow
        );

        assert_eq!(
            AccessControl::evaluate(Satisfy::Any, false, Some(&cfg), &HeaderMap::new()),
            AccessDecision::Unauthorized
        );
    }

    #[test]
    fn without_basic_auth_only_the_acl_decides() {
        assert_eq!(
            AccessControl::evaluate(Satisfy::All, true, None, &HeaderMap::new()),
            AccessDecision::Allow
        );
        assert_eq!(
            AccessControl::evaluate(Satisfy::Any, false, None, &HeaderMap::new()),
            AccessDecision::Forbidden
        );
    }
}
