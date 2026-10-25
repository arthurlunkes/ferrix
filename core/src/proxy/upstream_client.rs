//! Cliente HTTP usado para falar com os upstreams.
//!
//! Upstreams em `http://` vão direto pelo cliente do hyper: a requisição do cliente é
//! repassada quase sem conversão (mesmo `HeaderMap`, mesmo tipo de corpo) e a resposta volta
//! como `Body` nativo, preservando o tamanho exato do corpo. Antes, tudo passava pelo
//! `reqwest`, que reconstruía URL, cabeçalhos e corpo a cada requisição — no benchmark,
//! esse vai-e-volta custava mais da metade da vazão do proxy.
//!
//! Upstreams em `https://` continuam no `reqwest`, que já resolve TLS, verificação de
//! certificado opcional e afins.

use axum::body::{Body, Bytes};
use axum::http::{HeaderMap, Method, Request, Response};
use http_body::{Body as HttpBody, Frame, SizeHint};
use hyper_util::client::legacy::connect::HttpConnector;
use hyper_util::client::legacy::Client;
use hyper_util::rt::TokioExecutor;
use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Duration;
use tokio::time::Sleep;

/// Por que a ida ao upstream falhou — o que decide se o `proxy_next_upstream` tenta o
/// próximo servidor.
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

/// Corpo da requisição enviado ao upstream.
pub enum OutgoingBody {
    Empty,
    /// Bufferizado: pode ser reenviado a outro upstream no failover.
    Buffered(Bytes),
    /// Em streaming: só pode ser enviado uma vez.
    Streaming(Body),
}

/// Quantos pools de conexão independentes cada cliente mantém.
///
/// Cada worker (thread-per-core) usa o seu, escolhido pela identidade da thread. Uma
/// conexão com o upstream é uma tarefa do runtime da thread que a abriu; se outra thread a
/// pegasse do pool, cada requisição voltaria a cruzar threads. Também evita que todos os
/// workers disputem o mutex de um pool único.
const POOL_SHARDS: usize = 64;

thread_local! {
    /// Pool desta thread, distribuído em rodízio na primeira requisição dela.
    static SHARD: usize = {
        static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed) % POOL_SHARDS
    };
}

pub struct UpstreamClient {
    /// Pools criados sob demanda, um por shard.
    http: Box<[std::sync::OnceLock<Client<HttpConnector, Body>>]>,
    connector: HttpConnector,
    https: reqwest::Client,
    read_timeout: Duration,
}

impl UpstreamClient {
    pub fn new(connect_timeout: Duration, read_timeout: Duration, verify_tls: bool) -> Self {
        let mut connector = HttpConnector::new();
        connector.set_connect_timeout(Some(connect_timeout));
        connector.set_nodelay(true);

        let http = (0..POOL_SHARDS)
            .map(|_| std::sync::OnceLock::new())
            .collect();

        let https = reqwest::Client::builder()
            .pool_idle_timeout(Duration::from_secs(90))
            .pool_max_idle_per_host(100)
            .connect_timeout(connect_timeout)
            // Intervalo máximo entre duas leituras, e não tempo total da resposta. É a
            // semântica de `proxy_read_timeout` do nginx, e o que permite atravessar
            // respostas longas e contínuas — SSE, downloads — sem cortá-las.
            .read_timeout(read_timeout)
            .danger_accept_invalid_certs(!verify_tls)
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());

        Self {
            http,
            connector,
            https,
            read_timeout,
        }
    }

    fn http_client(&self) -> &Client<HttpConnector, Body> {
        let shard = SHARD.with(|s| *s);
        self.http[shard].get_or_init(|| {
            Client::builder(TokioExecutor::new())
                .pool_idle_timeout(Duration::from_secs(90))
                .pool_max_idle_per_host(100)
                .build(self.connector.clone())
        })
    }

    pub async fn send(
        &self,
        method: Method,
        url: &str,
        headers: HeaderMap,
        body: OutgoingBody,
    ) -> Result<Response<Body>, UpstreamError> {
        if url.starts_with("http://") {
            self.send_plain(method, url, headers, body).await
        } else {
            self.send_tls(method, url, headers, body).await
        }
    }

    async fn send_plain(
        &self,
        method: Method,
        url: &str,
        headers: HeaderMap,
        body: OutgoingBody,
    ) -> Result<Response<Body>, UpstreamError> {
        let body = match body {
            OutgoingBody::Empty => Body::empty(),
            OutgoingBody::Buffered(b) => Body::from(b),
            OutgoingBody::Streaming(b) => b,
        };
        let mut req = Request::builder()
            .method(method)
            .uri(url)
            .body(body)
            .map_err(|e| UpstreamError::Other(format!("URL de upstream inválida: {}", e)))?;
        *req.headers_mut() = headers;

        // Até os cabeçalhos da resposta chegarem, o `read_timeout` vale como prazo de
        // espera da primeira leitura — o mesmo que o `reqwest` fazia.
        // Prazo calculado pelo relógio em cache: `timeout()` leria o relógio do sistema.
        let deadline = tokio::time::Instant::from_std(crate::clock::now() + self.read_timeout);
        let resp = match tokio::time::timeout_at(deadline, self.http_client().request(req)).await {
            Ok(Ok(r)) => r,
            Ok(Err(e)) => {
                let msg = format!("erro ao falar com o upstream: {}", e);
                return Err(if e.is_connect() && is_timeout_error(&e) {
                    UpstreamError::Timeout(msg)
                } else {
                    UpstreamError::Other(msg)
                });
            }
            Err(_) => {
                return Err(UpstreamError::Timeout(
                    "tempo de resposta do upstream esgotado".to_string(),
                ))
            }
        };

        let (parts, incoming) = resp.into_parts();
        let body = Body::new(ReadTimeoutBody::new(incoming, self.read_timeout));
        Ok(Response::from_parts(parts, body))
    }

    async fn send_tls(
        &self,
        method: Method,
        url: &str,
        headers: HeaderMap,
        body: OutgoingBody,
    ) -> Result<Response<Body>, UpstreamError> {
        let mut req = self.https.request(method, url).headers(headers);
        match body {
            OutgoingBody::Empty => {}
            OutgoingBody::Buffered(b) => req = req.body(b),
            OutgoingBody::Streaming(b) => {
                req = req.body(reqwest::Body::wrap_stream(b.into_data_stream()))
            }
        }

        let res = req.send().await.map_err(|e| {
            if e.is_timeout() {
                UpstreamError::Timeout(e.to_string())
            } else {
                UpstreamError::Other(e.to_string())
            }
        })?;

        let mut builder = Response::builder().status(res.status());
        if let Some(h) = builder.headers_mut() {
            *h = res.headers().clone();
        }
        use futures_util::TryStreamExt;
        let stream = res.bytes_stream().map_err(std::io::Error::other);
        builder
            .body(Body::from_stream(stream))
            .map_err(|e| UpstreamError::Other(e.to_string()))
    }
}

/// Um timeout de conexão do `HttpConnector` aparece como erro de I/O `TimedOut` na cadeia.
fn is_timeout_error(e: &hyper_util::client::legacy::Error) -> bool {
    let mut source = std::error::Error::source(e);
    while let Some(err) = source {
        if let Some(io) = err.downcast_ref::<std::io::Error>() {
            if io.kind() == std::io::ErrorKind::TimedOut {
                return true;
            }
        }
        source = err.source();
    }
    false
}

/// Corpo de resposta com `proxy_read_timeout`: falha se o upstream ficar mais que `dur`
/// sem mandar nada. O prazo recomeça a cada pedaço recebido, então respostas longas e
/// contínuas (SSE, downloads) passam inteiras.
///
/// Rearmar o timer a cada pedaço custaria uma leitura de relógio por pedaço. Em vez
/// disso, cada pedaço só anota a hora (do relógio em cache) e o timer, quando dispara,
/// confere se houve atividade desde então — se houve, rearma a partir dela.
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
                        // Houve atividade depois que o timer foi armado: rearma para o
                        // fim da janela contada a partir dela, e volta a esperar.
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
