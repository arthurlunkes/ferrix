//! Concurrent connection limiting (nginx `limit_conn`): a counter per key, released
//! when the request finishes.

use std::collections::HashMap;
use std::hash::Hash;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex};

use super::limit_req::{key_hash, shards, SHARDS};
use crate::config::LimitConnConfig;

type CounterShard = Mutex<HashMap<u64, Arc<AtomicI64>>>;

pub struct ConnLimiter {
    counters: Box<[CounterShard]>,
}

impl Default for ConnLimiter {
    fn default() -> Self {
        Self { counters: shards() }
    }
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

    fn counter_for(&self, key: u64) -> Arc<AtomicI64> {
        match self.counters[key as usize % SHARDS].lock() {
            Ok(mut map) => map
                .entry(key)
                .or_insert_with(|| Arc::new(AtomicI64::new(0)))
                .clone(),
            Err(_) => Arc::new(AtomicI64::new(0)),
        }
    }

    pub fn acquire(&self, key: impl Hash, config: &LimitConnConfig) -> Option<ConnGuard> {
        let counter = self.counter_for(key_hash(key));
        let previous = counter.fetch_add(1, Ordering::AcqRel);
        if previous >= config.max_connections as i64 {
            counter.fetch_sub(1, Ordering::AcqRel);
            return None;
        }
        Some(ConnGuard { counter })
    }

    pub fn sweep(&self) {
        for shard in self.counters.iter() {
            if let Ok(mut map) = shard.lock() {
                map.retain(|_, c| c.load(Ordering::Relaxed) > 0);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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

        let live: usize = cl.counters.iter().map(|s| s.lock().unwrap().len()).sum();
        assert_eq!(live, 1);
    }
}
