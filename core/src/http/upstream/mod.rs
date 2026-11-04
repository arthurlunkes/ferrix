pub mod balancer;
pub mod cgi;
#[cfg(feature = "http2")]
pub mod grpc;
pub mod h1;
pub mod hash;
pub mod sticky;
pub mod tls_client;

use crate::event::clock::CoarseSleep;
use axum::body::{Body, Bytes};
use axum::http::{HeaderMap, Method, Response};
use http_body::{Body as HttpBody, Frame, SizeHint};
use std::future::Future;
use std::pin::Pin;
use std::sync::LazyLock;
use std::task::{Context, Poll};
use std::time::Duration;
use tokio_rustls::TlsConnector;

use h1::{H1Error, RequestBody, Target};

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

static TLS_VERIFIED: LazyLock<TlsConnector> = LazyLock::new(|| h1::tls_connector(true));
static TLS_INSECURE: LazyLock<TlsConnector> = LazyLock::new(|| h1::tls_connector(false));

#[derive(Clone, Copy)]
pub struct UpstreamClient {
    connect_timeout: Duration,
    read_timeout: Duration,
    verify_tls: bool,
    custom_tls: Option<&'static tls_client::CustomTls>,
}

impl std::fmt::Debug for UpstreamClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("UpstreamClient")
            .field("verify_tls", &self.verify_tls)
            .field("custom_tls", &self.custom_tls.is_some())
            .finish()
    }
}

impl UpstreamClient {
    pub fn new(connect_timeout: Duration, read_timeout: Duration, verify_tls: bool) -> Self {
        Self {
            connect_timeout,
            read_timeout,
            verify_tls,
            custom_tls: None,
        }
    }

    /// Uses the upstream's own TLS client settings (client cert, CA, name).
    pub fn with_tls(mut self, cfg: Option<&crate::config::UpstreamTlsConfig>) -> Self {
        self.custom_tls = tls_client::get(cfg, self.verify_tls);
        self
    }

    fn pool_key<'a>(&self, base: &'a str) -> std::borrow::Cow<'a, str> {
        match self.custom_tls {
            Some(c) => format!("{base}#{}", c.pool_tag).into(),
            None => base.into(),
        }
    }

    fn sni(&self) -> Option<&'static str> {
        self.custom_tls.and_then(|c| c.server_name.as_deref())
    }

    fn tls(&self) -> &'static TlsConnector {
        if let Some(c) = self.custom_tls {
            return &c.connector;
        }
        if self.verify_tls {
            &TLS_VERIFIED
        } else {
            &TLS_INSECURE
        }
    }

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
        let pool_key = self.pool_key(base);
        let deadline_ms = crate::event::clock::mono_ms() + self.read_timeout.as_millis() as u64;

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

            let mut reuse = streaming.is_none();
            loop {
                let pooled = if reuse {
                    h1::pooled(&pool_key, insecure)
                } else {
                    None
                };
                let reused = pooled.is_some();
                let conn = match pooled {
                    Some(c) => c,
                    None => {
                        let tls = target.tls.then_some((self.tls(), insecure, self.sni()));
                        Box::pin(h1::connect(&target, &pool_key, self.connect_timeout, tls)).await?
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

        let resp = match crate::event::clock::timeout_at_ms(deadline_ms, attempt).await {
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

impl UpstreamClient {
    pub async fn send_raw(
        &self,
        method: &Method,
        base: &str,
        encode: impl Fn(&str, &mut Vec<u8>),
        client_head: &mut Vec<u8>,
    ) -> Result<(h1::RawResponse, Duration), UpstreamError> {
        let base = base.trim_end_matches('/');
        let target = Target::parse(base)
            .ok_or_else(|| UpstreamError::Other(format!("Invalid upstream URL: {}", base)))?;
        let insecure = target.tls && !self.verify_tls;
        let pool_key = self.pool_key(base);
        let deadline_ms = crate::event::clock::mono_ms() + self.read_timeout.as_millis() as u64;
        let mut request = Vec::with_capacity(512);
        encode(target.authority, &mut request);

        let attempt = async {
            let mut reuse = true;
            loop {
                let pooled = if reuse {
                    h1::pooled(&pool_key, insecure)
                } else {
                    None
                };
                let reused = pooled.is_some();
                let conn = match pooled {
                    Some(c) => c,
                    None => {
                        let tls = target.tls.then_some((self.tls(), insecure, self.sni()));
                        Box::pin(h1::connect(&target, &pool_key, self.connect_timeout, tls)).await?
                    }
                };
                let mark = client_head.len();
                match h1::exchange_raw(conn, &request, method, client_head).await {
                    Err(H1Error::Stale(_)) if reused => {
                        client_head.truncate(mark);
                        reuse = false;
                        continue;
                    }
                    other => return other,
                }
            }
        };

        match crate::event::clock::timeout_at_ms(deadline_ms, attempt).await {
            Ok(Ok(r)) => Ok((r, self.read_timeout)),
            Ok(Err(H1Error::ConnectTimeout)) => Err(UpstreamError::Timeout(
                "Error communicating with upstream: connect timed out".to_string(),
            )),
            Ok(Err(e)) => Err(UpstreamError::Other(format!(
                "Error communicating with upstream: {}",
                e
            ))),
            Err(_) => Err(UpstreamError::Timeout(
                "Upstream response timed out".to_string(),
            )),
        }
    }
}

pub(crate) struct ReadTimeoutBody<B> {
    inner: B,
    sleep: Option<CoarseSleep>,
    dur: Duration,
    last_activity_ms: u64,
}

impl<B> ReadTimeoutBody<B> {
    pub(crate) fn new(inner: B, dur: Duration) -> Self {
        Self {
            inner,
            sleep: None,
            dur,
            last_activity_ms: crate::event::clock::mono_ms(),
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
                    this.last_activity_ms = crate::event::clock::mono_ms();
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
                            if crate::event::clock::mono_ms().saturating_sub(this.last_activity_ms)
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
