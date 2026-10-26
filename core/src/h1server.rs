//! Fast path for plain-HTTP/1.1 connections.
//!
//! Most proxy traffic is bodiless HTTP/1.1 requests on keep-alive connections. For those,
//! Ferrix parses and writes HTTP itself, which lets it:
//!
//! - send static files with `sendfile(2)` (zero-copy from the page cache, like nginx's
//!   `sendfile on`), which hyper can't do because it owns the socket writes;
//! - avoid hyper's per-poll `SystemTime::now()` (a syscall on hosts without a TSC);
//! - write the response head and a small body with a single syscall.
//!
//! Anything less common (request bodies, `Expect`, protocol upgrades such as WebSocket,
//! HTTP/2 prior knowledge) hands the connection to hyper, replaying the bytes already
//! read, so behaviour there is unchanged.

use axum::body::{Body, Bytes};
use axum::http::{
    header, HeaderMap, HeaderName, HeaderValue, Method, Request, StatusCode, Uri, Version,
};
use bytes::{Buf, BytesMut};
use http_body::Body as _;
use http_body_util::BodyExt;
use std::io;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, ReadBuf};
use tokio::net::TcpStream;

use crate::proxy::ProxyEngine;

const MAX_HEAD: usize = 64 * 1024;
const MAX_HEADERS: usize = 100;
// Bodies up to this size are copied after the head so both go out in one write.
const COALESCE_LIMIT: usize = 32 * 1024;

/// Response extension asking the plain-HTTP server to send this byte range of a file
/// with `sendfile(2)` instead of writing the response body. The body is still filled in,
/// so TLS and HTTP/2 connections (served by hyper) keep working unchanged.
#[derive(Debug, Clone)]
pub struct SendFile {
    pub path: PathBuf,
    pub offset: u64,
    pub len: u64,
}

/// Files at least this large are worth a `sendfile` (smaller ones are cheaper to copy
/// from the in-memory cache along with the head).
pub const SENDFILE_MIN: u64 = 32 * 1024;

pub struct Settings {
    pub header_timeout: Duration,
    pub keepalive_timeout: Duration,
}

pub enum Outcome {
    Done,
    /// The connection needs hyper; `prefix` holds the bytes already read from it.
    Handoff(TcpStream, Bytes),
}

pub async fn serve(
    mut tcp: TcpStream,
    addr: SocketAddr,
    engine: &ProxyEngine,
    port: u16,
    settings: &Settings,
) -> Outcome {
    let mut buf = BytesMut::with_capacity(8 * 1024);
    let mut out: Vec<u8> = Vec::with_capacity(1024);
    let mut first = true;

    loop {
        // Wait for (the rest of) a request head.
        let wait = if first && buf.is_empty() {
            settings.header_timeout
        } else if buf.is_empty() {
            settings.keepalive_timeout
        } else {
            settings.header_timeout
        };
        let head = match read_head(&mut tcp, &mut buf, wait).await {
            Ok(Some(h)) => h,
            Ok(None) => return Outcome::Done,
            Err(HeadError::Bad(status)) => {
                let _ = write_error(&mut tcp, status).await;
                return Outcome::Done;
            }
            Err(HeadError::Handoff) => return Outcome::Handoff(tcp, buf.freeze()),
        };
        first = false;

        if head.needs_hyper {
            // Put the head back in front of whatever else was buffered.
            let mut prefix = BytesMut::with_capacity(head.raw.len() + buf.len());
            prefix.extend_from_slice(&head.raw);
            prefix.extend_from_slice(&buf);
            return Outcome::Handoff(tcp, prefix.freeze());
        }

        let keep_alive = head.keep_alive;
        let is_head = head.method == Method::HEAD;
        let http10 = head.version == Version::HTTP_10;
        let mut req = Request::new(Body::empty());
        *req.method_mut() = head.method;
        *req.uri_mut() = head.uri;
        *req.version_mut() = head.version;
        *req.headers_mut() = head.headers;

        let resp = engine.handle_request(port, false, addr, req).await;
        if write_response(&mut tcp, &mut out, resp, is_head, http10, keep_alive)
            .await
            .is_err()
            || !keep_alive
        {
            let _ = tcp.shutdown().await;
            return Outcome::Done;
        }
    }
}

struct Head {
    method: Method,
    uri: Uri,
    version: Version,
    headers: HeaderMap,
    keep_alive: bool,
    needs_hyper: bool,
    raw: Bytes,
}

enum HeadError {
    Bad(StatusCode),
    Handoff,
}

async fn read_head(
    tcp: &mut TcpStream,
    buf: &mut BytesMut,
    wait: Duration,
) -> Result<Option<Head>, HeadError> {
    let deadline = crate::clock::mono_ms() + wait.as_millis() as u64;
    loop {
        if !buf.is_empty() {
            // HTTP/2 with prior knowledge: let hyper speak it.
            if buf.starts_with(b"PRI * HTTP/2.0")
                || (buf.len() < 14 && b"PRI * HTTP/2.0".starts_with(&buf[..]))
            {
                if buf.len() >= 14 {
                    return Err(HeadError::Handoff);
                }
            } else if let Some(head) = parse(buf)? {
                return Ok(Some(head));
            }
        }
        if buf.len() >= MAX_HEAD {
            return Err(HeadError::Bad(StatusCode::REQUEST_HEADER_FIELDS_TOO_LARGE));
        }
        buf.reserve(8 * 1024);
        let read = crate::clock::timeout_at_ms(deadline, tcp.read_buf(buf)).await;
        match read {
            Ok(Ok(0)) | Ok(Err(_)) => return Ok(None),
            Ok(Ok(_)) => {}
            // Idle keep-alive connection or slow client: just close it.
            Err(_) => return Ok(None),
        }
    }
}

fn parse(buf: &mut BytesMut) -> Result<Option<Head>, HeadError> {
    let mut spans = [(0usize, 0usize, 0usize, 0usize); MAX_HEADERS];
    let (len, method, path, minor, count) = {
        let mut headers = [httparse::EMPTY_HEADER; MAX_HEADERS];
        let mut req = httparse::Request::new(&mut headers);
        let base = buf.as_ptr() as usize;
        let len = match req.parse(buf) {
            Ok(httparse::Status::Complete(n)) => n,
            Ok(httparse::Status::Partial) => return Ok(None),
            Err(httparse::Error::TooManyHeaders) => {
                return Err(HeadError::Bad(StatusCode::REQUEST_HEADER_FIELDS_TOO_LARGE))
            }
            Err(_) => return Err(HeadError::Bad(StatusCode::BAD_REQUEST)),
        };
        let mut count = 0;
        for h in req.headers.iter() {
            let n = h.name.as_ptr() as usize - base;
            let v = h.value.as_ptr() as usize - base;
            spans[count] = (n, n + h.name.len(), v, v + h.value.len());
            count += 1;
        }
        let method = Method::from_bytes(req.method.unwrap_or("").as_bytes())
            .map_err(|_| HeadError::Bad(StatusCode::BAD_REQUEST))?;
        let path = req.path.unwrap_or("/");
        let p = path.as_ptr() as usize - base;
        (
            len,
            method,
            (p, p + path.len()),
            req.version.unwrap_or(1),
            count,
        )
    };

    let raw = buf.split_to(len).freeze();
    let uri = Uri::from_maybe_shared(raw.slice(path.0..path.1))
        .map_err(|_| HeadError::Bad(StatusCode::BAD_REQUEST))?;

    let mut headers = HeaderMap::with_capacity(count);
    let mut keep_alive = minor == 1;
    let mut needs_hyper = false;
    for &(ns, ne, vs, ve) in &spans[..count] {
        let name = HeaderName::from_bytes(&raw[ns..ne])
            .map_err(|_| HeadError::Bad(StatusCode::BAD_REQUEST))?;
        let value = HeaderValue::from_maybe_shared(raw.slice(vs..ve))
            .map_err(|_| HeadError::Bad(StatusCode::BAD_REQUEST))?;
        if name == header::CONTENT_LENGTH {
            if value.as_bytes() != b"0" {
                needs_hyper = true;
            }
        } else if name == header::TRANSFER_ENCODING
            || name == header::UPGRADE
            || name == header::EXPECT
        {
            needs_hyper = true;
        } else if name == header::CONNECTION {
            for token in value.to_str().unwrap_or("").split(',').map(str::trim) {
                if token.eq_ignore_ascii_case("close") {
                    keep_alive = false;
                } else if token.eq_ignore_ascii_case("keep-alive") {
                    keep_alive = true;
                } else if token.eq_ignore_ascii_case("upgrade") {
                    needs_hyper = true;
                }
            }
        }
        headers.append(name, value);
    }

    Ok(Some(Head {
        method,
        uri,
        version: if minor == 0 {
            Version::HTTP_10
        } else {
            Version::HTTP_11
        },
        headers,
        keep_alive,
        needs_hyper,
        raw,
    }))
}

fn push_u64(out: &mut Vec<u8>, n: u64) {
    let mut buf = [0u8; 20];
    let mut i = buf.len();
    let mut n = n;
    loop {
        i -= 1;
        buf[i] = b'0' + (n % 10) as u8;
        n /= 10;
        if n == 0 {
            break;
        }
    }
    out.extend_from_slice(&buf[i..]);
}

#[derive(Clone, Copy)]
enum Framing {
    Length(u64),
    Chunked,
    /// No length header at all: bodiless statuses, HEAD of an unknown-size response, or
    /// an HTTP/1.0 body delimited by closing the connection.
    None,
}

fn encode_head(
    out: &mut Vec<u8>,
    status: StatusCode,
    headers: &HeaderMap,
    framing: Framing,
    keep_alive: bool,
) {
    out.extend_from_slice(b"HTTP/1.1 ");
    out.extend_from_slice(status.as_str().as_bytes());
    out.push(b' ');
    out.extend_from_slice(status.canonical_reason().unwrap_or("").as_bytes());
    out.extend_from_slice(b"\r\n");
    for (name, value) in headers {
        if name == header::CONTENT_LENGTH
            || name == header::TRANSFER_ENCODING
            || name == header::CONNECTION
        {
            continue;
        }
        out.extend_from_slice(name.as_str().as_bytes());
        out.extend_from_slice(b": ");
        out.extend_from_slice(value.as_bytes());
        out.extend_from_slice(b"\r\n");
    }
    if !headers.contains_key(header::DATE) {
        out.extend_from_slice(b"date: ");
        out.extend_from_slice(crate::clock::http_date().as_bytes());
        out.extend_from_slice(b"\r\n");
    }
    match framing {
        Framing::Length(n) => {
            out.extend_from_slice(b"content-length: ");
            push_u64(out, n);
            out.extend_from_slice(b"\r\n");
        }
        Framing::Chunked => out.extend_from_slice(b"transfer-encoding: chunked\r\n"),
        Framing::None => {}
    }
    if !keep_alive {
        out.extend_from_slice(b"connection: close\r\n");
    }
    out.extend_from_slice(b"\r\n");
}

fn bodiless(status: StatusCode) -> bool {
    status.is_informational()
        || status == StatusCode::NO_CONTENT
        || status == StatusCode::NOT_MODIFIED
}

async fn write_response(
    tcp: &mut TcpStream,
    out: &mut Vec<u8>,
    resp: axum::response::Response,
    is_head: bool,
    http10: bool,
    keep_alive: bool,
) -> io::Result<()> {
    let (parts, mut body) = resp.into_parts();
    let status = parts.status;
    out.clear();

    if bodiless(status) {
        encode_head(out, status, &parts.headers, Framing::None, keep_alive);
        return tcp.write_all(out).await;
    }

    let declared = parts
        .headers
        .get(header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<u64>().ok());

    if is_head {
        let framing = match declared.or(body.size_hint().exact()) {
            Some(n) => Framing::Length(n),
            None => Framing::None,
        };
        encode_head(out, status, &parts.headers, framing, keep_alive);
        return tcp.write_all(out).await;
    }

    if let Some(sf) = parts.extensions.get::<SendFile>() {
        encode_head(
            out,
            status,
            &parts.headers,
            Framing::Length(sf.len),
            keep_alive,
        );
        // nginx's `tcp_nopush`: hold partial frames while the head and the file go out,
        // so the head shares full-sized packets with the file instead of travelling
        // alone; uncorking at the end flushes whatever is left.
        set_cork(tcp, true);
        let sent = async {
            tcp.write_all(out).await?;
            sendfile(tcp, sf).await
        }
        .await;
        set_cork(tcp, false);
        return sent;
    }

    match body.size_hint().exact() {
        Some(len) => {
            encode_head(
                out,
                status,
                &parts.headers,
                Framing::Length(len),
                keep_alive,
            );
            if len as usize <= COALESCE_LIMIT {
                while let Some(frame) = body.frame().await {
                    if let Ok(data) = frame.map_err(io::Error::other)?.into_data() {
                        out.extend_from_slice(&data);
                    }
                }
                return tcp.write_all(out).await;
            }
            tcp.write_all(out).await?;
            while let Some(frame) = body.frame().await {
                if let Ok(data) = frame.map_err(io::Error::other)?.into_data() {
                    tcp.write_all(&data).await?;
                }
            }
            Ok(())
        }
        None if http10 => {
            // HTTP/1.0 has no chunked encoding: the body ends when the connection does.
            encode_head(out, status, &parts.headers, Framing::None, false);
            tcp.write_all(out).await?;
            while let Some(frame) = body.frame().await {
                if let Ok(data) = frame.map_err(io::Error::other)?.into_data() {
                    tcp.write_all(&data).await?;
                }
            }
            Err(io::Error::other("close after HTTP/1.0 body"))
        }
        None => {
            encode_head(out, status, &parts.headers, Framing::Chunked, keep_alive);
            tcp.write_all(out).await?;
            while let Some(frame) = body.frame().await {
                let Ok(data) = frame.map_err(io::Error::other)?.into_data() else {
                    continue;
                };
                if data.is_empty() {
                    continue;
                }
                let size = format!("{:x}\r\n", data.len());
                tcp.write_all(size.as_bytes()).await?;
                tcp.write_all(&data).await?;
                tcp.write_all(b"\r\n").await?;
            }
            tcp.write_all(b"0\r\n\r\n").await
        }
    }
}

async fn write_error(tcp: &mut TcpStream, status: StatusCode) -> io::Result<()> {
    let mut out = Vec::new();
    encode_head(
        &mut out,
        status,
        &HeaderMap::new(),
        Framing::Length(0),
        false,
    );
    tcp.write_all(&out).await
}

#[cfg(target_os = "linux")]
fn set_cork(tcp: &TcpStream, on: bool) {
    use std::os::fd::AsRawFd;
    let v: libc::c_int = on as libc::c_int;
    unsafe {
        libc::setsockopt(
            tcp.as_raw_fd(),
            libc::IPPROTO_TCP,
            libc::TCP_CORK,
            &v as *const _ as *const libc::c_void,
            std::mem::size_of::<libc::c_int>() as libc::socklen_t,
        );
    }
}

#[cfg(not(target_os = "linux"))]
fn set_cork(_tcp: &TcpStream, _on: bool) {}

#[cfg(target_os = "linux")]
async fn sendfile(tcp: &mut TcpStream, sf: &SendFile) -> io::Result<()> {
    use std::os::fd::AsRawFd;
    let file = std::fs::File::open(&sf.path)?;
    let mut offset = sf.offset as libc::off_t;
    let end = (sf.offset + sf.len) as libc::off_t;
    while offset < end {
        let want = (end - offset).min(1 << 30) as usize;
        let res = tcp.try_io(tokio::io::Interest::WRITABLE, || {
            let n = unsafe { libc::sendfile(tcp.as_raw_fd(), file.as_raw_fd(), &mut offset, want) };
            if n < 0 {
                Err(io::Error::last_os_error())
            } else {
                Ok(n as usize)
            }
        });
        match res {
            Ok(0) => {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "file shrank while being sent",
                ))
            }
            Ok(_) => {}
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => tcp.writable().await?,
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

#[cfg(not(target_os = "linux"))]
async fn sendfile(tcp: &mut TcpStream, sf: &SendFile) -> io::Result<()> {
    use tokio::io::AsyncSeekExt;
    let mut file = tokio::fs::File::open(&sf.path).await?;
    file.seek(std::io::SeekFrom::Start(sf.offset)).await?;
    let mut limited = file.take(sf.len);
    tokio::io::copy(&mut limited, tcp).await?;
    Ok(())
}

/// A TCP stream with some already-read bytes put back in front, for handing a
/// connection to hyper mid-stream.
pub struct Rewind {
    prefix: Bytes,
    inner: TcpStream,
}

impl Rewind {
    pub fn new(inner: TcpStream, prefix: Bytes) -> Self {
        Self { prefix, inner }
    }
}

impl AsyncRead for Rewind {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        if !self.prefix.is_empty() {
            let n = self.prefix.len().min(buf.remaining());
            buf.put_slice(&self.prefix[..n]);
            self.prefix.advance(n);
            return Poll::Ready(Ok(()));
        }
        Pin::new(&mut self.inner).poll_read(cx, buf)
    }
}

impl AsyncWrite for Rewind {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.inner).poll_write(cx, buf)
    }

    fn poll_write_vectored(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bufs: &[io::IoSlice<'_>],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.inner).poll_write_vectored(cx, bufs)
    }

    fn is_write_vectored(&self) -> bool {
        self.inner.is_write_vectored()
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_shutdown(cx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_str(s: &str) -> Result<Option<Head>, HeadError> {
        let mut buf = BytesMut::from(s.as_bytes());
        parse(&mut buf)
    }

    #[test]
    fn simple_get_stays_on_the_fast_path() {
        let Ok(Some(h)) = parse_str("GET /a?b=1 HTTP/1.1\r\nHost: x\r\n\r\n") else {
            panic!("should parse");
        };
        assert_eq!(h.method, Method::GET);
        assert_eq!(h.uri, "/a?b=1");
        assert_eq!(h.headers["host"], "x");
        assert!(h.keep_alive && !h.needs_hyper);
    }

    #[test]
    fn bodies_and_upgrades_go_to_hyper() {
        for req in [
            "POST / HTTP/1.1\r\nContent-Length: 3\r\n\r\n",
            "POST / HTTP/1.1\r\nTransfer-Encoding: chunked\r\n\r\n",
            "GET / HTTP/1.1\r\nConnection: Upgrade\r\nUpgrade: websocket\r\n\r\n",
            "PUT / HTTP/1.1\r\nExpect: 100-continue\r\nContent-Length: 9\r\n\r\n",
        ] {
            let Ok(Some(h)) = parse_str(req) else {
                panic!("{req}")
            };
            assert!(h.needs_hyper, "{req}");
        }
    }

    #[test]
    fn http10_closes_unless_asked_to_keep_alive() {
        let Ok(Some(h)) = parse_str("GET / HTTP/1.0\r\n\r\n") else {
            panic!()
        };
        assert!(!h.keep_alive);
        let Ok(Some(h)) = parse_str("GET / HTTP/1.0\r\nConnection: keep-alive\r\n\r\n") else {
            panic!()
        };
        assert!(h.keep_alive);
    }

    #[test]
    fn partial_and_malformed_heads() {
        assert!(matches!(parse_str("GET / HTTP/1.1\r\nHost:"), Ok(None)));
        assert!(matches!(
            parse_str("GARBAGE\r\n\r\n"),
            Err(HeadError::Bad(StatusCode::BAD_REQUEST))
        ));
    }

    #[test]
    fn head_encoding() {
        let mut out = Vec::new();
        let mut h = HeaderMap::new();
        h.insert(header::CONTENT_TYPE, HeaderValue::from_static("text/plain"));
        h.insert(header::CONTENT_LENGTH, HeaderValue::from_static("999"));
        encode_head(&mut out, StatusCode::OK, &h, Framing::Length(5), true);
        let s = String::from_utf8(out).unwrap();
        assert!(s.starts_with("HTTP/1.1 200 OK\r\n"));
        assert!(s.contains("content-length: 5\r\n") && !s.contains("999"));
        assert!(s.contains("date: ") && s.ends_with("\r\n\r\n"));
    }
}
