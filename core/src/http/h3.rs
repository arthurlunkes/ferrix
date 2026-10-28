//! HTTP/3 (QUIC) for TLS ports, like nginx's `listen 443 quic`.
//!
//! One QUIC endpoint listens on UDP at the same address as the TLS listener and uses the
//! same SNI certificates. Each request goes through the same `ProxyEngine::handle_request`
//! as HTTP/1.1 and HTTP/2, so routing, filters and proxying behave identically; clients
//! find the endpoint through the `Alt-Svc` header the TCP listener adds.

use axum::body::{Body, Bytes};
use axum::http::{header, HeaderValue, Request, Response, StatusCode};
use bytes::Buf;
use http_body_util::BodyExt;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use tracing::{debug, error, info};

use crate::http::ProxyEngine;
use crate::tls::sni::SniResolver;

/// Headers that are specific to an HTTP/1 connection and are not allowed in HTTP/3.
const CONNECTION_HEADERS: [header::HeaderName; 5] = [
    header::CONNECTION,
    header::TRANSFER_ENCODING,
    header::UPGRADE,
    header::HeaderName::from_static("keep-alive"),
    header::HeaderName::from_static("proxy-connection"),
];

/// Starts the QUIC endpoint for `addr`. Returns an error if the UDP port can't be bound.
pub fn spawn(
    addr: SocketAddr,
    engine: Arc<ProxyEngine>,
    resolver: Arc<SniResolver>,
) -> Result<(), String> {
    let tls = crate::tls::sni::quic_server_config(resolver)?;
    let crypto = quinn::crypto::rustls::QuicServerConfig::try_from(tls)
        .map_err(|e| format!("HTTP/3 TLS configuration: {}", e))?;
    let mut server = quinn::ServerConfig::with_crypto(Arc::new(crypto));
    let http = engine.store.config().http.clone();
    let mut transport = quinn::TransportConfig::default();
    transport.max_idle_timeout(
        Duration::from_millis(http.keepalive_timeout_ms.max(1000))
            .try_into()
            .ok(),
    );
    server.transport_config(Arc::new(transport));
    let endpoint = quinn::Endpoint::server(server, addr)
        .map_err(|e| format!("HTTP/3: cannot bind UDP {}: {}", addr, e))?;
    info!("HTTP/3 (QUIC) listening on udp://{}", addr);

    tokio::spawn(async move {
        while let Some(incoming) = endpoint.accept().await {
            let engine = engine.clone();
            tokio::spawn(async move {
                let peer = incoming.remote_address();
                match incoming.await {
                    Ok(conn) => serve_connection(conn, engine, addr.port(), peer).await,
                    Err(e) => debug!(%peer, error = %e, "QUIC handshake failed"),
                }
            });
        }
        error!("HTTP/3 endpoint on {} stopped", addr);
    });
    Ok(())
}

async fn serve_connection(
    conn: quinn::Connection,
    engine: Arc<ProxyEngine>,
    port: u16,
    peer: SocketAddr,
) {
    let mut h3 = match h3::server::Connection::new(h3_quinn::Connection::new(conn)).await {
        Ok(c) => c,
        Err(e) => return debug!(%peer, error = %e, "HTTP/3 connection setup failed"),
    };
    loop {
        match h3.accept().await {
            Ok(Some(resolver)) => {
                let engine = engine.clone();
                tokio::spawn(async move {
                    let (req, stream) = match resolver.resolve_request().await {
                        Ok(r) => r,
                        Err(e) => return debug!(%peer, error = %e, "bad HTTP/3 request"),
                    };
                    if let Err(e) = serve_request(req, stream, &engine, port, peer).await {
                        debug!(%peer, error = %e, "HTTP/3 stream closed with error");
                    }
                });
            }
            Ok(None) => return,
            Err(e) => return debug!(%peer, error = %e, "HTTP/3 connection closed"),
        }
    }
}

type Stream = h3::server::RequestStream<h3_quinn::BidiStream<Bytes>, Bytes>;

async fn serve_request(
    req: Request<()>,
    mut stream: Stream,
    engine: &ProxyEngine,
    port: u16,
    peer: SocketAddr,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let limit = engine.store.config().http.client_max_body_size;
    let mut body = Vec::new();
    while let Some(mut chunk) = stream.recv_data().await? {
        if (body.len() + chunk.remaining()) as u64 > limit {
            let resp = Response::builder()
                .status(StatusCode::PAYLOAD_TOO_LARGE)
                .body(())?;
            stream.send_response(resp).await?;
            return Ok(stream.finish().await?);
        }
        while chunk.has_remaining() {
            let part = chunk.chunk();
            body.extend_from_slice(part);
            let n = part.len();
            chunk.advance(n);
        }
    }

    let (mut parts, ()) = req.into_parts();
    // HTTP/3 carries the host in the `:authority` pseudo-header; routing reads `Host`.
    if !parts.headers.contains_key(header::HOST) {
        if let Some(v) = parts
            .uri
            .authority()
            .and_then(|a| HeaderValue::from_str(a.as_str()).ok())
        {
            parts.headers.insert(header::HOST, v);
        }
    }
    if !body.is_empty() {
        parts
            .headers
            .insert(header::CONTENT_LENGTH, HeaderValue::from(body.len()));
    }
    let req = Request::from_parts(parts, Body::from(body));

    let resp = engine.handle_request(port, true, peer, req).await;
    let (mut parts, mut body) = resp.into_parts();
    for name in &CONNECTION_HEADERS {
        parts.headers.remove(name);
    }
    parts
        .headers
        .entry(header::DATE)
        .or_insert_with(crate::event::clock::http_date);
    stream
        .send_response(Response::from_parts(parts, ()))
        .await?;
    while let Some(frame) = body.frame().await {
        if let Ok(data) = frame?.into_data() {
            if !data.is_empty() {
                stream.send_data(data).await?;
            }
        }
    }
    Ok(stream.finish().await?)
}
