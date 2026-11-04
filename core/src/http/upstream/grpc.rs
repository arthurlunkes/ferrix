//! nginx `grpc_pass`: proxies gRPC over an HTTP/2 connection to the backend
//! (`grpc://` = cleartext h2c, `grpcs://` = TLS with ALPN h2). One multiplexed
//! connection per target is kept and reused; trailers pass through untouched.

use axum::body::Body;
use axum::http::{header, uri::PathAndQuery, Request, Response, Uri};
use hyper::client::conn::http2::SendRequest;
use hyper_util::rt::{TokioExecutor, TokioIo};
use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};
use std::time::Duration;
use tokio::net::TcpStream;

use super::h1::Target;

static CONNS: LazyLock<Mutex<HashMap<String, SendRequest<Body>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

static TLS: LazyLock<tokio_rustls::TlsConnector> =
    LazyLock::new(|| super::h1::tls_connector_alpn(true, b"h2"));
static TLS_INSECURE: LazyLock<tokio_rustls::TlsConnector> =
    LazyLock::new(|| super::h1::tls_connector_alpn(false, b"h2"));

/// `grpc://h:p` → `http://h:p`, `grpcs://` → `https://`.
pub fn normalize(url: &str) -> String {
    if let Some(r) = url.strip_prefix("grpc://") {
        format!("http://{r}")
    } else if let Some(r) = url.strip_prefix("grpcs://") {
        format!("https://{r}")
    } else {
        url.to_string()
    }
}

async fn connect(
    base: &str,
    timeout: Duration,
    verify_tls: bool,
) -> Result<SendRequest<Body>, String> {
    let target = Target::parse(base).ok_or_else(|| format!("invalid gRPC target {base}"))?;
    let tcp = tokio::time::timeout(timeout, TcpStream::connect((target.host, target.port)))
        .await
        .map_err(|_| "connect timed out".to_string())?
        .map_err(|e| e.to_string())?;
    let _ = tcp.set_nodelay(true);
    let mut builder = hyper::client::conn::http2::Builder::new(TokioExecutor::new());
    builder
        .keep_alive_interval(Duration::from_secs(30))
        .keep_alive_while_idle(true)
        .timer(hyper_util::rt::TokioTimer::new());
    if target.tls {
        let name = rustls::pki_types::ServerName::try_from(target.host.to_string())
            .map_err(|e| e.to_string())?;
        let tls = if verify_tls { &*TLS } else { &*TLS_INSECURE };
        let io = tls.connect(name, tcp).await.map_err(|e| e.to_string())?;
        let (send, conn) = builder
            .handshake(TokioIo::new(io))
            .await
            .map_err(|e| e.to_string())?;
        tokio::spawn(conn);
        Ok(send)
    } else {
        let (send, conn) = builder
            .handshake(TokioIo::new(tcp))
            .await
            .map_err(|e| e.to_string())?;
        tokio::spawn(conn);
        Ok(send)
    }
}

async fn sender(base: &str, timeout: Duration, verify: bool) -> Result<SendRequest<Body>, String> {
    if let Some(s) = CONNS.lock().ok().and_then(|m| m.get(base).cloned()) {
        if !s.is_closed() {
            return Ok(s);
        }
    }
    let s = connect(base, timeout, verify).await?;
    if let Ok(mut m) = CONNS.lock() {
        m.insert(base.to_string(), s.clone());
    }
    Ok(s)
}

/// Forwards `req` to `base` (already normalized to http/https).
pub async fn forward(
    base: &str,
    mut req: Request<Body>,
    connect_timeout: Duration,
    verify_tls: bool,
) -> Result<Response<Body>, String> {
    let base = base.trim_end_matches('/');
    let target = Target::parse(base).ok_or_else(|| format!("invalid gRPC target {base}"))?;
    let pq = req
        .uri()
        .path_and_query()
        .cloned()
        .unwrap_or_else(|| PathAndQuery::from_static("/"));
    let scheme = if target.tls { "https" } else { "http" };
    *req.uri_mut() = Uri::builder()
        .scheme(scheme)
        .authority(target.authority)
        .path_and_query(pq)
        .build()
        .map_err(|e| e.to_string())?;
    *req.version_mut() = axum::http::Version::HTTP_2;
    for h in [
        header::HOST,
        header::CONNECTION,
        header::TRANSFER_ENCODING,
        header::UPGRADE,
    ] {
        req.headers_mut().remove(h);
    }
    req.headers_mut().remove("keep-alive");
    req.headers_mut().remove("proxy-connection");

    let mut send = sender(base, connect_timeout, verify_tls).await?;
    send.ready().await.map_err(|e| e.to_string())?;
    let resp = send.send_request(req).await.map_err(|e| {
        if let Ok(mut m) = CONNS.lock() {
            m.remove(base);
        }
        e.to_string()
    })?;
    let (parts, body) = resp.into_parts();
    Ok(Response::from_parts(parts, Body::new(body)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_schemes() {
        assert_eq!(normalize("grpc://a:50051"), "http://a:50051");
        assert_eq!(normalize("grpcs://a"), "https://a");
        assert_eq!(normalize("http://a"), "http://a");
    }

    #[tokio::test]
    async fn proxies_over_h2c_with_trailers() {
        use http_body_util::BodyExt;
        let _ = rustls::crypto::ring::default_provider().install_default();
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = l.local_addr().unwrap();
        tokio::spawn(async move {
            let (s, _) = l.accept().await.unwrap();
            let svc =
                hyper::service::service_fn(|req: Request<hyper::body::Incoming>| async move {
                    assert_eq!(req.version(), axum::http::Version::HTTP_2);
                    assert_eq!(req.uri().path(), "/pkg.Svc/Call");
                    let mut trailers = axum::http::HeaderMap::new();
                    trailers.insert("grpc-status", "0".parse().unwrap());
                    let body = http_body_util::StreamBody::new(futures_util::stream::iter(vec![
                        Ok::<_, std::convert::Infallible>(hyper::body::Frame::data(
                            bytes::Bytes::from_static(b"\0\0\0\0\0"),
                        )),
                        Ok(hyper::body::Frame::trailers(trailers)),
                    ]));
                    Ok::<_, std::convert::Infallible>(
                        Response::builder()
                            .header("content-type", "application/grpc")
                            .body(body)
                            .unwrap(),
                    )
                });
            hyper::server::conn::http2::Builder::new(TokioExecutor::new())
                .serve_connection(TokioIo::new(s), svc)
                .await
                .unwrap();
        });
        let req = Request::builder()
            .method("POST")
            .uri("/pkg.Svc/Call")
            .header("content-type", "application/grpc")
            .header("te", "trailers")
            .body(Body::from(&b"\0\0\0\0\0"[..]))
            .unwrap();
        let resp = forward(&format!("http://{addr}"), req, Duration::from_secs(2), true)
            .await
            .unwrap();
        let collected = resp.into_body().collect().await.unwrap();
        assert_eq!(collected.trailers().unwrap()["grpc-status"], "0");
    }
}
