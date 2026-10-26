//! Accept loop for the proxy ports.
//!
//! The `ngx_event_accept` of Ferrix. Plain-HTTP connections start on its own HTTP/1.1
//! fast path (`crate::http::server`),
//! which hands anything unusual to hyper. TLS connections go to hyper directly, using
//! Ferrix's SNI certificate resolver.

use axum::body::Body;
use hyper::body::Incoming;
use hyper::Request;
use hyper_util::rt::{TokioExecutor, TokioIo};
use hyper_util::server::conn::auto;
use std::convert::Infallible;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::net::TcpListener;
use tokio_rustls::TlsAcceptor;
use tracing::{debug, warn};

use crate::http::ProxyEngine;

/// Counts a worker's open client connection until it's dropped.
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
    let mut builder = auto::Builder::new(TokioExecutor::new());
    // Coarse timers and a once-per-second Date header keep clock syscalls out of the
    // request path (see crate::event::clock).
    builder
        .http1()
        .timer(crate::event::clock::CoarseTimer)
        .auto_date_header(false)
        .keep_alive(http.keepalive_timeout_ms > 0)
        .header_read_timeout(Duration::from_millis(http.client_header_timeout_ms.max(1)));
    builder
        .http2()
        .timer(crate::event::clock::CoarseTimer)
        .auto_date_header(false)
        .keep_alive_interval(None);
    let builder = Arc::new(builder);
    let is_tls = tls.is_some();
    let fast = Arc::new(crate::http::server::Settings {
        header_timeout: Duration::from_millis(http.client_header_timeout_ms.max(1)),
        keepalive_timeout: Duration::from_millis(http.keepalive_timeout_ms.max(1)),
    });

    // nginx's `worker_connections`: a worker at its limit stops accepting (the kernel
    // keeps new connections in the listen backlog, or hands them to another worker)
    // until one of its connections closes.
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
                // Usually EMFILE/ENFILE: back off briefly instead of spinning.
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
        tokio::spawn(async move {
            let _slot = slot;
            // Plain HTTP/1.1 goes through Ferrix's own fast path first; it hands the
            // connection to hyper for anything it doesn't handle itself.
            let conn = match tls {
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
                async move {
                    let mut resp = engine
                        .handle_request(port, is_tls, addr, req.map(Body::new))
                        .await;
                    resp.headers_mut()
                        .entry(hyper::header::DATE)
                        .or_insert_with(crate::event::clock::http_date);
                    Ok::<_, Infallible>(resp)
                }
            });

            // Boxed: hyper's connection state is only needed for handed-off connections.
            // Boxed: hyper's connection state is only needed for handed-off connections.
            let result = Box::pin(async move {
                match conn {
                    Conn::Plain(tcp, prefix) => {
                        let io = crate::http::server::Rewind::new(tcp, prefix);
                        Some(
                            builder
                                .serve_connection_with_upgrades(TokioIo::new(io), service)
                                .await,
                        )
                    }
                    Conn::Tls(acceptor, tcp) => match acceptor.accept(tcp).await {
                        Ok(stream) => Some(
                            builder
                                .serve_connection_with_upgrades(TokioIo::new(stream), service)
                                .await,
                        ),
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
