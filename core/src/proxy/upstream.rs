use std::collections::hash_map::DefaultHasher;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::net::IpAddr;
use std::sync::atomic::{AtomicI64, AtomicUsize, Ordering};
use std::sync::{Arc, RwLock};

use crate::config::{LoadBalanceAlgorithm, UpstreamConfig, UpstreamTarget};
use crate::health::HealthChecker;
use crate::proxy::hashring::HashRing;

pub struct UpstreamPoolManager {
    rr_indices: RwLock<HashMap<String, AtomicUsize>>,

    active_connections: RwLock<HashMap<String, Arc<AtomicI64>>>,
    health_checker: Arc<HealthChecker>,
}

impl UpstreamPoolManager {
    pub fn new(health_checker: Arc<HealthChecker>) -> Self {
        Self {
            rr_indices: RwLock::new(HashMap::new()),
            active_connections: RwLock::new(HashMap::new()),
            health_checker,
        }
    }

    fn get_target_conn_counter(&self, target_url: &str) -> Arc<AtomicI64> {
        if let Ok(map) = self.active_connections.read() {
            if let Some(counter) = map.get(target_url) {
                return counter.clone();
            }
        }
        let mut map = match self.active_connections.write() {
            Ok(m) => m,
            Err(_) => return Arc::new(AtomicI64::new(0)),
        };
        map.entry(target_url.to_string())
            .or_insert_with(|| Arc::new(AtomicI64::new(0)))
            .clone()
    }

    pub fn inc_conn(&self, target_url: &str) {
        self.get_target_conn_counter(target_url)
            .fetch_add(1, Ordering::Relaxed);
    }

    pub fn dec_conn(&self, target_url: &str) {
        self.get_target_conn_counter(target_url)
            .fetch_sub(1, Ordering::Relaxed);
    }

    pub fn active_conns(&self, target_url: &str) -> i64 {
        self.get_target_conn_counter(target_url)
            .load(Ordering::Relaxed)
    }

    fn is_available(&self, target: &UpstreamTarget) -> bool {
        if !self.health_checker.is_healthy(&target.url) {
            return false;
        }
        match target.max_conns {
            Some(max) if max > 0 => self.active_conns(&target.url) < max as i64,
            _ => true,
        }
    }

    /// Failover order for `consistent_hash` upstreams: ring order from the key's owner,
    /// skipping unavailable servers, then the backups.
    pub fn select_targets_hashed<'a>(
        &self,
        upstream: &'a UpstreamConfig,
        ring: &HashRing,
        hash: u64,
    ) -> Vec<&'a UpstreamTarget> {
        let usable = |t: &&UpstreamTarget| !t.down && self.is_available(t);
        let mut out: Vec<&UpstreamTarget> = ring
            .order(hash)
            .into_iter()
            .filter_map(|i| upstream.targets.get(i))
            .filter(usable)
            .collect();
        out.extend(upstream.targets.iter().filter(|t| t.backup).filter(usable));
        if out.is_empty() {
            // Everything looks unavailable: still try the key's owner, like the other
            // algorithms do as a last resort.
            out.extend(
                ring.order(hash)
                    .into_iter()
                    .filter_map(|i| upstream.targets.get(i))
                    .filter(|t| !t.down),
            );
        }
        out
    }

    pub fn select_targets<'a>(
        &self,
        upstream: &'a UpstreamConfig,
        client_ip: IpAddr,
    ) -> Vec<&'a UpstreamTarget> {
        let live = || upstream.targets.iter().filter(|t| !t.down);
        if live().next().is_none() {
            return Vec::new();
        }

        let primary: Vec<&UpstreamTarget> = live()
            .filter(|t| !t.backup && self.is_available(t))
            .collect();

        let backups: Vec<&UpstreamTarget> = live()
            .filter(|t| t.backup && self.is_available(t))
            .collect();

        if !primary.is_empty() {
            let mut ordered = self.order_by_algorithm(upstream, primary, client_ip);

            ordered.extend(backups);
            return ordered;
        }

        if !backups.is_empty() {
            return self.order_by_algorithm(upstream, backups, client_ip);
        }

        self.order_by_algorithm(upstream, live().collect(), client_ip)
    }

    fn next_rr_index(&self, pool_name: &str, len: usize) -> usize {
        if len == 0 {
            return 0;
        }

        if let Ok(map) = self.rr_indices.read() {
            if let Some(counter) = map.get(pool_name) {
                return counter.fetch_add(1, Ordering::Relaxed) % len;
            }
        }
        let mut map = match self.rr_indices.write() {
            Ok(m) => m,
            Err(_) => return 0,
        };
        let counter = map
            .entry(pool_name.to_string())
            .or_insert_with(|| AtomicUsize::new(0));
        counter.fetch_add(1, Ordering::Relaxed) % len
    }

    fn order_by_algorithm<'a>(
        &self,
        upstream: &UpstreamConfig,
        mut pool: Vec<&'a UpstreamTarget>,
        client_ip: IpAddr,
    ) -> Vec<&'a UpstreamTarget> {
        if pool.len() <= 1 {
            return pool;
        }

        match upstream.algorithm {
            LoadBalanceAlgorithm::RoundRobin => {
                let idx = self.next_rr_index(&upstream.name, pool.len());
                pool.rotate_left(idx);
                pool
            }
            LoadBalanceAlgorithm::WeightedRoundRobin => {
                let total: usize = pool.iter().map(|t| t.weight.max(1) as usize).sum();
                let mut pick = self.next_rr_index(&upstream.name, total);
                let mut idx = 0;
                for (i, t) in pool.iter().enumerate() {
                    let w = t.weight.max(1) as usize;
                    if pick < w {
                        idx = i;
                        break;
                    }
                    pick -= w;
                }
                let primary = pool.remove(idx);
                pool.insert(0, primary);
                pool
            }
            LoadBalanceAlgorithm::LeastConn => {
                pool.sort_by_cached_key(|t| self.active_conns(&t.url));
                pool
            }
            // Without a ring (e.g. route tester) fall back to hashing the client address.
            LoadBalanceAlgorithm::IpHash | LoadBalanceAlgorithm::ConsistentHash => {
                let mut hasher = DefaultHasher::new();
                client_ip.hash(&mut hasher);
                let idx = (hasher.finish() as usize) % pool.len();
                pool.rotate_left(idx);
                pool
            }
            LoadBalanceAlgorithm::Random => {
                let nanos = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .subsec_nanos() as usize;
                let idx = nanos % pool.len();
                pool.rotate_left(idx);
                pool
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn target(url: &str) -> UpstreamTarget {
        UpstreamTarget {
            url: url.to_string(),
            weight: 1,
            max_fails: 3,
            fail_timeout_secs: 10,
            backup: false,
            down: false,
            max_conns: None,
        }
    }

    fn pool(name: &str, targets: Vec<UpstreamTarget>) -> UpstreamConfig {
        UpstreamConfig {
            name: name.to_string(),
            algorithm: LoadBalanceAlgorithm::RoundRobin,
            targets,
            health_check: None,
            verify_tls: true,
            hash_key: None,
        }
    }

    fn manager() -> UpstreamPoolManager {
        UpstreamPoolManager::new(Arc::new(HealthChecker::new()))
    }

    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    fn urls<'a>(targets: &[&'a UpstreamTarget]) -> Vec<&'a str> {
        targets.iter().map(|t| t.url.as_str()).collect()
    }

    #[test]
    fn round_robin_advances_each_call() {
        let m = manager();
        let p = pool("p", vec![target("a"), target("b"), target("c")]);

        assert_eq!(m.select_targets(&p, ip("1.1.1.1"))[0].url, "a");
        assert_eq!(m.select_targets(&p, ip("1.1.1.1"))[0].url, "b");
        assert_eq!(m.select_targets(&p, ip("1.1.1.1"))[0].url, "c");
        assert_eq!(m.select_targets(&p, ip("1.1.1.1"))[0].url, "a");
    }

    #[test]
    fn every_candidate_is_offered_for_failover() {
        let m = manager();
        let p = pool("p", vec![target("a"), target("b"), target("c")]);
        let selected = m.select_targets(&p, ip("1.1.1.1"));

        assert_eq!(selected.len(), 3);
        assert_eq!(urls(&selected), vec!["a", "b", "c"]);
    }

    #[test]
    fn down_targets_are_excluded() {
        let m = manager();
        let mut fora = target("b");
        fora.down = true;
        let p = pool("p", vec![target("a"), fora, target("c")]);

        let selected = m.select_targets(&p, ip("1.1.1.1"));
        assert_eq!(selected.len(), 2);
        assert!(!urls(&selected).contains(&"b"));
    }

    #[test]
    fn all_down_yields_no_candidates() {
        let m = manager();
        let mut a = target("a");
        a.down = true;
        let p = pool("p", vec![a]);

        assert!(m.select_targets(&p, ip("1.1.1.1")).is_empty());
    }

    #[test]
    fn backup_is_only_reached_after_the_regular_servers() {
        let m = manager();
        let mut reserva = target("reserva");
        reserva.backup = true;
        let p = pool("p", vec![target("a"), reserva]);

        let selected = m.select_targets(&p, ip("1.1.1.1"));
        assert_eq!(urls(&selected), vec!["a", "reserva"]);
    }

    #[test]
    fn backup_takes_over_when_every_regular_server_is_out() {
        let m = manager();
        let mut principal = target("a");
        principal.down = true;
        let mut reserva = target("reserva");
        reserva.backup = true;
        let p = pool("p", vec![principal, reserva]);

        let selected = m.select_targets(&p, ip("1.1.1.1"));
        assert_eq!(urls(&selected), vec!["reserva"]);
    }

    #[test]
    fn max_conns_takes_a_saturated_server_out_of_rotation() {
        let m = manager();
        let mut limitado = target("limitado");
        limitado.max_conns = Some(1);
        let p = pool("p", vec![limitado, target("livre")]);

        m.inc_conn("limitado");
        let selected = m.select_targets(&p, ip("1.1.1.1"));
        assert_eq!(urls(&selected), vec!["livre"]);

        m.dec_conn("limitado");
        assert_eq!(m.select_targets(&p, ip("1.1.1.1")).len(), 2);
    }

    #[test]
    fn saturated_pool_still_gets_tried_as_a_last_resort() {
        let m = manager();
        let mut limitado = target("unico");
        limitado.max_conns = Some(1);
        let p = pool("p", vec![limitado]);

        m.inc_conn("unico");
        assert_eq!(urls(&m.select_targets(&p, ip("1.1.1.1"))), vec!["unico"]);
    }

    #[test]
    fn ip_hash_is_stable_for_the_same_client() {
        let m = manager();
        let mut p = pool("p", vec![target("a"), target("b"), target("c")]);
        p.algorithm = LoadBalanceAlgorithm::IpHash;

        let first = m.select_targets(&p, ip("10.0.0.1"))[0].url.clone();
        for _ in 0..10 {
            assert_eq!(m.select_targets(&p, ip("10.0.0.1"))[0].url, first);
        }
    }

    #[test]
    fn ip_hash_separates_different_clients() {
        let m = manager();
        let mut p = pool("p", vec![target("a"), target("b"), target("c")]);
        p.algorithm = LoadBalanceAlgorithm::IpHash;

        let chosen: std::collections::HashSet<String> = (0..40)
            .map(|i| {
                m.select_targets(&p, ip(&format!("10.0.0.{}", i)))[0]
                    .url
                    .clone()
            })
            .collect();

        assert!(chosen.len() > 1, "o hash deveria espalhar entre servidores");
    }

    #[test]
    fn least_conn_prefers_the_idle_server() {
        let m = manager();
        let mut p = pool("p", vec![target("ocupado"), target("ocioso")]);
        p.algorithm = LoadBalanceAlgorithm::LeastConn;

        m.inc_conn("ocupado");
        m.inc_conn("ocupado");
        assert_eq!(m.select_targets(&p, ip("1.1.1.1"))[0].url, "ocioso");
    }

    #[test]
    fn weighted_round_robin_favours_the_heavier_server() {
        let m = manager();
        let mut pesado = target("pesado");
        pesado.weight = 3;
        let mut p = pool("p", vec![pesado, target("leve")]);
        p.algorithm = LoadBalanceAlgorithm::WeightedRoundRobin;

        let mut pesado_count = 0;
        for _ in 0..40 {
            if m.select_targets(&p, ip("1.1.1.1"))[0].url == "pesado" {
                pesado_count += 1;
            }
        }

        assert!(
            pesado_count > 22,
            "esperava predominância do peso 3, veio {}",
            pesado_count
        );
    }

    #[test]
    fn weighted_still_offers_every_server_for_failover() {
        let m = manager();
        let mut pesado = target("pesado");
        pesado.weight = 5;
        let mut p = pool("p", vec![pesado, target("leve")]);
        p.algorithm = LoadBalanceAlgorithm::WeightedRoundRobin;

        let selected = m.select_targets(&p, ip("1.1.1.1"));
        assert_eq!(selected.len(), 2);
    }

    #[test]
    fn empty_pool_yields_nothing() {
        let m = manager();
        assert!(m
            .select_targets(&pool("p", vec![]), ip("1.1.1.1"))
            .is_empty());
    }

    #[test]
    fn single_target_pool_is_returned_as_is() {
        let m = manager();
        let p = pool("p", vec![target("unico")]);
        assert_eq!(urls(&m.select_targets(&p, ip("1.1.1.1"))), vec!["unico"]);
    }

    #[test]
    fn connection_counter_tracks_up_and_down() {
        let m = manager();
        assert_eq!(m.active_conns("x"), 0);
        m.inc_conn("x");
        m.inc_conn("x");
        assert_eq!(m.active_conns("x"), 2);
        m.dec_conn("x");
        assert_eq!(m.active_conns("x"), 1);
    }
}
