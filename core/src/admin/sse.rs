use axum::response::sse::{Event, KeepAlive, Sse};
use futures_util::stream::Stream;
use std::convert::Infallible;
use std::sync::Arc;
use tokio_stream::wrappers::BroadcastStream;
use tokio_stream::StreamExt;

use crate::metrics::MetricsCollector;

pub async fn stream_logs(
    metrics: Arc<MetricsCollector>,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let rx = metrics.subscribe_logs();
    let stream = BroadcastStream::new(rx).filter_map(|msg| match msg {
        Ok(log) => {
            let json = serde_json::to_string(&log).unwrap_or_default();
            Some(Ok(Event::default().event("request").data(json)))
        }
        Err(_) => None,
    });

    Sse::new(stream).keep_alive(KeepAlive::default())
}
