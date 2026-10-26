use axum::http::{HeaderValue, Method, Uri};
use chrono::{DateTime, Utc};
use serde::ser::{SerializeStruct, Serializer};
use serde::Serialize;
use std::net::IpAddr;

// Built on every request, so every field is either Copy or a refcounted clone of data
// the request already owns (Uri, HeaderValue). Strings are only produced when the log is
// serialized for the dashboard, which happens orders of magnitude less often.
#[derive(Debug, Clone)]
pub struct RequestLog {
    pub id: u64,
    pub timestamp: DateTime<Utc>,
    pub client_ip: IpAddr,
    pub method: Method,
    pub uri: Uri,
    pub host: HeaderValue,
    pub status: u16,
    pub latency_ms: f64,
    pub bytes_sent: u64,
    pub upstream: Option<HeaderValue>,
    pub user_agent: Option<HeaderValue>,
    pub is_websocket: bool,
}

impl RequestLog {
    pub fn host_str(&self) -> &str {
        header_str(&self.host)
    }

    pub fn error(&self) -> Option<String> {
        (self.status >= 400).then(|| format!("HTTP {}", self.status))
    }
}

fn header_str(v: &HeaderValue) -> &str {
    v.to_str().unwrap_or("")
}

impl Serialize for RequestLog {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut s = serializer.serialize_struct("RequestLog", 13)?;
        s.serialize_field("id", &format!("{:016x}", self.id))?;
        s.serialize_field("timestamp", &self.timestamp)?;
        s.serialize_field("client_ip", &self.client_ip.to_string())?;
        s.serialize_field("method", self.method.as_str())?;
        s.serialize_field("uri", &self.uri.to_string())?;
        s.serialize_field("host", self.host_str())?;
        s.serialize_field("status", &self.status)?;
        s.serialize_field("latency_ms", &self.latency_ms)?;
        s.serialize_field("bytes_sent", &self.bytes_sent)?;
        s.serialize_field("upstream", &self.upstream.as_ref().map(header_str))?;
        s.serialize_field("user_agent", &self.user_agent.as_ref().map(header_str))?;
        s.serialize_field("is_websocket", &self.is_websocket)?;
        s.serialize_field("error", &self.error())?;
        s.end()
    }
}
