//! Live request log as Server-Sent Events (`GET /api/logs/stream`).

use axum::body::{Body, Bytes};
use axum::http::{header, HeaderValue};
use axum::response::Response;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::broadcast::error::RecvError;

use crate::metrics::MetricsCollector;

/// A comment line every so often keeps proxies and browsers from closing an idle stream.
const KEEP_ALIVE: Duration = Duration::from_secs(15);

pub fn stream_logs(metrics: Arc<MetricsCollector>) -> Response {
    let rx = metrics.subscribe_logs();
    let events = futures_util::stream::unfold(rx, |mut rx| async move {
        loop {
            tokio::select! {
                msg = rx.recv() => match msg {
                    Ok(log) => {
                        let json = serde_json::to_string(&log).unwrap_or_default();
                        let frame = format!("event: request\ndata: {}\n\n", json);
                        return Some((Ok::<_, std::io::Error>(Bytes::from(frame)), rx));
                    }
                    // A slow client missed some entries: keep streaming the newer ones.
                    Err(RecvError::Lagged(_)) => continue,
                    Err(RecvError::Closed) => return None,
                },
                _ = tokio::time::sleep(KEEP_ALIVE) => {
                    return Some((Ok(Bytes::from_static(b": keep-alive\n\n")), rx));
                }
            }
        }
    });

    let mut resp = Response::new(Body::from_stream(events));
    let h = resp.headers_mut();
    h.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/event-stream"),
    );
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
    resp
}
