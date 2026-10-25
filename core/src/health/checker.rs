use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};
use tracing::{debug, error, info};

use crate::config::store::ConfigStore;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TargetHealthStatus {
    pub upstream_name: String,
    pub target_url: String,
    pub healthy: bool,
    pub latency_ms: f64,
    pub last_checked: Option<DateTime<Utc>>,
    pub consecutive_fails: u32,
    pub status_code: Option<u16>,
}

#[derive(Clone)]
pub struct HealthChecker {
    statuses: Arc<RwLock<HashMap<String, TargetHealthStatus>>>,

    http_client: reqwest::Client,

    http_client_insecure: reqwest::Client,
}

impl Default for HealthChecker {
    fn default() -> Self {
        Self::new()
    }
}

impl HealthChecker {
    pub fn new() -> Self {
        let build = |insecure: bool| {
            reqwest::Client::builder()
                .danger_accept_invalid_certs(insecure)
                .build()
                .unwrap_or_else(|_| reqwest::Client::new())
        };

        Self {
            statuses: Arc::new(RwLock::new(HashMap::new())),
            http_client: build(false),
            http_client_insecure: build(true),
        }
    }

    pub fn is_healthy(&self, target_url: &str) -> bool {
        if let Ok(map) = self.statuses.read() {
            if let Some(st) = map.get(target_url) {
                return st.healthy;
            }
        }

        true
    }

    pub fn mark_failure(&self, upstream_name: &str, target_url: &str, max_fails: u32) {
        if let Ok(mut map) = self.statuses.write() {
            let entry = map
                .entry(target_url.to_string())
                .or_insert_with(|| TargetHealthStatus {
                    upstream_name: upstream_name.to_string(),
                    target_url: target_url.to_string(),
                    healthy: true,
                    latency_ms: 0.0,
                    last_checked: Some(crate::clock::utc_now()),
                    consecutive_fails: 0,
                    status_code: None,
                });
            entry.consecutive_fails += 1;
            entry.last_checked = Some(crate::clock::utc_now());
            if entry.consecutive_fails >= max_fails {
                if entry.healthy {
                    error!(target = %target_url, fails = entry.consecutive_fails, "Target marked UNHEALTHY (passive)");
                }
                entry.healthy = false;
            }
        }
    }

    pub fn mark_success(&self, upstream_name: &str, target_url: &str, latency_ms: f64) {
        if let Ok(map) = self.statuses.read() {
            if let Some(st) = map.get(target_url) {
                let recent = st.last_checked.is_some_and(|t| {
                    crate::clock::utc_now()
                        .signed_duration_since(t)
                        .num_milliseconds()
                        < 1000
                });
                if st.healthy && st.consecutive_fails == 0 && recent {
                    return;
                }
            }
        }
        if let Ok(mut map) = self.statuses.write() {
            let entry = map
                .entry(target_url.to_string())
                .or_insert_with(|| TargetHealthStatus {
                    upstream_name: upstream_name.to_string(),
                    target_url: target_url.to_string(),
                    healthy: true,
                    latency_ms,
                    last_checked: Some(crate::clock::utc_now()),
                    consecutive_fails: 0,
                    status_code: Some(200),
                });
            entry.consecutive_fails = 0;
            entry.healthy = true;
            entry.latency_ms = latency_ms;
            entry.last_checked = Some(crate::clock::utc_now());
        }
    }

    pub fn get_all_statuses(&self) -> Vec<TargetHealthStatus> {
        if let Ok(map) = self.statuses.read() {
            map.values().cloned().collect()
        } else {
            Vec::new()
        }
    }

    pub fn start_background_checker(self: Arc<Self>, store: Arc<ConfigStore>) {
        tokio::spawn(async move {
            info!("Health check worker started");

            let mut last_check: HashMap<String, Instant> = HashMap::new();
            let mut ticker = tokio::time::interval(Duration::from_secs(1));

            loop {
                ticker.tick().await;
                let cfg = store.config();
                let now = Instant::now();

                for (upstream_name, upstream_cfg) in &cfg.upstreams {
                    if let Some(hc) = &upstream_cfg.health_check {
                        let interval = Duration::from_secs(hc.interval_secs.max(1));

                        for target in &upstream_cfg.targets {
                            if target.down {
                                continue;
                            }
                            let due = last_check
                                .get(&target.url)
                                .is_none_or(|last| now.duration_since(*last) >= interval);
                            if !due {
                                continue;
                            }
                            last_check.insert(target.url.clone(), now);

                            let client = if upstream_cfg.verify_tls {
                                self.http_client.clone()
                            } else {
                                self.http_client_insecure.clone()
                            };
                            let upstream_name = upstream_name.clone();
                            let target_url = target.url.clone();
                            let check_path = hc.path.clone();
                            let expected = hc.expected_status;
                            let timeout = Duration::from_secs(hc.timeout_secs.max(1));
                            let max_fails = target.max_fails;
                            let statuses = self.statuses.clone();

                            tokio::spawn(async move {
                                let full_url =
                                    format!("{}{}", target_url.trim_end_matches('/'), check_path);
                                let start = Instant::now();
                                let result = client.get(&full_url).timeout(timeout).send().await;
                                let elapsed = start.elapsed().as_secs_f64() * 1000.0;

                                let mut is_ok = false;
                                let mut code_val = None;

                                if let Ok(res) = result {
                                    code_val = Some(res.status().as_u16());
                                    if res.status().as_u16() == expected {
                                        is_ok = true;
                                    }
                                }

                                if let Ok(mut map) = statuses.write() {
                                    let entry =
                                        map.entry(target_url.clone()).or_insert_with(|| {
                                            TargetHealthStatus {
                                                upstream_name: upstream_name.clone(),
                                                target_url: target_url.clone(),
                                                healthy: true,
                                                latency_ms: elapsed,
                                                last_checked: Some(Utc::now()),
                                                consecutive_fails: 0,
                                                status_code: code_val,
                                            }
                                        });

                                    entry.last_checked = Some(Utc::now());
                                    entry.status_code = code_val;
                                    entry.latency_ms = (elapsed * 10.0).round() / 10.0;

                                    if is_ok {
                                        entry.consecutive_fails = 0;
                                        entry.healthy = true;
                                        debug!(target = %target_url, "Health check PASSED");
                                    } else {
                                        entry.consecutive_fails += 1;
                                        if entry.consecutive_fails >= max_fails {
                                            if entry.healthy {
                                                error!(target = %target_url, "Health check FAILED: marked UNHEALTHY");
                                            }
                                            entry.healthy = false;
                                        }
                                    }
                                }
                            });
                        }
                    }
                }
            }
        });
    }
}
