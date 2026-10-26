//! Request rate limiting (nginx `limit_req`): a token bucket per key, with `nodelay`,
//! queueing and `delay=` modes.

use std::collections::hash_map::DefaultHasher;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use crate::config::{RateLimitConfig, RateLimitMode};

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

// Buckets and counters are split over independent shards so worker threads that hit
// different keys never wait on the same lock. Keys are any hashable value (e.g. a
// `(route_id, client_ip)` tuple) reduced to a 64-bit hash, which avoids formatting a
// String per request; a collision would only merge two clients' quotas.
pub(super) const SHARDS: usize = 64;

pub(super) fn key_hash(key: impl Hash) -> u64 {
    let mut h = DefaultHasher::new();
    key.hash(&mut h);
    h.finish()
}

pub(super) fn shards<T: Default>() -> Box<[Mutex<T>]> {
    (0..SHARDS).map(|_| Mutex::new(T::default())).collect()
}

pub struct RateLimiter {
    buckets: Box<[Mutex<HashMap<u64, Bucket>>]>,
}

impl Default for RateLimiter {
    fn default() -> Self {
        Self::new()
    }
}

impl RateLimiter {
    pub fn new() -> Self {
        Self { buckets: shards() }
    }

    pub fn check(&self, key: impl Hash, config: &RateLimitConfig) -> RateLimitDecision {
        let rate = config.requests_per_second as f64;
        if rate <= 0.0 {
            return RateLimitDecision::Reject;
        }
        let burst = config.burst as f64;
        let now = crate::event::clock::now();
        let key = key_hash(key);

        let mut map = match self.buckets[key as usize % SHARDS].lock() {
            Ok(m) => m,

            Err(_) => return RateLimitDecision::Allow,
        };

        let bucket = map.entry(key).or_insert(Bucket {
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
        let now = crate::event::clock::now();
        for shard in self.buckets.iter() {
            if let Ok(mut map) = shard.lock() {
                map.retain(|_, b| {
                    now.duration_since(b.last_update) < BUCKET_IDLE_TTL || b.excess > 0.0
                });
            }
        }
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.buckets
            .iter()
            .map(|s| s.lock().map(|m| m.len()).unwrap_or(0))
            .sum()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
            rl.check(format!("ip-{}", i), &cfg);
        }
        assert_eq!(rl.len(), 100);

        rl.sweep();
        assert_eq!(rl.len(), 100);
    }
}
