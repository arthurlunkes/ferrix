use axum::body::{Body, Bytes};
use axum::http::{HeaderMap, Method, Response};
use http_body::{Body as HttpBody, Frame, SizeHint};
use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Duration;
use tokio::time::Sleep;
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

pub struct UpstreamClient {
    connect_timeout: Duration,
    read_timeout: Duration,
    verify_tls: bool,
    tls: TlsConnector,
}

impl UpstreamClient {
    pub fn new(connect_timeout: Duration, read_timeout: Duration, verify_tls: bool) -> Self {
        Self {
            connect_timeout,
            read_timeout,
            verify_tls,
            tls: h1::tls_connector(verify_tls),
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
        let deadline = tokio::time::Instant::from_std(crate::clock::now() + self.read_timeout);

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
                        let tls = target.tls.then_some((&self.tls, insecure));
                        h1::connect(&target, base, self.connect_timeout, tls).await?
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

        let resp = match tokio::time::timeout_at(deadline, attempt).await {
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

struct ReadTimeoutBody<B> {
    inner: B,
    sleep: Pin<Box<Sleep>>,
    dur: Duration,
    last_activity_ms: u64,
}

impl<B> ReadTimeoutBody<B> {
    fn new(inner: B, dur: Duration) -> Self {
        let deadline = tokio::time::Instant::from_std(crate::clock::now() + dur);
        Self {
            inner,
            sleep: Box::pin(tokio::time::sleep_until(deadline)),
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
                Poll::Pending => match this.sleep.as_mut().poll(cx) {
                    Poll::Pending => return Poll::Pending,
                    Poll::Ready(()) => {
                        let idle_ms = crate::clock::mono_ms().saturating_sub(this.last_activity_ms);
                        let limit_ms = this.dur.as_millis() as u64;
                        if idle_ms >= limit_ms {
                            return Poll::Ready(Some(Err(
                                "tempo de leitura do upstream esgotado".into()
                            )));
                        }

                        let remaining = Duration::from_millis(limit_ms - idle_ms);
                        let deadline =
                            tokio::time::Instant::from_std(crate::clock::now() + remaining);
                        this.sleep.as_mut().reset(deadline);
                    }
                },
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
