use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RequestLog {
    pub id: String,
    pub timestamp: DateTime<Utc>,
    pub client_ip: String,
    pub method: String,
    pub uri: String,
    pub host: String,
    pub status: u16,
    pub latency_ms: f64,
    pub bytes_sent: u64,
    pub upstream: Option<String>,
    pub user_agent: Option<String>,
    pub is_websocket: bool,
    pub error: Option<String>,
}
