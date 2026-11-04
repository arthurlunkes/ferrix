//! nginx `tunnel_pass` (ngx_http_tunnel_module): HTTP CONNECT forward proxy.
//! The client asks `CONNECT host:port`; Ferrix opens a TCP connection there
//! (or to a fixed `pass` target), answers 200 and relays bytes both ways.

use axum::body::Body;
use axum::http::{Request, Response, StatusCode};
use axum::response::IntoResponse;
use hyper_util::rt::TokioIo;
use std::time::Duration;
use tokio::net::TcpStream;

use crate::config::TunnelConfig;

/// `host:port` the client wants, if it is allowed.
pub fn target(cfg: &TunnelConfig, req: &Request<Body>) -> Result<String, StatusCode> {
    if let Some(p) = &cfg.pass {
        return Ok(p.clone());
    }
    let auth = req
        .uri()
        .authority()
        .map(|a| a.as_str().to_string())
        .ok_or(StatusCode::BAD_REQUEST)?;
    let port: u16 = auth
        .rsplit_once(':')
        .and_then(|(_, p)| p.parse().ok())
        .ok_or(StatusCode::BAD_REQUEST)?;
    let ports = if cfg.allowed_ports.is_empty() {
        &[443][..]
    } else {
        &cfg.allowed_ports[..]
    };
    if !ports.contains(&port) {
        return Err(StatusCode::FORBIDDEN);
    }
    let host = auth.rsplit_once(':').map(|(h, _)| h).unwrap_or("");
    if !cfg.allowed_hosts.is_empty()
        && !cfg.allowed_hosts.iter().any(|h| {
            h == host
                || h.strip_prefix("*.")
                    .is_some_and(|s| host.ends_with(&format!(".{s}")))
        })
    {
        return Err(StatusCode::FORBIDDEN);
    }
    Ok(auth)
}

pub async fn handle(
    cfg: &TunnelConfig,
    mut req: Request<Body>,
    connect_timeout: Duration,
) -> Response<Body> {
    let dest = match target(cfg, &req) {
        Ok(d) => d,
        Err(code) => return code.into_response(),
    };
    let upstream = match tokio::time::timeout(connect_timeout, TcpStream::connect(&dest)).await {
        Ok(Ok(s)) => s,
        Ok(Err(e)) => {
            tracing::debug!(%dest, error = %e, "tunnel connect failed");
            return StatusCode::BAD_GATEWAY.into_response();
        }
        Err(_) => return StatusCode::GATEWAY_TIMEOUT.into_response(),
    };
    let _ = upstream.set_nodelay(true);
    let idle = Duration::from_millis(cfg.idle_timeout_ms.unwrap_or(600_000));
    let on_upgrade = hyper::upgrade::on(&mut req);
    tokio::spawn(async move {
        let Ok(upgraded) = on_upgrade.await else {
            return;
        };
        let mut client = TokioIo::new(upgraded);
        let mut upstream = upstream;
        let _ = tokio::time::timeout(
            idle,
            tokio::io::copy_bidirectional(&mut client, &mut upstream),
        )
        .await;
    });
    Response::new(Body::empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn connect(auth: &str) -> Request<Body> {
        Request::builder()
            .method("CONNECT")
            .uri(auth)
            .body(Body::empty())
            .unwrap()
    }

    #[test]
    fn port_and_host_rules() {
        let cfg = TunnelConfig::default();
        assert_eq!(
            target(&cfg, &connect("example.com:443")).unwrap(),
            "example.com:443"
        );
        assert_eq!(
            target(&cfg, &connect("example.com:22")).unwrap_err(),
            StatusCode::FORBIDDEN
        );
        let cfg = TunnelConfig {
            allowed_ports: vec![443, 8443],
            allowed_hosts: vec!["*.example.com".into()],
            ..Default::default()
        };
        assert!(target(&cfg, &connect("api.example.com:8443")).is_ok());
        assert_eq!(
            target(&cfg, &connect("evil.com:443")).unwrap_err(),
            StatusCode::FORBIDDEN
        );
        let fixed = TunnelConfig {
            pass: Some("10.0.0.1:5432".into()),
            ..Default::default()
        };
        assert_eq!(target(&fixed, &connect("x:1")).unwrap(), "10.0.0.1:5432");
    }
}
