use crate::clock::CoarseSleep;
use axum::body::{Body, Bytes};
use axum::http::{HeaderMap, Method, Response};
use http_body::{Body as HttpBody, Frame, SizeHint};
use std::future::Future;
use std::pin::Pin;
use std::sync::LazyLock;
use std::task::{Context, Poll};
use std::time::Duration;
use tokio_rustls::TlsConnector;

use super::h1::{self, H1Error, RequestBody, Target};

#[derive(Debug)]
pub enum UpstreamError {
    Timeout(String),
    Other(String),
}

impl UpstreamError {
    pub fn is_timeout(&self) -> bool {
        matches!(self, UpstreamError::Timeout(_))
    }
}

impl std::fmt::Display for UpstreamError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            UpstreamError::Timeout(m) | UpstreamError::Other(m) => f.write_str(m),
        }
    }
}

pub enum OutgoingBody {
    Empty,

    Buffered(Bytes),

    Streaming(Body),
}

// Built once per process; cloning a TlsConnector is just an Arc bump, done only when a
// new TLS connection is opened.
static TLS_VERIFIED: LazyLock<TlsConnector> = LazyLock::new(|| h1::tls_connector(true));
static TLS_INSECURE: LazyLock<TlsConnector> = LazyLock::new(|| h1::tls_connector(false));

/// Per-request settings for talking to an upstream. Connections live in the per-thread
/// pool in `h1`, so this is a plain value with nothing shared to lock or refcount.
#[derive(Debug, Clone, Copy)]
pub struct UpstreamClient {
    connect_timeout: Duration,
    read_timeout: Duration,
    verify_tls: bool,
}

impl UpstreamClient {
    pub fn new(connect_timeout: Duration, read_timeout: Duration, verify_tls: bool) -> Self {
        Self {
            connect_timeout,
            read_timeout,
            verify_tls,
        }
    }

    fn tls(&self) -> &'static TlsConnector {
        if self.verify_tls {
            &TLS_VERIFIED
        } else {
            &TLS_INSECURE
        }
    }

    /// `base` is the target URL (`http://host:port`), `path` the path and query to request.
    pub async fn send(
        &self,
        method: &Method,
        base: &str,
        path: &str,
        headers: &HeaderMap,
        body: OutgoingBody,
    ) -> Result<Response<Body>, UpstreamError> {
        let base = base.trim_end_matches('/');
        let target = Target::parse(base)
            .ok_or_else(|| UpstreamError::Other(format!("Invalid upstream URL: {}", base)))?;
        let insecure = target.tls && !self.verify_tls;
        let deadline_ms = crate::clock::mono_ms() + self.read_timeout.as_millis() as u64;

        let attempt = async {
            let (full, mut streaming) = match body {
                OutgoingBody::Streaming(b) => (None, Some(b)),
                OutgoingBody::Buffered(b) if !b.is_empty() => (Some(b), None),
                _ => (None, None),
            };
            let content_length = headers
                .get(axum::http::header::CONTENT_LENGTH)
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.parse::<u64>().ok());

            // A pooled connection can be closed by the upstream at any moment; when that
            // happens before any response byte arrives, the request is replayed once on
            // a fresh connection. Streaming bodies can't be replayed, so they always use
            // a new connection.
            let mut reuse = streaming.is_none();
            loop {
                let pooled = if reuse {
                    h1::pooled(base, insecure)
                } else {
                    None
                };
                let reused = pooled.is_some();
                let conn = match pooled {
                    Some(c) => c,
                    None => {
                        let tls = target.tls.then_some((self.tls(), insecure));
                        // Boxed: opening a connection (with a possible TLS handshake,
                        // whose state is several KB) is rare once the pool is warm, so
                        // it shouldn't size every request's state machine.
                        Box::pin(h1::connect(&target, base, self.connect_timeout, tls)).await?
                    }
                };
                let req_body = match (&full, streaming.take()) {
                    (_, Some(b)) => RequestBody::Stream(b, content_length),
                    (Some(b), None) => RequestBody::Full(b),
                    (None, None) => RequestBody::Empty,
                };
                match h1::exchange(conn, method, path, target.authority, headers, req_body).await {
                    Err(H1Error::Stale(_)) if reused => {
                        reuse = false;
                        continue;
                    }
                    other => return other,
                }
            }
        };

        let resp = match crate::clock::timeout_at_ms(deadline_ms, attempt).await {
            Ok(Ok(r)) => r,
            Ok(Err(H1Error::ConnectTimeout)) => {
                return Err(UpstreamError::Timeout(
                    "Error communicating with upstream: connect timed out".to_string(),
                ))
            }
            Ok(Err(e)) => {
                return Err(UpstreamError::Other(format!(
                    "Error communicating with upstream: {}",
                    e
                )))
            }
            Err(_) => {
                return Err(UpstreamError::Timeout(
                    "Upstream response timed out".to_string(),
                ))
            }
        };

        let (parts, incoming) = resp.into_parts();
        let body = Body::new(ReadTimeoutBody::new(incoming, self.read_timeout));
        Ok(Response::from_parts(parts, body))
    }
}

// Fails the body when the upstream sends nothing for `dur`. The timer is only armed if
// a read actually has to wait, so bodies that arrive with the head cost nothing.
struct ReadTimeoutBody<B> {
    inner: B,
    sleep: Option<CoarseSleep>,
    dur: Duration,
    last_activity_ms: u64,
}

impl<B> ReadTimeoutBody<B> {
    fn new(inner: B, dur: Duration) -> Self {
        Self {
            inner,
            sleep: None,
            dur,
            last_activity_ms: crate::clock::mono_ms(),
        }
    }
}

impl<B> HttpBody for ReadTimeoutBody<B>
where
    B: HttpBody<Data = Bytes> + Unpin,
    B::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
{
    type Data = Bytes;
    type Error = Box<dyn std::error::Error + Send + Sync>;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, Self::Error>>> {
        let this = &mut *self;
        loop {
            match Pin::new(&mut this.inner).poll_frame(cx) {
                Poll::Ready(frame) => {
                    this.last_activity_ms = crate::clock::mono_ms();
                    return Poll::Ready(frame.map(|r| r.map_err(Into::into)));
                }
                Poll::Pending => {
                    let deadline = this.last_activity_ms + this.dur.as_millis() as u64;
                    let sleep = match &mut this.sleep {
                        Some(s) if s.deadline_ms() == deadline => s,
                        slot => slot.insert(CoarseSleep::until_ms(deadline)),
                    };
                    match Pin::new(sleep).poll(cx) {
                        Poll::Pending => return Poll::Pending,
                        Poll::Ready(()) => {
                            if crate::clock::mono_ms().saturating_sub(this.last_activity_ms)
                                >= this.dur.as_millis() as u64
                            {
                                return Poll::Ready(Some(Err(
                                    "tempo de leitura do upstream esgotado".into(),
                                )));
                            }
                            this.sleep = None;
                        }
                    }
                }
            }
        }
    }

    fn is_end_stream(&self) -> bool {
        self.inner.is_end_stream()
    }

    fn size_hint(&self) -> SizeHint {
        self.inner.size_hint()
    }
}
