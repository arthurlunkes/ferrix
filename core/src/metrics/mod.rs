pub mod logger;

use chrono::{DateTime, Utc};
use logger::RequestLog;
use serde::Serialize;
use std::sync::atomic::{AtomicI64, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;
use tokio::sync::broadcast;

/// Janela de amostras de latência usada para avg/p95/p99 (potência de 2: índice via máscara).
const LATENCY_WINDOW: usize = 1024;
/// Linhas mantidas no buffer de logs recentes (mesmo teto de antes).
const LOG_CAPACITY: usize = 500;

#[derive(Debug, Clone, Serialize)]
pub struct MetricsSnapshot {
    pub uptime_seconds: u64,
    pub total_requests: u64,
    pub active_connections: i64,
    pub requests_per_second: f64,
    pub status_2xx: u64,
    pub status_3xx: u64,
    pub status_4xx: u64,
    pub status_5xx: u64,
    pub total_bytes_sent: u64,
    pub avg_latency_ms: f64,
    pub p95_latency_ms: f64,
    pub p99_latency_ms: f64,
}

/// Coletor de métricas sem lock global no caminho quente.
///
/// Antes, toda requisição pegava três `RwLock::write()` compartilhados por todos os
/// cores (RPS, latências e logs) — sob carga, as threads do Tokio serializavam nesses
/// locks e o p99 explodia. Agora:
/// - contadores e RPS: só atômicos;
/// - latências: ring buffer de `AtomicU64` (bits do f64), escrita = 1 `fetch_add` + 1 `store`;
/// - logs recentes: ring buffer de slots com `Mutex` **por slot** — duas requisições só
///   disputam o mesmo lock se caírem no mesmo slot, o que exige dar a volta no buffer.
/// - SSE: o `RequestLog` só é clonado se houver alguém ouvindo.
pub struct MetricsCollector {
    start_time: Instant,
    start_utc: DateTime<Utc>,
    total_requests: AtomicU64,
    active_connections: AtomicI64,
    status_2xx: AtomicU64,
    status_3xx: AtomicU64,
    status_4xx: AtomicU64,
    status_5xx: AtomicU64,
    total_bytes_sent: AtomicU64,

    // RPS: segundo corrente, contagem nele, e RPS fechado do segundo anterior (bits f64).
    rps_sec: AtomicU64,
    rps_count: AtomicU64,
    rps_last: AtomicU64,

    // Latências: ring buffer lock-free.
    latencies: Box<[AtomicU64]>,
    latency_head: AtomicUsize,

    // Logs recentes: ring buffer com lock por slot.
    log_slots: Box<[Mutex<Option<RequestLog>>]>,
    log_head: AtomicUsize,

    log_sender: broadcast::Sender<RequestLog>,
}

impl MetricsCollector {
    /// `_capacity` mantido por compatibilidade de assinatura; o teto é `LOG_CAPACITY`,
    /// como já era na prática.
    pub fn new(_capacity: usize) -> Arc<Self> {
        let (log_sender, _) = broadcast::channel(512);
        Arc::new(Self {
            start_time: Instant::now(),
            start_utc: Utc::now(),
            total_requests: AtomicU64::new(0),
            active_connections: AtomicI64::new(0),
            status_2xx: AtomicU64::new(0),
            status_3xx: AtomicU64::new(0),
            status_4xx: AtomicU64::new(0),
            status_5xx: AtomicU64::new(0),
            total_bytes_sent: AtomicU64::new(0),
            rps_sec: AtomicU64::new(0),
            rps_count: AtomicU64::new(0),
            rps_last: AtomicU64::new(0f64.to_bits()),
            latencies: (0..LATENCY_WINDOW).map(|_| AtomicU64::new(0)).collect(),
            latency_head: AtomicUsize::new(0),
            log_slots: (0..LOG_CAPACITY).map(|_| Mutex::new(None)).collect(),
            log_head: AtomicUsize::new(0),
            log_sender,
        })
    }

    pub fn inc_active_connections(&self) {
        self.active_connections.fetch_add(1, Ordering::Relaxed);
    }

    pub fn dec_active_connections(&self) {
        self.active_connections.fetch_sub(1, Ordering::Relaxed);
    }

    /// Contabiliza bytes efetivamente entregues ao cliente, chunk a chunk.
    pub fn add_bytes_sent(&self, bytes: u64) {
        self.total_bytes_sent.fetch_add(bytes, Ordering::Relaxed);
    }

    /// `log_line: false` (rota com `access_log: false`) continua contando nos agregados;
    /// só não aparece no buffer de logs nem no stream ao vivo — como `access_log off`.
    pub fn record_request(&self, log: RequestLog, log_line: bool) {
        self.total_requests.fetch_add(1, Ordering::Relaxed);

        let counter = match log.status {
            200..=299 => Some(&self.status_2xx),
            300..=399 => Some(&self.status_3xx),
            400..=499 => Some(&self.status_4xx),
            500..=599 => Some(&self.status_5xx),
            _ => None,
        };
        if let Some(c) = counter {
            c.fetch_add(1, Ordering::Relaxed);
        }

        self.tick_rps();

        // Latência: reserva um índice e grava. Sem lock.
        let i = self.latency_head.fetch_add(1, Ordering::Relaxed) % LATENCY_WINDOW;
        self.latencies[i].store(log.latency_ms.to_bits(), Ordering::Relaxed);

        if !log_line {
            return;
        }

        // Só clona se houver ouvinte SSE.
        if self.log_sender.receiver_count() > 0 {
            let _ = self.log_sender.send(log.clone());
        }

        let slot = self.log_head.fetch_add(1, Ordering::AcqRel) % LOG_CAPACITY;
        if let Ok(mut s) = self.log_slots[slot].lock() {
            *s = Some(log);
        }
    }

    /// RPS sem lock. Quem vê o segundo virar ganha o CAS e fecha o anterior; os demais
    /// só incrementam. Na virada pode perder/atribuir 1–2 req ao segundo vizinho —
    /// irrelevante para um indicador de painel.
    fn tick_rps(&self) {
        let now = crate::clock::unix_secs();
        let cur = self.rps_sec.load(Ordering::Relaxed);
        if cur == now {
            self.rps_count.fetch_add(1, Ordering::Relaxed);
            return;
        }
        if self
            .rps_sec
            .compare_exchange(cur, now, Ordering::AcqRel, Ordering::Relaxed)
            .is_ok()
        {
            let prev_count = self.rps_count.swap(1, Ordering::AcqRel);
            let prev_rps = if now == cur + 1 { prev_count as f64 } else { 0.0 };
            self.rps_last.store(prev_rps.to_bits(), Ordering::Relaxed);
        } else {
            self.rps_count.fetch_add(1, Ordering::Relaxed);
        }
    }

    pub fn get_recent_logs(
        &self,
        limit: usize,
        filter_status: Option<u16>,
        query: Option<&str>,
    ) -> Vec<RequestLog> {
        let head = self.log_head.load(Ordering::Acquire);
        let filled = head.min(LOG_CAPACITY);
        let q_lower = query.filter(|q| !q.is_empty()).map(|q| q.to_lowercase());

        let mut out = Vec::with_capacity(limit.min(filled));
        // Do mais recente para o mais antigo.
        for k in 1..=filled {
            if out.len() >= limit {
                break;
            }
            let idx = (head - k) % LOG_CAPACITY;
            let Ok(slot) = self.log_slots[idx].lock() else { continue };
            let Some(log) = slot.as_ref() else { continue };

            if let Some(st) = filter_status {
                if log.status / 100 != st / 100 && log.status != st {
                    continue;
                }
            }
            if let Some(q) = &q_lower {
                let hit = log.uri.to_lowercase().contains(q)
                    || log.method.to_lowercase().contains(q)
                    || log.client_ip.contains(q.as_str())
                    || log.host.to_lowercase().contains(q);
                if !hit {
                    continue;
                }
            }
            out.push(log.clone());
        }
        out
    }

    pub fn subscribe_logs(&self) -> broadcast::Receiver<RequestLog> {
        self.log_sender.subscribe()
    }

    pub fn get_metrics_snapshot(&self) -> MetricsSnapshot {
        let uptime = self.start_time.elapsed().as_secs();
        let rps = f64::from_bits(self.rps_last.load(Ordering::Relaxed));

        // Cópia da janela (fora do caminho quente) e ordenação local.
        let filled = self.latency_head.load(Ordering::Relaxed).min(LATENCY_WINDOW);
        let mut sorted: Vec<f64> = self.latencies[..filled]
            .iter()
            .map(|a| f64::from_bits(a.load(Ordering::Relaxed)))
            .collect();

        let (avg_lat, p95_lat, p99_lat) = if sorted.is_empty() {
            (0.0, 0.0, 0.0)
        } else {
            sorted.sort_unstable_by(|a, b| a.total_cmp(b));
            let n = sorted.len();
            let avg = sorted.iter().sum::<f64>() / n as f64;
            let pick = |p: f64| sorted[((n as f64 * p).floor() as usize).min(n - 1)];
            (avg, pick(0.95), pick(0.99))
        };

        MetricsSnapshot {
            uptime_seconds: uptime,
            total_requests: self.total_requests.load(Ordering::Relaxed),
            active_connections: self.active_connections.load(Ordering::Relaxed).max(0),
            requests_per_second: rps,
            status_2xx: self.status_2xx.load(Ordering::Relaxed),
            status_3xx: self.status_3xx.load(Ordering::Relaxed),
            status_4xx: self.status_4xx.load(Ordering::Relaxed),
            status_5xx: self.status_5xx.load(Ordering::Relaxed),
            total_bytes_sent: self.total_bytes_sent.load(Ordering::Relaxed),
            avg_latency_ms: (avg_lat * 100.0).round() / 100.0,
            p95_latency_ms: (p95_lat * 100.0).round() / 100.0,
            p99_latency_ms: (p99_lat * 100.0).round() / 100.0,
        }
    }

    pub fn start_time_utc(&self) -> DateTime<Utc> {
        self.start_utc
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_log(status: u16) -> RequestLog {
        RequestLog {
            id: "id".to_string(),
            timestamp: Utc::now(),
            client_ip: "127.0.0.1".to_string(),
            method: "GET".to_string(),
            uri: "/x".to_string(),
            host: "exemplo.com".to_string(),
            status,
            latency_ms: 1.0,
            bytes_sent: 10,
            upstream: None,
            user_agent: None,
            is_websocket: false,
            error: None,
        }
    }

    /// `access_log: false` numa rota vira `log_line: false` aqui — a requisição some do
    /// buffer de logs recentes, mas continua contando nos totais agregados. É o mesmo
    /// corte que `access_log off` faz no nginx: `stub_status` não para de contar.
    #[test]
    fn log_line_false_still_counts_aggregate_metrics() {
        let m = MetricsCollector::new(100);
        m.record_request(sample_log(200), false);

        let snap = m.get_metrics_snapshot();
        assert_eq!(snap.total_requests, 1);
        assert_eq!(snap.status_2xx, 1);
        assert!(m.get_recent_logs(10, None, None).is_empty());
    }

    #[test]
    fn log_line_true_appears_in_recent_logs() {
        let m = MetricsCollector::new(100);
        m.record_request(sample_log(200), true);

        assert_eq!(m.get_metrics_snapshot().total_requests, 1);
        assert_eq!(m.get_recent_logs(10, None, None).len(), 1);
    }

    #[test]
    fn mixed_requests_only_silent_ones_are_hidden_from_the_log() {
        let m = MetricsCollector::new(100);
        m.record_request(sample_log(200), true);
        m.record_request(sample_log(200), false);
        m.record_request(sample_log(404), true);

        assert_eq!(m.get_metrics_snapshot().total_requests, 3);
        assert_eq!(m.get_recent_logs(10, None, None).len(), 2);
    }

    #[test]
    fn recent_logs_wrap_and_stay_newest_first() {
        let m = MetricsCollector::new(0);
        for i in 0..(LOG_CAPACITY + 10) {
            let mut l = sample_log(200);
            l.uri = format!("/{i}");
            m.record_request(l, true);
        }
        let logs = m.get_recent_logs(usize::MAX, None, None);
        assert_eq!(logs.len(), LOG_CAPACITY);
        assert_eq!(logs[0].uri, format!("/{}", LOG_CAPACITY + 9));
        assert_eq!(logs.last().unwrap().uri, "/10");
    }

    #[test]
    fn percentiles_over_window() {
        let m = MetricsCollector::new(0);
        for i in 1..=100 {
            let mut l = sample_log(200);
            l.latency_ms = i as f64;
            m.record_request(l, false);
        }
        let s = m.get_metrics_snapshot();
        assert_eq!(s.p99_latency_ms, 100.0);
        assert_eq!(s.p95_latency_ms, 96.0);
        assert_eq!(s.avg_latency_ms, 50.5);
    }

    #[test]
    fn concurrent_recording_counts_everything() {
        let m = MetricsCollector::new(0);
        let hs: Vec<_> = (0..8)
            .map(|_| {
                let m = m.clone();
                std::thread::spawn(move || {
                    for _ in 0..10_000 {
                        m.record_request(sample_log(200), true);
                    }
                })
            })
            .collect();
        hs.into_iter().for_each(|h| h.join().unwrap());
        let s = m.get_metrics_snapshot();
        assert_eq!(s.total_requests, 80_000);
        assert_eq!(s.status_2xx, 80_000);
        assert_eq!(m.get_recent_logs(usize::MAX, None, None).len(), LOG_CAPACITY);
    }
}
