use axum::http::{header, HeaderMap, Method, Uri};
use std::time::Duration;

use crate::http::upstream::{OutgoingBody, UpstreamClient};

pub fn spawn(base: &str, method: &Method, uri: &Uri, headers: &HeaderMap) {
    let base = base.trim_end_matches('/').to_string();
    let method = method.clone();
    let path = uri
        .path_and_query()
        .map(|p| p.as_str().to_string())
        .unwrap_or_else(|| "/".into());
    let mut h = headers.clone();
    for n in [
        header::CONTENT_LENGTH,
        header::TRANSFER_ENCODING,
        header::HOST,
    ] {
        h.remove(n);
    }
    tokio::spawn(async move {
        let client = UpstreamClient::new(Duration::from_secs(2), Duration::from_secs(10), true);
        match client
            .send(&method, &base, &path, &h, OutgoingBody::Empty)
            .await
        {
            Ok(resp) => {
                let _ = axum::body::to_bytes(resp.into_body(), 1 << 20).await;
            }
            Err(e) => tracing::debug!(mirror = %base, error = %e, "mirror request failed"),
        }
    });
}
