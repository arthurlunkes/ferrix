use std::collections::hash_map::DefaultHasher;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::sync::atomic::{AtomicI64, AtomicUsize, Ordering};
use std::sync::{Arc, RwLock};

use crate::config::{LoadBalanceAlgorithm, UpstreamConfig, UpstreamTarget};
use crate::health::HealthChecker;

pub struct UpstreamPoolManager {
    /// Posição do rodízio por pool.
    rr_indices: RwLock<HashMap<String, AtomicUsize>>,
    /// Conexões em curso por servidor, base do `least_conn` e do `max_conns`.
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

    /// Um servidor só entra na rotação se estiver saudável e abaixo do seu `max_conns`.
    fn is_available(&self, target: &UpstreamTarget) -> bool {
        if !self.health_checker.is_healthy(&target.url) {
            return false;
        }
        match target.max_conns {
            Some(max) if max > 0 => self.active_conns(&target.url) < max as i64,
            _ => true,
        }
    }

    /// Monta a lista ordenada de candidatos: o primeiro é para onde a requisição vai, e
    /// os seguintes são o caminho do failover.
    ///
    /// A ordem segue o nginx: servidores `down` ficam de fora; os `backup` só entram
    /// quando nenhum servidor comum está disponível; e se nada estiver saudável, o pool
    /// inteiro é tentado assim mesmo, para não transformar uma janela de health check
    /// ruim em indisponibilidade total.
    pub fn select_targets(
        &self,
        upstream: &UpstreamConfig,
        client_ip: &str,
    ) -> Vec<UpstreamTarget> {
        let live: Vec<&UpstreamTarget> = upstream.targets.iter().filter(|t| !t.down).collect();
        if live.is_empty() {
            return Vec::new();
        }

        let primary: Vec<UpstreamTarget> = live
            .iter()
            .filter(|t| !t.backup && self.is_available(t))
            .map(|t| (*t).clone())
            .collect();

        let backups: Vec<UpstreamTarget> = live
            .iter()
            .filter(|t| t.backup && self.is_available(t))
            .map(|t| (*t).clone())
            .collect();

        if !primary.is_empty() {
            let mut ordered = self.order_by_algorithm(upstream, primary, client_ip);
            // Os backups ficam no fim: só são alcançados se todos os comuns falharem
            // durante esta requisição.
            ordered.extend(backups);
            return ordered;
        }

        if !backups.is_empty() {
            return self.order_by_algorithm(upstream, backups, client_ip);
        }

        // Último recurso: ninguém passou no filtro de saúde ou de capacidade. Tentar
        // mesmo assim é melhor que devolver 502 sem sequer bater na porta.
        let fallback: Vec<UpstreamTarget> = live.iter().map(|t| (*t).clone()).collect();
        self.order_by_algorithm(upstream, fallback, client_ip)
    }

    fn next_rr_index(&self, pool_name: &str, len: usize) -> usize {
        if len == 0 {
            return 0;
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

    fn order_by_algorithm(
        &self,
        upstream: &UpstreamConfig,
        pool: Vec<UpstreamTarget>,
        client_ip: &str,
    ) -> Vec<UpstreamTarget> {
        if pool.len() <= 1 {
            return pool;
        }

        match upstream.algorithm {
            LoadBalanceAlgorithm::RoundRobin => {
                let idx = self.next_rr_index(&upstream.name, pool.len());
                rotate(pool, idx)
            }
            LoadBalanceAlgorithm::WeightedRoundRobin => {
                let mut weighted: Vec<&UpstreamTarget> = Vec::new();
                for t in &pool {
                    for _ in 0..t.weight.max(1) {
                        weighted.push(t);
                    }
                }
                if weighted.is_empty() {
                    return pool;
                }
                let idx = self.next_rr_index(&upstream.name, weighted.len());
                let primary = weighted[idx].clone();

                let mut ordered = vec![primary.clone()];
                ordered.extend(pool.into_iter().filter(|t| t.url != primary.url));
                ordered
            }
            LoadBalanceAlgorithm::LeastConn => {
                let mut with_conns: Vec<(UpstreamTarget, i64)> = pool
                    .into_iter()
                    .map(|t| {
                        let conns = self.active_conns(&t.url);
                        (t, conns)
                    })
                    .collect();
                with_conns.sort_by_key(|(_, c)| *c);
                with_conns.into_iter().map(|(t, _)| t).collect()
            }
            LoadBalanceAlgorithm::IpHash => {
                let mut hasher = DefaultHasher::new();
                client_ip.hash(&mut hasher);
                let idx = (hasher.finish() as usize) % pool.len();
                rotate(pool, idx)
            }
            LoadBalanceAlgorithm::Random => {
                let nanos = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .subsec_nanos() as usize;
                let idx = nanos % pool.len();
                rotate(pool, idx)
            }
        }
    }
}

/// Gira a lista para que `start` fique na frente, preservando a ordem cíclica dos demais.
fn rotate(pool: Vec<UpstreamTarget>, start: usize) -> Vec<UpstreamTarget> {
    let len = pool.len();
    (0..len).map(|i| pool[(start + i) % len].clone()).collect()
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
        }
    }

    fn manager() -> UpstreamPoolManager {
        UpstreamPoolManager::new(Arc::new(HealthChecker::new()))
    }

    fn urls(targets: &[UpstreamTarget]) -> Vec<&str> {
        targets.iter().map(|t| t.url.as_str()).collect()
    }

    #[test]
    fn round_robin_advances_each_call() {
        let m = manager();
        let p = pool("p", vec![target("a"), target("b"), target("c")]);

        assert_eq!(m.select_targets(&p, "1.1.1.1")[0].url, "a");
        assert_eq!(m.select_targets(&p, "1.1.1.1")[0].url, "b");
        assert_eq!(m.select_targets(&p, "1.1.1.1")[0].url, "c");
        assert_eq!(m.select_targets(&p, "1.1.1.1")[0].url, "a");
    }

    #[test]
    fn every_candidate_is_offered_for_failover() {
        let m = manager();
        let p = pool("p", vec![target("a"), target("b"), target("c")]);
        let selected = m.select_targets(&p, "1.1.1.1");

        assert_eq!(selected.len(), 3);
        assert_eq!(urls(&selected), vec!["a", "b", "c"]);
    }

    #[test]
    fn down_targets_are_excluded() {
        let m = manager();
        let mut fora = target("b");
        fora.down = true;
        let p = pool("p", vec![target("a"), fora, target("c")]);

        let selected = m.select_targets(&p, "1.1.1.1");
        assert_eq!(selected.len(), 2);
        assert!(!urls(&selected).contains(&"b"));
    }

    #[test]
    fn all_down_yields_no_candidates() {
        let m = manager();
        let mut a = target("a");
        a.down = true;
        let p = pool("p", vec![a]);

        assert!(m.select_targets(&p, "1.1.1.1").is_empty());
    }

    #[test]
    fn backup_is_only_reached_after_the_regular_servers() {
        let m = manager();
        let mut reserva = target("reserva");
        reserva.backup = true;
        let p = pool("p", vec![target("a"), reserva]);

        let selected = m.select_targets(&p, "1.1.1.1");
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

        let selected = m.select_targets(&p, "1.1.1.1");
        assert_eq!(urls(&selected), vec!["reserva"]);
    }

    #[test]
    fn max_conns_takes_a_saturated_server_out_of_rotation() {
        let m = manager();
        let mut limitado = target("limitado");
        limitado.max_conns = Some(1);
        let p = pool("p", vec![limitado, target("livre")]);

        m.inc_conn("limitado");
        let selected = m.select_targets(&p, "1.1.1.1");
        assert_eq!(urls(&selected), vec!["livre"]);

        m.dec_conn("limitado");
        assert_eq!(m.select_targets(&p, "1.1.1.1").len(), 2);
    }

    #[test]
    fn saturated_pool_still_gets_tried_as_a_last_resort() {
        // Devolver 502 sem tentar seria pior que exceder o teto por uma requisição.
        let m = manager();
        let mut limitado = target("unico");
        limitado.max_conns = Some(1);
        let p = pool("p", vec![limitado]);

        m.inc_conn("unico");
        assert_eq!(urls(&m.select_targets(&p, "1.1.1.1")), vec!["unico"]);
    }

    #[test]
    fn ip_hash_is_stable_for_the_same_client() {
        let m = manager();
        let mut p = pool("p", vec![target("a"), target("b"), target("c")]);
        p.algorithm = LoadBalanceAlgorithm::IpHash;

        let first = m.select_targets(&p, "10.0.0.1")[0].url.clone();
        for _ in 0..10 {
            assert_eq!(m.select_targets(&p, "10.0.0.1")[0].url, first);
        }
    }

    #[test]
    fn ip_hash_separates_different_clients() {
        let m = manager();
        let mut p = pool("p", vec![target("a"), target("b"), target("c")]);
        p.algorithm = LoadBalanceAlgorithm::IpHash;

        let chosen: std::collections::HashSet<String> = (0..40)
            .map(|i| {
                m.select_targets(&p, &format!("10.0.0.{}", i))[0]
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
        assert_eq!(m.select_targets(&p, "1.1.1.1")[0].url, "ocioso");
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
            if m.select_targets(&p, "1.1.1.1")[0].url == "pesado" {
                pesado_count += 1;
            }
        }
        // Proporção 3:1 — a margem é folgada de propósito, o teste é sobre a tendência.
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

        let selected = m.select_targets(&p, "1.1.1.1");
        assert_eq!(selected.len(), 2);
    }

    #[test]
    fn empty_pool_yields_nothing() {
        let m = manager();
        assert!(m.select_targets(&pool("p", vec![]), "1.1.1.1").is_empty());
    }

    #[test]
    fn single_target_pool_is_returned_as_is() {
        let m = manager();
        let p = pool("p", vec![target("unico")]);
        assert_eq!(urls(&m.select_targets(&p, "1.1.1.1")), vec!["unico"]);
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
