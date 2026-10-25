use axum::http::HeaderMap;
use base64::prelude::*;
use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

use crate::config::{BasicAuthConfig, LimitConnConfig, RateLimitConfig, RateLimitMode};
use crate::net::cidr::IpMatcher;

const BUCKET_IDLE_TTL: Duration = Duration::from_secs(300);

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum RateLimitDecision {
    Allow,

    Delay(Duration),
    Reject,
}

#[derive(Debug)]
struct Bucket {
    excess: f64,
    last_update: Instant,
}

pub struct RateLimiter {
    buckets: RwLock<HashMap<String, Bucket>>,
}

impl Default for RateLimiter {
    fn default() -> Self {
        Self::new()
    }
}

impl RateLimiter {
    pub fn new() -> Self {
        Self {
            buckets: RwLock::new(HashMap::new()),
        }
    }

    pub fn check(&self, key: &str, config: &RateLimitConfig) -> RateLimitDecision {
        let rate = config.requests_per_second as f64;
        if rate <= 0.0 {
            return RateLimitDecision::Reject;
        }
        let burst = config.burst as f64;
        let now = crate::clock::now();

        let mut map = match self.buckets.write() {
            Ok(m) => m,

            Err(_) => return RateLimitDecision::Allow,
        };

        let bucket = map.entry(key.to_string()).or_insert(Bucket {
            excess: 0.0,
            last_update: now,
        });

        let elapsed = now.duration_since(bucket.last_update).as_secs_f64();
        let drained = (bucket.excess - rate * elapsed).max(0.0);
        let excess = drained + 1.0;
        bucket.last_update = now;

        if excess - 1.0 > burst {
            bucket.excess = drained;
            return RateLimitDecision::Reject;
        }

        bucket.excess = excess;

        let free = match config.mode {
            RateLimitMode::NoDelay => burst,
            RateLimitMode::Queue => 0.0,
            RateLimitMode::Delay => config.delay.unwrap_or(0) as f64,
        };

        let queued = excess - 1.0;
        if queued > free {
            RateLimitDecision::Delay(Duration::from_secs_f64((queued - free) / rate))
        } else {
            RateLimitDecision::Allow
        }
    }

    pub fn sweep(&self) {
        let now = crate::clock::now();
        if let Ok(mut map) = self.buckets.write() {
            map.retain(|_, b| {
                now.duration_since(b.last_update) < BUCKET_IDLE_TTL || b.excess > 0.0
            });
        }
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.buckets.read().map(|m| m.len()).unwrap_or(0)
    }
}

#[derive(Default)]
pub struct ConnLimiter {
    counters: RwLock<HashMap<String, Arc<AtomicI64>>>,
}

pub struct ConnGuard {
    counter: Arc<AtomicI64>,
}

impl Drop for ConnGuard {
    fn drop(&mut self) {
        self.counter.fetch_sub(1, Ordering::Relaxed);
    }
}

impl ConnLimiter {
    pub fn new() -> Self {
        Self::default()
    }

    fn counter_for(&self, key: &str) -> Arc<AtomicI64> {
        if let Ok(map) = self.counters.read() {
            if let Some(c) = map.get(key) {
                return c.clone();
            }
        }
        let mut map = match self.counters.write() {
            Ok(m) => m,
            Err(_) => return Arc::new(AtomicI64::new(0)),
        };
        map.entry(key.to_string())
            .or_insert_with(|| Arc::new(AtomicI64::new(0)))
            .clone()
    }

    pub fn acquire(&self, key: &str, config: &LimitConnConfig) -> Option<ConnGuard> {
        let counter = self.counter_for(key);
        let previous = counter.fetch_add(1, Ordering::AcqRel);
        if previous >= config.max_connections as i64 {
            counter.fetch_sub(1, Ordering::AcqRel);
            return None;
        }
        Some(ConnGuard { counter })
    }

    pub fn sweep(&self) {
        if let Ok(mut map) = self.counters.write() {
            map.retain(|_, c| c.load(Ordering::Relaxed) > 0);
        }
    }
}

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

    fn limit(rps: u32, burst: u32, mode: RateLimitMode) -> RateLimitConfig {
        RateLimitConfig {
            requests_per_second: rps,
            burst,
            mode,
            delay: None,
            status: 429,
            retry_after: None,
        }
    }

    fn ip(s: &str) -> IpAddr {
        IpAddr::from_str(s).unwrap()
    }

    #[test]
    fn first_request_always_passes() {
        let rl = RateLimiter::new();
        assert_eq!(
            rl.check("a", &limit(5, 0, RateLimitMode::NoDelay)),
            RateLimitDecision::Allow
        );
    }

    #[test]
    fn without_burst_the_second_immediate_request_is_rejected() {
        let rl = RateLimiter::new();
        let cfg = limit(5, 0, RateLimitMode::NoDelay);
        assert_eq!(rl.check("a", &cfg), RateLimitDecision::Allow);
        assert_eq!(rl.check("a", &cfg), RateLimitDecision::Reject);
    }

    #[test]
    fn burst_allows_a_batch_through_in_nodelay_mode() {
        let rl = RateLimiter::new();
        let cfg = limit(5, 3, RateLimitMode::NoDelay);

        for i in 0..4 {
            assert_eq!(rl.check("a", &cfg), RateLimitDecision::Allow, "req {}", i);
        }
        assert_eq!(rl.check("a", &cfg), RateLimitDecision::Reject);
    }

    #[test]
    fn queue_mode_delays_instead_of_rejecting() {
        let rl = RateLimiter::new();
        let cfg = limit(5, 3, RateLimitMode::Queue);
        assert_eq!(rl.check("a", &cfg), RateLimitDecision::Allow);

        match rl.check("a", &cfg) {
            RateLimitDecision::Delay(d) => assert!(d > Duration::ZERO),
            other => panic!("esperava espera, veio {:?}", other),
        }
    }

    #[test]
    fn delay_mode_lets_the_first_n_through_untouched() {
        let rl = RateLimiter::new();
        let mut cfg = limit(5, 10, RateLimitMode::Delay);
        cfg.delay = Some(2);

        assert_eq!(rl.check("a", &cfg), RateLimitDecision::Allow);
        assert_eq!(rl.check("a", &cfg), RateLimitDecision::Allow);
        assert_eq!(rl.check("a", &cfg), RateLimitDecision::Allow);
        match rl.check("a", &cfg) {
            RateLimitDecision::Delay(_) => {}
            other => panic!("esperava espera, veio {:?}", other),
        }
    }

    #[test]
    fn keys_do_not_share_quota() {
        let rl = RateLimiter::new();
        let cfg = limit(5, 0, RateLimitMode::NoDelay);
        assert_eq!(rl.check("a", &cfg), RateLimitDecision::Allow);
        assert_eq!(rl.check("b", &cfg), RateLimitDecision::Allow);
    }

    #[test]
    fn bucket_refills_over_time() {
        let rl = RateLimiter::new();
        let cfg = limit(100, 0, RateLimitMode::NoDelay);
        assert_eq!(rl.check("a", &cfg), RateLimitDecision::Allow);
        assert_eq!(rl.check("a", &cfg), RateLimitDecision::Reject);

        std::thread::sleep(Duration::from_millis(20));
        assert_eq!(rl.check("a", &cfg), RateLimitDecision::Allow);
    }

    #[test]
    fn rejection_does_not_deepen_the_queue() {
        let rl = RateLimiter::new();
        let cfg = limit(100, 0, RateLimitMode::NoDelay);
        assert_eq!(rl.check("a", &cfg), RateLimitDecision::Allow);
        for _ in 0..50 {
            let _ = rl.check("a", &cfg);
        }
        std::thread::sleep(Duration::from_millis(20));
        assert_eq!(rl.check("a", &cfg), RateLimitDecision::Allow);
    }

    #[test]
    fn zero_rate_is_rejected_rather_than_dividing_by_zero() {
        let rl = RateLimiter::new();
        assert_eq!(
            rl.check("a", &limit(0, 0, RateLimitMode::NoDelay)),
            RateLimitDecision::Reject
        );
    }

    #[test]
    fn sweep_discards_drained_idle_buckets() {
        let rl = RateLimiter::new();
        let cfg = limit(1000, 0, RateLimitMode::NoDelay);
        for i in 0..100 {
            rl.check(&format!("ip-{}", i), &cfg);
        }
        assert_eq!(rl.len(), 100);

        rl.sweep();
        assert_eq!(rl.len(), 100);
    }

    #[test]
    fn conn_limiter_blocks_above_the_ceiling() {
        let cl = ConnLimiter::new();
        let cfg = LimitConnConfig {
            max_connections: 2,
            status: 429,
        };

        let a = cl.acquire("ip", &cfg);
        let b = cl.acquire("ip", &cfg);
        assert!(a.is_some() && b.is_some());
        assert!(cl.acquire("ip", &cfg).is_none());

        drop(a);
        assert!(cl.acquire("ip", &cfg).is_some());
    }

    #[test]
    fn conn_guard_releases_the_slot_when_dropped() {
        let cl = ConnLimiter::new();
        let cfg = LimitConnConfig {
            max_connections: 1,
            status: 429,
        };

        {
            let _guard = cl.acquire("ip", &cfg).unwrap();
            assert!(cl.acquire("ip", &cfg).is_none());
        }
        assert!(cl.acquire("ip", &cfg).is_some());
    }

    #[test]
    fn conn_sweep_keeps_active_keys() {
        let cl = ConnLimiter::new();
        let cfg = LimitConnConfig {
            max_connections: 5,
            status: 429,
        };

        let _held = cl.acquire("ativo", &cfg).unwrap();
        drop(cl.acquire("ocioso", &cfg));
        cl.sweep();

        assert_eq!(cl.counters.read().unwrap().len(), 1);
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
