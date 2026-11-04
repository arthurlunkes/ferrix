//! nginx `limit_rate` / `limit_rate_after`: caps how fast a response body is
//! sent to the client (per response), after an optional unthrottled prefix.

use axum::body::{Body, Bytes};
use axum::http::Response;
use futures_util::StreamExt;
use std::time::Duration;
use tokio::time::Instant;

/// Largest slice written at once, so pauses stay short and smooth.
fn slice_for(rate: u64) -> usize {
    (rate / 8).clamp(1024, 256 * 1024) as usize
}

pub fn apply(resp: Response<Body>, rate: u64, after: u64) -> Response<Body> {
    if rate == 0 {
        return resp;
    }
    let (mut parts, body) = resp.into_parts();
    // The zero-copy sendfile path would bypass the throttle.
    parts.extensions.remove::<crate::http::server::SendFile>();
    let slice = slice_for(rate);
    let stream = async_stream(body.into_data_stream(), rate, after, slice);
    Response::from_parts(parts, Body::from_stream(stream))
}

fn async_stream(
    inner: axum::body::BodyDataStream,
    rate: u64,
    after: u64,
    slice: usize,
) -> impl futures_util::Stream<Item = Result<Bytes, axum::Error>> {
    struct St {
        inner: axum::body::BodyDataStream,
        pending: Bytes,
        sent: u64,
        start: Option<Instant>,
    }
    let st = St {
        inner,
        pending: Bytes::new(),
        sent: 0,
        start: None,
    };
    futures_util::stream::unfold(st, move |mut st| async move {
        if st.pending.is_empty() {
            match st.inner.next().await? {
                Ok(b) => st.pending = b,
                Err(e) => return Some((Err(e), st)),
            }
        }
        let n = st.pending.len().min(slice);
        let chunk = st.pending.split_to(n);
        if st.sent >= after {
            let start = *st.start.get_or_insert_with(Instant::now);
            let throttled = st.sent - after;
            let due = start + Duration::from_secs_f64(throttled as f64 / rate as f64);
            tokio::time::sleep_until(due).await;
        }
        st.sent += n as u64;
        Some((Ok(chunk), st))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn throttles_after_the_free_prefix() {
        // 1 KB free, then 4 KB at 10 KB/s: the last 1 KB slice starts ~0.3 s in.
        let resp = apply(Response::new(Body::from(vec![0u8; 5120])), 10_240, 1024);
        let t0 = Instant::now();
        let b = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        assert_eq!(b.len(), 5120);
        let took = t0.elapsed().as_secs_f64();
        assert!((0.25..0.6).contains(&took), "took {took}");
    }
}
