pub mod logger;

use chrono::{DateTime, Utc};
use logger::RequestLog;
use serde::Serialize;
use std::fmt::Write as _;
use std::sync::atomic::{AtomicI64, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};
use tokio::sync::broadcast;

const LATENCY_WINDOW: usize = 1024;

const LOG_CAPACITY: usize = 500;

// Counters are split into stripes, one per worker thread in practice, so the hot path
// never bounces a shared cache line between cores; readers add the stripes up.
const STRIPES: usize = 16;

/// Upper bounds (seconds) of the Prometheus latency histogram buckets.
const BUCKETS: [f64; 11] = [
    0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0,
];

#[derive(Debug, Clone, Serialize)]
pub struct MetricsSnapshot {
    /// `metrics.enabled` from the config; when false every counter stays at zero.
    pub enabled: bool,
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

#[repr(align(128))]
struct Stripe {
    requests: AtomicU64,
    status: [AtomicU64; 4],
    bytes: AtomicU64,
    active: AtomicI64,
    buckets: [AtomicU64; BUCKETS.len() + 1],
    latency_sum_us: AtomicU64,

    // Allocated on first use, so a Ferrix with metrics disabled never pays for them.
    latencies: OnceLock<Box<[AtomicU64]>>,
    latency_head: AtomicUsize,

    log_slots: OnceLock<Box<[Mutex<Option<RequestLog>>]>>,
    log_head: AtomicUsize,
}

impl Stripe {
    fn latencies(&self) -> &[AtomicU64] {
        self.latencies
            .get_or_init(|| (0..LATENCY_WINDOW).map(|_| AtomicU64::new(0)).collect())
    }

    fn log_slots(&self) -> &[Mutex<Option<RequestLog>>] {
        self.log_slots
            .get_or_init(|| (0..LOG_CAPACITY).map(|_| Mutex::new(None)).collect())
    }

    fn new() -> Self {
        Self {
            requests: AtomicU64::new(0),
            status: Default::default(),
            bytes: AtomicU64::new(0),
            active: AtomicI64::new(0),
            buckets: Default::default(),
            latency_sum_us: AtomicU64::new(0),
            latencies: OnceLock::new(),
            latency_head: AtomicUsize::new(0),
            log_slots: OnceLock::new(),
            log_head: AtomicUsize::new(0),
        }
    }
}

fn stripe_index() -> usize {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    thread_local! {
        static INDEX: usize = NEXT.fetch_add(1, Ordering::Relaxed) % STRIPES;
    }
    INDEX.with(|i| *i)
}

pub struct MetricsCollector {
    start_time: Instant,
    start_utc: DateTime<Utc>,

    stripes: Box<[Stripe]>,

    rps_last: AtomicU64,

    log_sender: broadcast::Sender<RequestLog>,
}

impl MetricsCollector {
    pub fn new(_capacity: usize) -> Arc<Self> {
        let (log_sender, _) = broadcast::channel(512);
        Arc::new(Self {
            start_time: Instant::now(),
            start_utc: Utc::now(),
            stripes: (0..STRIPES).map(|_| Stripe::new()).collect(),
            rps_last: AtomicU64::new(0f64.to_bits()),
            log_sender,
        })
    }

    fn stripe(&self) -> &Stripe {
        &self.stripes[stripe_index()]
    }

    fn sum(&self, f: impl Fn(&Stripe) -> u64) -> u64 {
        self.stripes.iter().map(f).sum()
    }

    /// Samples the request counter once a second to publish requests/second, instead of
    /// doing that bookkeeping on every request.
    pub fn start_rps_ticker(self: &Arc<Self>) {
        let me = Arc::downgrade(self);
        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(Duration::from_secs(1));
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            let mut last: Option<(u64, Instant)> = None;
            loop {
                ticker.tick().await;
                let Some(m) = me.upgrade() else { return };
                let total = m.total_requests();
                let now = Instant::now();
                if let Some((prev, at)) = last {
                    let secs = now.duration_since(at).as_secs_f64().max(0.001);
                    let rps = (total.saturating_sub(prev)) as f64 / secs;
                    m.rps_last.store(rps.to_bits(), Ordering::Relaxed);
                }
                last = Some((total, now));
            }
        });
    }

    fn total_requests(&self) -> u64 {
        self.sum(|s| s.requests.load(Ordering::Relaxed))
    }

    pub fn inc_active_connections(&self) {
        self.stripe().active.fetch_add(1, Ordering::Relaxed);
    }

    pub fn dec_active_connections(&self) {
        self.stripe().active.fetch_sub(1, Ordering::Relaxed);
    }

    pub fn add_bytes_sent(&self, bytes: u64) {
        self.stripe().bytes.fetch_add(bytes, Ordering::Relaxed);
    }

    pub fn record_request(&self, log: RequestLog, log_line: bool) {
        let s = self.stripe();
        s.requests.fetch_add(1, Ordering::Relaxed);

        if let 200..=599 = log.status {
            s.status[(log.status / 100 - 2) as usize].fetch_add(1, Ordering::Relaxed);
        }

        let secs = log.latency_ms / 1000.0;
        let bucket = BUCKETS
            .iter()
            .position(|b| secs <= *b)
            .unwrap_or(BUCKETS.len());
        s.buckets[bucket].fetch_add(1, Ordering::Relaxed);
        s.latency_sum_us
            .fetch_add((log.latency_ms * 1000.0) as u64, Ordering::Relaxed);

        let i = s.latency_head.fetch_add(1, Ordering::Relaxed) % LATENCY_WINDOW;
        s.latencies()[i].store(log.latency_ms.to_bits(), Ordering::Relaxed);

        if !log_line {
            return;
        }

        if self.log_sender.receiver_count() > 0 {
            let _ = self.log_sender.send(log.clone());
        }

        let slot = s.log_head.fetch_add(1, Ordering::AcqRel) % LOG_CAPACITY;
        if let Ok(mut l) = s.log_slots()[slot].lock() {
            *l = Some(log);
        }
    }

    pub fn get_recent_logs(
        &self,
        limit: usize,
        filter_status: Option<u16>,
        query: Option<&str>,
    ) -> Vec<RequestLog> {
        let q_lower = query.filter(|q| !q.is_empty()).map(|q| q.to_lowercase());

        let mut all: Vec<RequestLog> = Vec::new();
        for s in self.stripes.iter() {
            let Some(slots) = s.log_slots.get() else {
                continue;
            };
            let head = s.log_head.load(Ordering::Acquire);
            let filled = head.min(LOG_CAPACITY);
            for k in 1..=filled {
                let idx = (head - k) % LOG_CAPACITY;
                let Ok(slot) = slots[idx].lock() else {
                    continue;
                };
                let Some(log) = slot.as_ref() else { continue };

                if let Some(st) = filter_status {
                    if log.status / 100 != st / 100 && log.status != st {
                        continue;
                    }
                }
                if let Some(q) = &q_lower {
                    let hit = log.uri.to_string().to_lowercase().contains(q)
                        || log.method.as_str().to_lowercase().contains(q)
                        || log.client_ip.to_string().contains(q.as_str())
                        || log.host_str().to_lowercase().contains(q);
                    if !hit {
                        continue;
                    }
                }
                all.push(log.clone());
            }
        }
        // Newest first across stripes; ids break ties within the same clock tick.
        all.sort_by(|a, b| b.timestamp.cmp(&a.timestamp).then(b.id.cmp(&a.id)));
        all.truncate(limit.min(LOG_CAPACITY));
        all
    }

    pub fn subscribe_logs(&self) -> broadcast::Receiver<RequestLog> {
        self.log_sender.subscribe()
    }

    pub fn get_metrics_snapshot(&self) -> MetricsSnapshot {
        let uptime = self.start_time.elapsed().as_secs();
        let rps = f64::from_bits(self.rps_last.load(Ordering::Relaxed));

        let mut sorted: Vec<f64> = Vec::new();
        for s in self.stripes.iter() {
            let Some(latencies) = s.latencies.get() else {
                continue;
            };
            let filled = s.latency_head.load(Ordering::Relaxed).min(LATENCY_WINDOW);
            sorted.extend(
                latencies[..filled]
                    .iter()
                    .map(|a| f64::from_bits(a.load(Ordering::Relaxed))),
            );
        }

        let (avg_lat, p95_lat, p99_lat) = if sorted.is_empty() {
            (0.0, 0.0, 0.0)
        } else {
            sorted.sort_unstable_by(|a, b| a.total_cmp(b));
            let n = sorted.len();
            let avg = sorted.iter().sum::<f64>() / n as f64;
            let pick = |p: f64| sorted[((n as f64 * p).floor() as usize).min(n - 1)];
            (avg, pick(0.95), pick(0.99))
        };

        let status = |i: usize| self.sum(|s| s.status[i].load(Ordering::Relaxed));
        MetricsSnapshot {
            enabled: true,
            uptime_seconds: uptime,
            total_requests: self.total_requests(),
            active_connections: self
                .stripes
                .iter()
                .map(|s| s.active.load(Ordering::Relaxed))
                .sum::<i64>()
                .max(0),
            requests_per_second: rps,
            status_2xx: status(0),
            status_3xx: status(1),
            status_4xx: status(2),
            status_5xx: status(3),
            total_bytes_sent: self.sum(|s| s.bytes.load(Ordering::Relaxed)),
            avg_latency_ms: (avg_lat * 100.0).round() / 100.0,
            p95_latency_ms: (p95_lat * 100.0).round() / 100.0,
            p99_latency_ms: (p99_lat * 100.0).round() / 100.0,
        }
    }

    pub fn start_time_utc(&self) -> DateTime<Utc> {
        self.start_utc
    }

    /// Prometheus text exposition format (version 0.0.4).
    pub fn prometheus(&self, upstreams: &[(String, String, bool)]) -> String {
        let mut out = String::with_capacity(2048);
        let snap = self.get_metrics_snapshot();

        let _ = writeln!(
            out,
            "# HELP ferrix_requests_total Requests handled by the proxy."
        );
        let _ = writeln!(out, "# TYPE ferrix_requests_total counter");
        for (i, class) in ["2xx", "3xx", "4xx", "5xx"].iter().enumerate() {
            let v = self.sum(|s| s.status[i].load(Ordering::Relaxed));
            let _ = writeln!(out, "ferrix_requests_total{{code=\"{}\"}} {}", class, v);
        }
        let classified: u64 = snap.status_2xx + snap.status_3xx + snap.status_4xx + snap.status_5xx;
        let _ = writeln!(
            out,
            "ferrix_requests_total{{code=\"other\"}} {}",
            snap.total_requests.saturating_sub(classified)
        );

        let _ = writeln!(
            out,
            "# HELP ferrix_request_duration_seconds Time to produce the response head."
        );
        let _ = writeln!(out, "# TYPE ferrix_request_duration_seconds histogram");
        let mut cumulative = 0u64;
        for (i, le) in BUCKETS.iter().enumerate() {
            cumulative += self.sum(|s| s.buckets[i].load(Ordering::Relaxed));
            let _ = writeln!(
                out,
                "ferrix_request_duration_seconds_bucket{{le=\"{}\"}} {}",
                le, cumulative
            );
        }
        cumulative += self.sum(|s| s.buckets[BUCKETS.len()].load(Ordering::Relaxed));
        let _ = writeln!(
            out,
            "ferrix_request_duration_seconds_bucket{{le=\"+Inf\"}} {}",
            cumulative
        );
        let sum_us = self.sum(|s| s.latency_sum_us.load(Ordering::Relaxed));
        let _ = writeln!(
            out,
            "ferrix_request_duration_seconds_sum {}",
            sum_us as f64 / 1e6
        );
        let _ = writeln!(out, "ferrix_request_duration_seconds_count {}", cumulative);

        let _ = writeln!(
            out,
            "# HELP ferrix_response_bytes_total Response body bytes sent."
        );
        let _ = writeln!(out, "# TYPE ferrix_response_bytes_total counter");
        let _ = writeln!(out, "ferrix_response_bytes_total {}", snap.total_bytes_sent);

        let _ = writeln!(
            out,
            "# HELP ferrix_active_requests Requests currently in flight."
        );
        let _ = writeln!(out, "# TYPE ferrix_active_requests gauge");
        let _ = writeln!(out, "ferrix_active_requests {}", snap.active_connections);

        let _ = writeln!(
            out,
            "# HELP ferrix_uptime_seconds Seconds since Ferrix started."
        );
        let _ = writeln!(out, "# TYPE ferrix_uptime_seconds gauge");
        let _ = writeln!(out, "ferrix_uptime_seconds {}", snap.uptime_seconds);

        if !upstreams.is_empty() {
            let _ = writeln!(
                out,
                "# HELP ferrix_upstream_up Whether an upstream target is considered healthy."
            );
            let _ = writeln!(out, "# TYPE ferrix_upstream_up gauge");
            for (pool, target, healthy) in upstreams {
                let _ = writeln!(
                    out,
                    "ferrix_upstream_up{{upstream=\"{}\",target=\"{}\"}} {}",
                    escape_label(pool),
                    escape_label(target),
                    *healthy as u8
                );
            }
        }
        out
    }
}

fn escape_label(v: &str) -> String {
    v.replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_log(status: u16) -> RequestLog {
        RequestLog {
            id: 1,
            timestamp: Utc::now(),
            client_ip: "127.0.0.1".parse().unwrap(),
            method: axum::http::Method::GET,
            uri: axum::http::Uri::from_static("/x"),
            host: axum::http::HeaderValue::from_static("exemplo.com"),
            status,
            latency_ms: 1.0,
            bytes_sent: 10,
            upstream: None,
            user_agent: None,
            is_websocket: false,
        }
    }

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
            l.uri = format!("/{i}").parse().unwrap();
            m.record_request(l, true);
        }
        let logs = m.get_recent_logs(usize::MAX, None, None);
        assert_eq!(logs.len(), LOG_CAPACITY);
        assert_eq!(logs[0].uri.to_string(), format!("/{}", LOG_CAPACITY + 9));
        assert_eq!(logs.last().unwrap().uri.to_string(), "/10");
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
    fn prometheus_exposition_has_counters_and_histogram() {
        let m = MetricsCollector::new(0);
        let mut l = sample_log(200);
        l.latency_ms = 7.0;
        m.record_request(l, false);
        m.record_request(sample_log(503), false);
        let text = m.prometheus(&[("api".into(), "http://a:1".into(), true)]);
        assert!(text.contains("ferrix_requests_total{code=\"2xx\"} 1"));
        assert!(text.contains("ferrix_requests_total{code=\"5xx\"} 1"));
        assert!(text.contains("ferrix_request_duration_seconds_bucket{le=\"0.01\"} 2"));
        assert!(text.contains("ferrix_request_duration_seconds_count 2"));
        assert!(text.contains("ferrix_upstream_up{upstream=\"api\",target=\"http://a:1\"} 1"));
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
        assert_eq!(
            m.get_recent_logs(usize::MAX, None, None).len(),
            LOG_CAPACITY
        );
    }
}
