//! Minimal HTTP/1.1 client used to talk to upstreams.
//!
//! hyper's pooled client hands every request to a separate connection task through
//! channels, which costs a few task wake-ups per request. Like Pingora, this client
//! drives the connection directly from the request task and keeps idle connections in a
//! per-thread pool: each Ferrix worker runs its own single-threaded runtime, so a
//! connection is only ever reused by the thread (and reactor) that opened it, with no
//! locks involved.

use axum::body::Bytes;
use axum::http::{
    header, HeaderMap, HeaderName, HeaderValue, Method, Response, StatusCode, Version,
};
use bytes::{Buf, BytesMut};
use futures_util::FutureExt;
use http_body::{Body as HttpBody, Frame, SizeHint};
use std::cell::RefCell;
use std::collections::HashMap;
use std::io;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{ready, Context, Poll};
use std::time::{Duration, Instant};
use tokio::io::{AsyncRead, AsyncWrite, AsyncWriteExt, ReadBuf};
use tokio::net::TcpStream;
use tokio_rustls::client::TlsStream;
use tokio_rustls::TlsConnector;

const MAX_HEAD_BYTES: usize = 64 * 1024;
const MAX_HEADERS: usize = 128;
const READ_CHUNK: usize = 16 * 1024;
const MAX_IDLE_PER_HOST: usize = 256;
const IDLE_TIMEOUT: Duration = Duration::from_secs(60);

pub enum Stream {
    Plain(TcpStream),
    Tls(Box<TlsStream<TcpStream>>),
}

impl AsyncRead for Stream {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        match self.get_mut() {
            Stream::Plain(s) => Pin::new(s).poll_read(cx, buf),
            Stream::Tls(s) => Pin::new(s.as_mut()).poll_read(cx, buf),
        }
    }
}

impl AsyncWrite for Stream {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        match self.get_mut() {
            Stream::Plain(s) => Pin::new(s).poll_write(cx, buf),
            Stream::Tls(s) => Pin::new(s.as_mut()).poll_write(cx, buf),
        }
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        match self.get_mut() {
            Stream::Plain(s) => Pin::new(s).poll_flush(cx),
            Stream::Tls(s) => Pin::new(s.as_mut()).poll_flush(cx),
        }
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        match self.get_mut() {
            Stream::Plain(s) => Pin::new(s).poll_shutdown(cx),
            Stream::Tls(s) => Pin::new(s.as_mut()).poll_shutdown(cx),
        }
    }
}

impl Stream {
    fn tcp(&self) -> &TcpStream {
        match self {
            Stream::Plain(s) => s,
            Stream::Tls(s) => s.get_ref().0,
        }
    }

    // An idle keep-alive connection must have nothing to read: EOF or stray bytes mean
    // the upstream closed it (or broke protocol) while it sat in the pool.
    fn looks_alive(&self) -> bool {
        let mut probe = [0u8; 1];
        !self.tcp().peek(&mut probe).now_or_never().is_some()
    }
}

pub struct Conn {
    stream: Stream,
    // Pool this connection goes back to; set once at connect time and carried along so
    // returning it never allocates.
    key: Box<str>,
    insecure: bool,
}

struct Idle {
    conn: Conn,
    since: Instant,
}

thread_local! {
    // [0] plain and verified TLS, [1] TLS with certificate verification disabled, so a
    // connection opened without verification is never handed to a pool that requires it.
    static POOL: RefCell<[HashMap<Box<str>, Vec<Idle>>; 2]> = RefCell::new([HashMap::new(), HashMap::new()]);
}

fn take_idle(key: &str, insecure: bool) -> Option<Conn> {
    POOL.with(|p| {
        let mut pools = p.borrow_mut();
        let list = pools[insecure as usize].get_mut(key)?;
        while let Some(idle) = list.pop() {
            if idle.since.elapsed() < IDLE_TIMEOUT && idle.conn.stream.looks_alive() {
                return Some(idle.conn);
            }
        }
        None
    })
}

fn release(conn: Conn) {
    POOL.with(|p| {
        let mut pools = p.borrow_mut();
        let pool = &mut pools[conn.insecure as usize];
        let list = match pool.get_mut(&*conn.key) {
            Some(l) => l,
            None => pool.entry(conn.key.clone()).or_default(),
        };
        if list.len() < MAX_IDLE_PER_HOST {
            list.push(Idle {
                conn,
                since: Instant::now(),
            });
        }
    });
}

#[derive(Debug)]
pub enum H1Error {
    Connect(io::Error),
    ConnectTimeout,
    // Failed before a single response byte arrived; safe to retry on a fresh connection
    // when the request was sent over a reused one.
    Stale(io::Error),
    Io(io::Error),
    Protocol(&'static str),
}

impl std::fmt::Display for H1Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            H1Error::Connect(e) => write!(f, "connect error: {}", e),
            H1Error::ConnectTimeout => f.write_str("connect timed out"),
            H1Error::Stale(e) | H1Error::Io(e) => write!(f, "{}", e),
            H1Error::Protocol(m) => write!(f, "invalid upstream response: {}", m),
        }
    }
}

impl std::error::Error for H1Error {}

pub struct Target<'a> {
    pub tls: bool,
    pub host: &'a str,
    pub port: u16,
    pub authority: &'a str,
}

impl<'a> Target<'a> {
    // `base` is an upstream target URL such as `http://10.0.0.1:8080` or
    // `https://api.example.com`; anything after the authority is ignored.
    pub fn parse(base: &'a str) -> Option<Self> {
        let (tls, rest) = if let Some(r) = base.strip_prefix("http://") {
            (false, r)
        } else {
            let r = base.strip_prefix("https://")?;
            (true, r)
        };
        let authority = rest.split('/').next().unwrap_or(rest);
        if authority.is_empty() {
            return None;
        }
        let default_port = if tls { 443 } else { 80 };
        let (host, port) = if let Some(v6) = authority.strip_prefix('[') {
            let (h, after) = v6.split_once(']')?;
            let port = match after.strip_prefix(':') {
                Some(p) => p.parse().ok()?,
                None => default_port,
            };
            (h, port)
        } else {
            match authority.rsplit_once(':') {
                Some((h, p)) => (h, p.parse().ok()?),
                None => (authority, default_port),
            }
        };
        Some(Self {
            tls,
            host,
            port,
            authority,
        })
    }
}

pub async fn connect(
    target: &Target<'_>,
    pool_key: &str,
    connect_timeout: Duration,
    tls: Option<(&TlsConnector, bool)>,
) -> Result<Conn, H1Error> {
    let tcp = tokio::time::timeout(
        connect_timeout,
        TcpStream::connect((target.host, target.port)),
    )
    .await
    .map_err(|_| H1Error::ConnectTimeout)?
    .map_err(H1Error::Connect)?;
    let _ = tcp.set_nodelay(true);

    let (stream, insecure) = match (target.tls, tls) {
        (false, _) => (Stream::Plain(tcp), false),
        (true, Some((connector, insecure))) => {
            let name = rustls::pki_types::ServerName::try_from(target.host.to_owned())
                .map_err(|_| H1Error::Protocol("invalid TLS server name"))?;
            let tls = tokio::time::timeout(connect_timeout, connector.connect(name, tcp))
                .await
                .map_err(|_| H1Error::ConnectTimeout)?
                .map_err(H1Error::Connect)?;
            (Stream::Tls(Box::new(tls)), insecure)
        }
        (true, None) => return Err(H1Error::Protocol("TLS upstream without TLS config")),
    };
    Ok(Conn {
        stream,
        key: pool_key.into(),
        insecure,
    })
}

pub fn pooled(pool_key: &str, insecure: bool) -> Option<Conn> {
    take_idle(pool_key, insecure)
}

pub enum RequestBody<'a> {
    Empty,
    Full(&'a Bytes),
    Stream(axum::body::Body, Option<u64>),
}

fn encode_head(
    out: &mut Vec<u8>,
    method: &Method,
    path: &str,
    authority: &str,
    headers: &HeaderMap,
    body: &RequestBody<'_>,
) {
    out.extend_from_slice(method.as_str().as_bytes());
    out.push(b' ');
    out.extend_from_slice(path.as_bytes());
    out.extend_from_slice(b" HTTP/1.1\r\n");
    if !headers.contains_key(header::HOST) {
        out.extend_from_slice(b"host: ");
        out.extend_from_slice(authority.as_bytes());
        out.extend_from_slice(b"\r\n");
    }
    for (name, value) in headers {
        if name == header::CONTENT_LENGTH || name == header::TRANSFER_ENCODING {
            continue;
        }
        out.extend_from_slice(name.as_str().as_bytes());
        out.extend_from_slice(b": ");
        out.extend_from_slice(value.as_bytes());
        out.extend_from_slice(b"\r\n");
    }
    let mut len_buf = itoa_buf();
    match body {
        RequestBody::Empty => {
            if matches!(*method, Method::POST | Method::PUT | Method::PATCH) {
                out.extend_from_slice(b"content-length: 0\r\n");
            }
        }
        RequestBody::Full(b) => {
            out.extend_from_slice(b"content-length: ");
            out.extend_from_slice(fmt_u64(&mut len_buf, b.len() as u64));
            out.extend_from_slice(b"\r\n");
        }
        RequestBody::Stream(_, Some(len)) => {
            out.extend_from_slice(b"content-length: ");
            out.extend_from_slice(fmt_u64(&mut len_buf, *len));
            out.extend_from_slice(b"\r\n");
        }
        RequestBody::Stream(_, None) => out.extend_from_slice(b"transfer-encoding: chunked\r\n"),
    }
    out.extend_from_slice(b"\r\n");
}

fn itoa_buf() -> [u8; 20] {
    [0; 20]
}

fn fmt_u64(buf: &mut [u8; 20], mut n: u64) -> &[u8] {
    let mut i = buf.len();
    loop {
        i -= 1;
        buf[i] = b'0' + (n % 10) as u8;
        n /= 10;
        if n == 0 {
            break;
        }
    }
    &buf[i..]
}

thread_local! {
    static HEAD_BUF: RefCell<Vec<u8>> = RefCell::new(Vec::with_capacity(4096));
}

/// Sends one request over `conn` and reads the response head. On success the connection
/// is owned by the returned body, which puts it back in the pool once fully read.
pub async fn exchange(
    mut conn: Conn,
    method: &Method,
    path: &str,
    authority: &str,
    headers: &HeaderMap,
    body: RequestBody<'_>,
) -> Result<Response<H1Body>, H1Error> {
    // The encode buffer is borrowed from the thread for the duration of the write and
    // handed back afterwards, so steady-state requests don't allocate it.
    let mut head = HEAD_BUF.with(|b| std::mem::take(&mut *b.borrow_mut()));
    head.clear();
    encode_head(&mut head, method, path, authority, headers, &body);
    if let RequestBody::Full(bytes) = &body {
        if bytes.len() <= 16 * 1024 {
            head.extend_from_slice(bytes);
        }
    }
    let written = conn.stream.write_all(&head).await;
    HEAD_BUF.with(|b| *b.borrow_mut() = head);
    written.map_err(H1Error::Stale)?;

    match body {
        RequestBody::Empty => {}
        RequestBody::Full(bytes) => {
            if bytes.len() > 16 * 1024 {
                conn.stream.write_all(bytes).await.map_err(H1Error::Stale)?;
            }
        }
        RequestBody::Stream(body, len) => {
            write_streaming(&mut conn.stream, body, len.is_none()).await?
        }
    }
    conn.stream.flush().await.map_err(H1Error::Io)?;

    let mut buf = BytesMut::with_capacity(READ_CHUNK);
    let (parts, kind, keep_alive) = read_head(&mut conn.stream, &mut buf, method).await?;

    let mut body = H1Body {
        conn: Some(conn),
        buf,
        state: kind,
        keep_alive,
    };
    if body.state.is_done() {
        body.finish();
    }
    Ok(Response::from_parts(parts, body))
}

async fn write_streaming(
    stream: &mut Stream,
    body: axum::body::Body,
    chunked: bool,
) -> Result<(), H1Error> {
    use futures_util::StreamExt;
    let mut data = body.into_data_stream();
    while let Some(chunk) = data.next().await {
        let chunk = chunk.map_err(|e| H1Error::Io(io::Error::other(e)))?;
        if chunk.is_empty() {
            continue;
        }
        if chunked {
            let size = format!("{:x}\r\n", chunk.len());
            stream
                .write_all(size.as_bytes())
                .await
                .map_err(H1Error::Io)?;
            stream.write_all(&chunk).await.map_err(H1Error::Io)?;
            stream.write_all(b"\r\n").await.map_err(H1Error::Io)?;
        } else {
            stream.write_all(&chunk).await.map_err(H1Error::Io)?;
        }
    }
    if chunked {
        stream.write_all(b"0\r\n\r\n").await.map_err(H1Error::Io)?;
    }
    Ok(())
}

async fn read_head(
    stream: &mut Stream,
    buf: &mut BytesMut,
    method: &Method,
) -> Result<(axum::http::response::Parts, BodyState, bool), H1Error> {
    use tokio::io::AsyncReadExt;
    let mut received_any = false;
    loop {
        if !buf.is_empty() {
            match parse_head(buf, method)? {
                Some(Parsed::Informational) => continue,
                Some(Parsed::Final(parts, state, keep_alive)) => {
                    return Ok((parts, state, keep_alive))
                }
                None => {}
            }
        }
        if buf.len() >= MAX_HEAD_BYTES {
            return Err(H1Error::Protocol("response head too large"));
        }
        buf.reserve(READ_CHUNK);
        let n = stream.read_buf(buf).await.map_err(|e| {
            if received_any {
                H1Error::Io(e)
            } else {
                H1Error::Stale(e)
            }
        })?;
        if n == 0 {
            let eof = io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "upstream closed the connection",
            );
            return Err(if received_any {
                H1Error::Io(eof)
            } else {
                H1Error::Stale(eof)
            });
        }
        received_any = true;
    }
}

enum Parsed {
    Informational,
    Final(axum::http::response::Parts, BodyState, bool),
}

fn parse_head(buf: &mut BytesMut, method: &Method) -> Result<Option<Parsed>, H1Error> {
    // Header names/values are recorded as offsets so the head can be frozen into one
    // shared buffer and sliced, instead of copying every value into its own allocation.
    let mut spans = [(0usize, 0usize, 0usize, 0usize); MAX_HEADERS];
    let (len, code, minor, count) = {
        let mut headers = [httparse::EMPTY_HEADER; MAX_HEADERS];
        let mut res = httparse::Response::new(&mut headers);
        let base = buf.as_ptr() as usize;
        let len = match res.parse(buf) {
            Ok(httparse::Status::Complete(n)) => n,
            Ok(httparse::Status::Partial) => return Ok(None),
            Err(httparse::Error::TooManyHeaders) => {
                return Err(H1Error::Protocol("too many response headers"))
            }
            Err(_) => return Err(H1Error::Protocol("malformed response head")),
        };
        let mut count = 0;
        for h in res.headers.iter() {
            let n = h.name.as_ptr() as usize - base;
            let v = h.value.as_ptr() as usize - base;
            spans[count] = (n, n + h.name.len(), v, v + h.value.len());
            count += 1;
        }
        (len, res.code.unwrap_or(0), res.version.unwrap_or(1), count)
    };

    let head = buf.split_to(len).freeze();
    if (100..200).contains(&code) && code != 101 {
        return Ok(Some(Parsed::Informational));
    }
    if code == 101 {
        return Err(H1Error::Protocol("unexpected protocol upgrade"));
    }

    let mut map = HeaderMap::with_capacity(count);
    let mut content_length: Option<u64> = None;
    let mut chunked = false;
    let mut conn_close = minor == 0;
    for &(ns, ne, vs, ve) in &spans[..count] {
        let name = HeaderName::from_bytes(&head[ns..ne])
            .map_err(|_| H1Error::Protocol("invalid header name"))?;
        let value = HeaderValue::from_maybe_shared(head.slice(vs..ve))
            .map_err(|_| H1Error::Protocol("invalid header value"))?;
        if name == header::CONTENT_LENGTH {
            let parsed = value
                .to_str()
                .ok()
                .and_then(|v| v.trim().parse::<u64>().ok())
                .ok_or(H1Error::Protocol("invalid content-length"))?;
            if content_length.is_some_and(|c| c != parsed) {
                return Err(H1Error::Protocol("conflicting content-length"));
            }
            content_length = Some(parsed);
        } else if name == header::TRANSFER_ENCODING {
            chunked = value
                .to_str()
                .ok()
                .and_then(|v| v.rsplit(',').next())
                .is_some_and(|last| last.trim().eq_ignore_ascii_case("chunked"));
        } else if name == header::CONNECTION {
            let v = value.to_str().unwrap_or("");
            for token in v.split(',').map(str::trim) {
                if token.eq_ignore_ascii_case("close") {
                    conn_close = true;
                } else if token.eq_ignore_ascii_case("keep-alive") && minor == 0 {
                    conn_close = false;
                }
            }
        }
        map.append(name, value);
    }

    let status = StatusCode::from_u16(code).map_err(|_| H1Error::Protocol("invalid status"))?;
    let no_body = *method == Method::HEAD
        || status == StatusCode::NO_CONTENT
        || status == StatusCode::NOT_MODIFIED;

    let state = if no_body {
        BodyState::Done
    } else if chunked {
        map.remove(header::CONTENT_LENGTH);
        BodyState::Chunked(Chunk::Size)
    } else if let Some(len) = content_length {
        if len == 0 {
            BodyState::Done
        } else {
            BodyState::Length(len)
        }
    } else {
        conn_close = true;
        BodyState::UntilClose
    };

    let mut parts = Response::new(()).into_parts().0;
    parts.status = status;
    parts.version = Version::HTTP_11;
    parts.headers = map;
    Ok(Some(Parsed::Final(parts, state, !conn_close)))
}

#[derive(Debug, Clone, Copy)]
enum Chunk {
    Size,
    Data(u64),
    DataEnd,
    Trailers,
}

#[derive(Debug, Clone, Copy)]
enum BodyState {
    Length(u64),
    Chunked(Chunk),
    UntilClose,
    Done,
}

impl BodyState {
    fn is_done(&self) -> bool {
        matches!(self, BodyState::Done)
    }
}

pub struct H1Body {
    conn: Option<Conn>,
    buf: BytesMut,
    state: BodyState,
    keep_alive: bool,
}

impl H1Body {
    fn finish(&mut self) {
        self.state = BodyState::Done;
        if let Some(conn) = self.conn.take() {
            // Leftover bytes would belong to a response nobody asked for.
            if self.keep_alive && self.buf.is_empty() {
                release(conn);
            }
        }
    }

    fn poll_fill(&mut self, cx: &mut Context<'_>) -> Poll<io::Result<usize>> {
        let Some(conn) = self.conn.as_mut() else {
            return Poll::Ready(Ok(0));
        };
        if self.buf.capacity() - self.buf.len() < 1024 {
            self.buf.reserve(READ_CHUNK);
        }
        tokio_util::io::poll_read_buf(Pin::new(&mut conn.stream), cx, &mut self.buf)
    }

    fn take_data(&mut self, max: u64) -> Bytes {
        let n = (self.buf.len() as u64).min(max) as usize;
        self.buf.split_to(n).freeze()
    }
}

fn find_crlf(buf: &[u8]) -> Option<usize> {
    buf.windows(2).position(|w| w == b"\r\n")
}

fn eof() -> io::Error {
    io::Error::new(
        io::ErrorKind::UnexpectedEof,
        "upstream closed the connection mid-body",
    )
}

impl HttpBody for H1Body {
    type Data = Bytes;
    type Error = io::Error;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, io::Error>>> {
        let this = &mut *self;
        loop {
            match this.state {
                BodyState::Done => return Poll::Ready(None),
                BodyState::Length(remaining) => {
                    if !this.buf.is_empty() {
                        let data = this.take_data(remaining);
                        let left = remaining - data.len() as u64;
                        if left == 0 {
                            this.finish();
                        } else {
                            this.state = BodyState::Length(left);
                        }
                        return Poll::Ready(Some(Ok(Frame::data(data))));
                    }
                    if ready!(this.poll_fill(cx))? == 0 {
                        return Poll::Ready(Some(Err(eof())));
                    }
                }
                BodyState::UntilClose => {
                    if !this.buf.is_empty() {
                        let data = this.take_data(u64::MAX);
                        return Poll::Ready(Some(Ok(Frame::data(data))));
                    }
                    if ready!(this.poll_fill(cx))? == 0 {
                        this.conn = None;
                        this.state = BodyState::Done;
                        return Poll::Ready(None);
                    }
                }
                BodyState::Chunked(chunk) => {
                    match chunk {
                        Chunk::Size => {
                            if let Some(end) = find_crlf(&this.buf) {
                                let line = &this.buf[..end];
                                let hex = line.split(|&b| b == b';').next().unwrap_or(line);
                                let size = std::str::from_utf8(hex)
                                    .ok()
                                    .and_then(|s| u64::from_str_radix(s.trim(), 16).ok())
                                    .ok_or_else(|| {
                                        io::Error::new(io::ErrorKind::InvalidData, "bad chunk size")
                                    })?;
                                this.buf.advance(end + 2);
                                this.state = BodyState::Chunked(if size == 0 {
                                    Chunk::Trailers
                                } else {
                                    Chunk::Data(size)
                                });
                                continue;
                            }
                            if this.buf.len() > 4096 {
                                return Poll::Ready(Some(Err(io::Error::new(
                                    io::ErrorKind::InvalidData,
                                    "chunk size line too long",
                                ))));
                            }
                        }
                        Chunk::Data(remaining) => {
                            if !this.buf.is_empty() {
                                let data = this.take_data(remaining);
                                let left = remaining - data.len() as u64;
                                this.state = BodyState::Chunked(if left == 0 {
                                    Chunk::DataEnd
                                } else {
                                    Chunk::Data(left)
                                });
                                return Poll::Ready(Some(Ok(Frame::data(data))));
                            }
                        }
                        Chunk::DataEnd => {
                            if this.buf.len() >= 2 {
                                if &this.buf[..2] != b"\r\n" {
                                    return Poll::Ready(Some(Err(io::Error::new(
                                        io::ErrorKind::InvalidData,
                                        "missing CRLF after chunk",
                                    ))));
                                }
                                this.buf.advance(2);
                                this.state = BodyState::Chunked(Chunk::Size);
                                continue;
                            }
                        }
                        Chunk::Trailers => {
                            if let Some(end) = find_crlf(&this.buf) {
                                this.buf.advance(end + 2);
                                if end == 0 {
                                    this.finish();
                                    return Poll::Ready(None);
                                }
                                continue;
                            }
                        }
                    }
                    if ready!(this.poll_fill(cx))? == 0 {
                        return Poll::Ready(Some(Err(eof())));
                    }
                }
            }
        }
    }

    fn is_end_stream(&self) -> bool {
        self.state.is_done()
    }

    fn size_hint(&self) -> SizeHint {
        match self.state {
            BodyState::Done => SizeHint::with_exact(0),
            BodyState::Length(n) => SizeHint::with_exact(n),
            _ => SizeHint::default(),
        }
    }
}

pub fn tls_connector(verify: bool) -> TlsConnector {
    let builder = rustls::ClientConfig::builder();
    let mut config = if verify {
        let mut roots = rustls::RootCertStore::empty();
        roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
        builder.with_root_certificates(roots).with_no_client_auth()
    } else {
        builder
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(NoVerify::new()))
            .with_no_client_auth()
    };
    config.alpn_protocols = vec![b"http/1.1".to_vec()];
    TlsConnector::from(Arc::new(config))
}

// Used only when an upstream sets `verify_tls: false` (self-signed internal services),
// mirroring nginx's default of not verifying upstream certificates.
#[derive(Debug)]
struct NoVerify(Arc<rustls::crypto::CryptoProvider>);

impl NoVerify {
    fn new() -> Self {
        Self(Arc::new(rustls::crypto::ring::default_provider()))
    }
}

impl rustls::client::danger::ServerCertVerifier for NoVerify {
    fn verify_server_cert(
        &self,
        _end_entity: &rustls::pki_types::CertificateDer<'_>,
        _intermediates: &[rustls::pki_types::CertificateDer<'_>],
        _server_name: &rustls::pki_types::ServerName<'_>,
        _ocsp_response: &[u8],
        _now: rustls::pki_types::UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &rustls::pki_types::CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(
            message,
            cert,
            dss,
            &self.0.signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &rustls::pki_types::CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            dss,
            &self.0.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        self.0.signature_verification_algorithms.supported_schemes()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::AsyncReadExt;
    use tokio::net::TcpListener;

    #[test]
    fn target_parsing() {
        let t = Target::parse("http://10.0.0.1:8080").unwrap();
        assert_eq!(
            (t.tls, t.host, t.port, t.authority),
            (false, "10.0.0.1", 8080, "10.0.0.1:8080")
        );
        let t = Target::parse("https://api.example.com/").unwrap();
        assert_eq!((t.tls, t.host, t.port), (true, "api.example.com", 443));
        let t = Target::parse("http://[::1]:9000").unwrap();
        assert_eq!((t.host, t.port), ("::1", 9000));
        assert!(Target::parse("ftp://x").is_none());
    }

    #[test]
    fn head_encoding_sets_framing() {
        let mut out = Vec::new();
        let mut h = HeaderMap::new();
        h.insert("x-a", HeaderValue::from_static("1"));
        h.insert(header::CONTENT_LENGTH, HeaderValue::from_static("999"));
        let body = Bytes::from_static(b"hello");
        encode_head(
            &mut out,
            &Method::POST,
            "/p?q=1",
            "up:80",
            &h,
            &RequestBody::Full(&body),
        );
        let s = String::from_utf8(out).unwrap();
        assert!(s.starts_with("POST /p?q=1 HTTP/1.1\r\nhost: up:80\r\n"));
        assert!(s.contains("x-a: 1\r\n"));
        assert!(s.contains("content-length: 5\r\n"));
        assert!(!s.contains("999"));
        assert!(s.ends_with("\r\n\r\n"));
    }

    async fn serve_once(
        responses: Vec<&'static [u8]>,
    ) -> (String, tokio::task::JoinHandle<Vec<String>>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let handle = tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.unwrap();
            let mut seen = Vec::new();
            for resp in responses {
                let mut buf = vec![0u8; 4096];
                let n = sock.read(&mut buf).await.unwrap();
                seen.push(String::from_utf8_lossy(&buf[..n]).into_owned());
                sock.write_all(resp).await.unwrap();
            }
            // Keep the connection open for a moment so the client can pool it.
            let mut rest = [0u8; 1];
            let _ = tokio::time::timeout(Duration::from_millis(100), sock.read(&mut rest)).await;
            seen
        });
        (format!("http://{}", addr), handle)
    }

    async fn body_of(resp: Response<H1Body>) -> Vec<u8> {
        use http_body_util::BodyExt;
        resp.into_body()
            .collect()
            .await
            .unwrap()
            .to_bytes()
            .to_vec()
    }

    #[tokio::test]
    async fn content_length_body_and_connection_reuse() {
        let (base, server) = serve_once(vec![
            b"HTTP/1.1 200 OK\r\ncontent-length: 5\r\nx-up: a\r\n\r\nhello",
            b"HTTP/1.1 200 OK\r\ncontent-length: 2\r\n\r\nhi",
        ])
        .await;
        let t = Target::parse(&base).unwrap();
        let conn = connect(&t, &base, Duration::from_secs(1), None)
            .await
            .unwrap();
        let resp = exchange(
            conn,
            &Method::GET,
            "/a",
            t.authority,
            &HeaderMap::new(),
            RequestBody::Empty,
        )
        .await
        .unwrap();
        assert_eq!(resp.headers()["x-up"], "a");
        assert_eq!(body_of(resp).await, b"hello");

        let conn = pooled(&base, false).expect("connection should be back in the pool");
        let resp = exchange(
            conn,
            &Method::GET,
            "/b",
            t.authority,
            &HeaderMap::new(),
            RequestBody::Empty,
        )
        .await
        .unwrap();
        assert_eq!(body_of(resp).await, b"hi");
        let seen = server.await.unwrap();
        assert!(seen[1].starts_with("GET /b HTTP/1.1\r\n"));
    }

    #[tokio::test]
    async fn chunked_body_is_decoded() {
        let (base, _server) = serve_once(vec![
            b"HTTP/1.1 200 OK\r\ntransfer-encoding: chunked\r\n\r\n5\r\nhello\r\n6;ext=1\r\n world\r\n0\r\n\r\n",
        ])
        .await;
        let t = Target::parse(&base).unwrap();
        let conn = connect(&t, &base, Duration::from_secs(1), None)
            .await
            .unwrap();
        let resp = exchange(
            conn,
            &Method::GET,
            "/",
            t.authority,
            &HeaderMap::new(),
            RequestBody::Empty,
        )
        .await
        .unwrap();
        assert_eq!(body_of(resp).await, b"hello world");
        assert!(pooled(&base, false).is_some());
    }

    #[tokio::test]
    async fn informational_responses_are_skipped_and_close_is_honoured() {
        let (base, _server) = serve_once(vec![
            b"HTTP/1.1 100 Continue\r\n\r\nHTTP/1.1 201 Created\r\nconnection: close\r\ncontent-length: 2\r\n\r\nok",
        ])
        .await;
        let t = Target::parse(&base).unwrap();
        let conn = connect(&t, &base, Duration::from_secs(1), None)
            .await
            .unwrap();
        let body = Bytes::from_static(b"x");
        let resp = exchange(
            conn,
            &Method::POST,
            "/",
            t.authority,
            &HeaderMap::new(),
            RequestBody::Full(&body),
        )
        .await
        .unwrap();
        assert_eq!(resp.status(), StatusCode::CREATED);
        assert_eq!(body_of(resp).await, b"ok");
        assert!(pooled(&base, false).is_none());
    }

    #[tokio::test]
    async fn body_until_close() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.unwrap();
            let mut buf = vec![0u8; 1024];
            let _ = sock.read(&mut buf).await;
            sock.write_all(b"HTTP/1.0 200 OK\r\n\r\nstreamed")
                .await
                .unwrap();
        });
        let t = Target::parse(&base).unwrap();
        let conn = connect(&t, &base, Duration::from_secs(1), None)
            .await
            .unwrap();
        let resp = exchange(
            conn,
            &Method::GET,
            "/",
            t.authority,
            &HeaderMap::new(),
            RequestBody::Empty,
        )
        .await
        .unwrap();
        assert_eq!(body_of(resp).await, b"streamed");
    }

    #[tokio::test]
    async fn closed_idle_connection_is_not_reused() {
        let (base, server) = serve_once(vec![b"HTTP/1.1 204 No Content\r\n\r\n"]).await;
        let t = Target::parse(&base).unwrap();
        let conn = connect(&t, &base, Duration::from_secs(1), None)
            .await
            .unwrap();
        let resp = exchange(
            conn,
            &Method::GET,
            "/",
            t.authority,
            &HeaderMap::new(),
            RequestBody::Empty,
        )
        .await
        .unwrap();
        assert_eq!(resp.status(), StatusCode::NO_CONTENT);
        server.await.unwrap();
        // The server task has dropped its socket by now.
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert!(pooled(&base, false).is_none());
    }
}
