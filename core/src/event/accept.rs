use axum::body::Body;
use hyper::body::Incoming;
use hyper::Request;
use hyper_util::rt::TokioIo;
#[cfg(feature = "http2")]
use hyper_util::{rt::TokioExecutor, server::conn::auto};

#[cfg(feature = "http2")]
macro_rules! serve_io {
    ($builder:expr, $io:expr, $service:expr) => {
        $builder
            .serve_connection_with_upgrades(TokioIo::new($io), $service)
            .await
            .map_err(|e| e.to_string())
    };
}
#[cfg(not(feature = "http2"))]
macro_rules! serve_io {
    ($builder:expr, $io:expr, $service:expr) => {
        $builder
            .serve_connection(TokioIo::new($io), $service)
            .with_upgrades()
            .await
            .map_err(|e| e.to_string())
    };
}
use std::convert::Infallible;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::net::TcpListener;
use tokio_rustls::TlsAcceptor;
use tracing::{debug, warn};

use crate::http::ProxyEngine;

struct OpenConn(Arc<AtomicUsize>);

impl Drop for OpenConn {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::Relaxed);
    }
}

enum Conn {
    Plain(tokio::net::TcpStream, axum::body::Bytes),
    Tls(TlsAcceptor, tokio::net::TcpStream),
}

pub async fn serve(
    listener: TcpListener,
    engine: Arc<ProxyEngine>,
    port: u16,
    tls: Option<TlsAcceptor>,
) {
    let http = engine.store.config().http.clone();
    use crate::config::HttpProtocol;
    let http1 = http.serves(HttpProtocol::Http1);
    #[cfg(feature = "http2")]
    let builder = {
        let mut b = auto::Builder::new(TokioExecutor::new());
        b.http1()
            .timer(crate::event::clock::CoarseTimer)
            .auto_date_header(false)
            .keep_alive(http.keepalive_timeout_ms > 0)
            .header_read_timeout(Duration::from_millis(http.client_header_timeout_ms.max(1)));
        b.http2()
            .timer(crate::event::clock::CoarseTimer)
            .auto_date_header(false)
            .keep_alive_interval(None);
        if !http.serves(HttpProtocol::Http2) {
            b = b.http1_only();
        } else if !http1 {
            b = b.http2_only();
        }
        b
    };
    #[cfg(not(feature = "http2"))]
    let builder = {
        let mut b = hyper::server::conn::http1::Builder::new();
        b.timer(crate::event::clock::CoarseTimer)
            .auto_date_header(false)
            .keep_alive(http.keepalive_timeout_ms > 0)
            .header_read_timeout(Duration::from_millis(http.client_header_timeout_ms.max(1)));
        b
    };
    let builder = Arc::new(builder);
    let is_tls = tls.is_some();
    let alt_svc = (is_tls && http.serves(HttpProtocol::Http3) && cfg!(feature = "http3"))
        .then(|| hyper::header::HeaderValue::from_str(&format!("h3=\":{}\"; ma=86400", port)).ok())
        .flatten();
    let fast = Arc::new(crate::http::server::Settings {
        header_timeout: Duration::from_millis(http.client_header_timeout_ms.max(1)),
        keepalive_timeout: Duration::from_millis(http.keepalive_timeout_ms.max(1)),
    });

    let max_conns = http.worker_connections.max(1) as usize;
    let open = Arc::new(AtomicUsize::new(0));
    let mut warned = false;

    loop {
        if open.load(Ordering::Relaxed) >= max_conns {
            if !warned {
                warn!(
                    port,
                    max_conns, "worker_connections reached; pausing accept"
                );
                warned = true;
            }
            crate::event::clock::CoarseSleep::after(Duration::from_millis(5)).await;
            continue;
        }
        warned = false;
        let (tcp, addr) = match listener.accept().await {
            Ok(c) => c,
            Err(e) => {
                warn!(port, error = %e, "accept failed");
                tokio::time::sleep(Duration::from_millis(10)).await;
                continue;
            }
        };
        let _ = tcp.set_nodelay(true);
        open.fetch_add(1, Ordering::Relaxed);
        let slot = OpenConn(open.clone());

        let engine = engine.clone();
        let builder = builder.clone();
        let tls = tls.clone();
        let fast = fast.clone();
        let alt_svc = alt_svc.clone();
        tokio::spawn(async move {
            let _slot = slot;
            let conn = match tls {
                None if !http1 => Conn::Plain(tcp, axum::body::Bytes::new()),
                None => {
                    match Box::pin(crate::http::server::serve(tcp, addr, &engine, port, &fast))
                        .await
                    {
                        crate::http::server::Outcome::Done => return,
                        crate::http::server::Outcome::Handoff(tcp, prefix) => {
                            Conn::Plain(tcp, prefix)
                        }
                    }
                }
                Some(acceptor) => Conn::Tls(acceptor, tcp),
            };
            let service = hyper::service::service_fn(move |req: Request<Incoming>| {
                let engine = engine.clone();
                let alt_svc = alt_svc.clone();
                async move {
                    let mut resp = engine
                        .handle_request(port, is_tls, addr, req.map(Body::new))
                        .await;
                    resp.headers_mut()
                        .entry(hyper::header::DATE)
                        .or_insert_with(crate::event::clock::http_date);
                    if let Some(v) = alt_svc {
                        resp.headers_mut().insert(hyper::header::ALT_SVC, v);
                    }
                    Ok::<_, Infallible>(resp)
                }
            });

            let result = Box::pin(async move {
                match conn {
                    Conn::Plain(tcp, prefix) => {
                        let io = crate::http::server::Rewind::new(tcp, prefix);
                        Some(serve_io!(builder, io, service))
                    }
                    Conn::Tls(acceptor, tcp) => match acceptor.accept(tcp).await {
                        Ok(stream) => Some(serve_io!(builder, stream, service)),
                        Err(e) => {
                            debug!(%addr, error = %e, "TLS handshake failed");
                            None
                        }
                    },
                }
            })
            .await;
            if let Some(Err(e)) = result {
                debug!(%addr, error = %e, "connection closed with error");
            }
        });
    }
}
