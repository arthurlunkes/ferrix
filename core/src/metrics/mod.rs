pub mod logger;

use chrono::{DateTime, Utc};
use logger::RequestLog;
use serde::Serialize;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};
use std::sync::{Arc, RwLock};
use std::time::Instant;
use tokio::sync::broadcast;

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
    // Recent logs buffer (e.g. max 1000 items)
    recent_logs: RwLock<VecDeque<RequestLog>>,
    // Recent latencies for percentile calculation (max 1000 items)
    recent_latencies: RwLock<VecDeque<f64>>,
    // For RPS calculation (timestamp in secs -> count)
    rps_tracker: RwLock<(u64, u64, f64)>, // (current_sec, count_this_sec, last_rps)
    // Broadcast channel for real-time live request stream (SSE)
    log_sender: broadcast::Sender<RequestLog>,
}

impl MetricsCollector {
    pub fn new(capacity: usize) -> Arc<Self> {
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
            recent_logs: RwLock::new(VecDeque::with_capacity(capacity)),
            recent_latencies: RwLock::new(VecDeque::with_capacity(1000)),
            rps_tracker: RwLock::new((0, 0, 0.0)),
            log_sender,
        })
    }

    pub fn inc_active_connections(&self) {
        self.active_connections.fetch_add(1, Ordering::Relaxed);
    }

    pub fn dec_active_connections(&self) {
        self.active_connections.fetch_sub(1, Ordering::Relaxed);
    }

    /// Contabiliza bytes efetivamente entregues ao cliente.
    ///
    /// Chamado por chunk enquanto o corpo escoa. É o que torna o total confiável: o
    /// `Content-Length` da resposta, que era a fonte anterior, não existe em resposta
    /// streamada — e por isso o total ficava zerado justamente no tráfego mais pesado.
    pub fn add_bytes_sent(&self, bytes: u64) {
        self.total_bytes_sent.fetch_add(bytes, Ordering::Relaxed);
    }

    pub fn record_request(&self, log: RequestLog) {
        self.total_requests.fetch_add(1, Ordering::Relaxed);

        match log.status {
            200..=299 => {
                self.status_2xx.fetch_add(1, Ordering::Relaxed);
            }
            300..=399 => {
                self.status_3xx.fetch_add(1, Ordering::Relaxed);
            }
            400..=499 => {
                self.status_4xx.fetch_add(1, Ordering::Relaxed);
            }
            500..=599 => {
                self.status_5xx.fetch_add(1, Ordering::Relaxed);
            }
            _ => {}
        }

        // Update RPS
        let now_sec = Utc::now().timestamp() as u64;
        if let Ok(mut tracker) = self.rps_tracker.write() {
            if tracker.0 == now_sec {
                tracker.1 += 1;
            } else {
                let prev_rps = if now_sec == tracker.0 + 1 {
                    tracker.1 as f64
                } else {
                    0.0
                };
                tracker.0 = now_sec;
                tracker.1 = 1;
                tracker.2 = prev_rps;
            }
        }

        // Store latency
        if let Ok(mut latencies) = self.recent_latencies.write() {
            if latencies.len() >= 1000 {
                latencies.pop_front();
            }
            latencies.push_back(log.latency_ms);
        }

        // Broadcast to SSE listeners
        let _ = self.log_sender.send(log.clone());

        // Store in recent logs ring buffer
        if let Ok(mut logs) = self.recent_logs.write() {
            if logs.len() >= 500 {
                logs.pop_front();
            }
            logs.push_back(log);
        }
    }

    pub fn get_recent_logs(
        &self,
        limit: usize,
        filter_status: Option<u16>,
        query: Option<&str>,
    ) -> Vec<RequestLog> {
        if let Ok(logs) = self.recent_logs.read() {
            let result: Vec<RequestLog> = logs
                .iter()
                .rev()
                .filter(|log| {
                    if let Some(st) = filter_status {
                        if log.status / 100 != st / 100 && log.status != st {
                            return false;
                        }
                    }
                    if let Some(q) = query {
                        if !q.is_empty() {
                            let q_lower = q.to_lowercase();
                            return log.uri.to_lowercase().contains(&q_lower)
                                || log.method.to_lowercase().contains(&q_lower)
                                || log.client_ip.contains(&q_lower)
                                || log.host.to_lowercase().contains(&q_lower);
                        }
                    }
                    true
                })
                .take(limit)
                .cloned()
                .collect();
            result
        } else {
            Vec::new()
        }
    }

    pub fn subscribe_logs(&self) -> broadcast::Receiver<RequestLog> {
        self.log_sender.subscribe()
    }

    pub fn get_metrics_snapshot(&self) -> MetricsSnapshot {
        let uptime = self.start_time.elapsed().as_secs();
        let rps = self.rps_tracker.read().map(|t| t.2).unwrap_or(0.0);

        let (avg_lat, p95_lat, p99_lat) = if let Ok(latencies) = self.recent_latencies.read() {
            if latencies.is_empty() {
                (0.0, 0.0, 0.0)
            } else {
                let mut sorted: Vec<f64> = latencies.iter().copied().collect();
                sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
                let sum: f64 = sorted.iter().sum();
                let avg = sum / sorted.len() as f64;
                let p95_idx = ((sorted.len() as f64) * 0.95).floor() as usize;
                let p99_idx = ((sorted.len() as f64) * 0.99).floor() as usize;
                let p95 = sorted[p95_idx.min(sorted.len() - 1)];
                let p99 = sorted[p99_idx.min(sorted.len() - 1)];
                (avg, p95, p99)
            }
        } else {
            (0.0, 0.0, 0.0)
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
