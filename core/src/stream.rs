//! Layer-4 proxying (nginx `stream {}`): raw TCP connections and UDP datagrams are
//! forwarded to an upstream pool, reusing its balancing and passive health checks.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream, UdpSocket};
use tokio::sync::Mutex;
use tracing::{debug, error, info, warn};

use crate::config::{StreamConfig, StreamProtocol, UpstreamTarget};
use crate::proxy::ProxyEngine;

/// `host:port` out of a target URL, ignoring any scheme and path.
pub fn target_addr(url: &str) -> &str {
    let rest = url.split_once("://").map(|(_, r)| r).unwrap_or(url);
    rest.split('/').next().unwrap_or(rest)
}

fn targets_for(
    engine: &ProxyEngine,
    stream: &StreamConfig,
    client: SocketAddr,
) -> Vec<UpstreamTarget> {
    let config = engine.store.config();
    match config.upstreams.get(&stream.upstream) {
        Some(up) => engine
            .pool_manager
            .select_targets(up, client.ip())
            .into_iter()
            .cloned()
            .collect(),
        None => Vec::new(),
    }
}

pub fn spawn(engine: Arc<ProxyEngine>, stream: StreamConfig) {
    tokio::spawn(async move {
        let result = match stream.protocol {
            StreamProtocol::Tcp => run_tcp(engine, &stream).await,
            StreamProtocol::Udp => run_udp(engine, &stream).await,
        };
        if let Err(e) = result {
            error!(stream = %stream.id, listen = %stream.listen, "stream proxy stopped: {}", e);
        }
    });
}

async fn run_tcp(engine: Arc<ProxyEngine>, stream: &StreamConfig) -> std::io::Result<()> {
    let listener = TcpListener::bind(&stream.listen).await?;
    info!(
        "TCP stream '{}' listening on {} -> {}",
        stream.id, stream.listen, stream.upstream
    );
    loop {
        let (client, addr) = match listener.accept().await {
            Ok(c) => c,
            Err(e) => {
                warn!(stream = %stream.id, error = %e, "accept failed");
                tokio::time::sleep(Duration::from_millis(10)).await;
                continue;
            }
        };
        let _ = client.set_nodelay(true);
        let engine = engine.clone();
        let stream = stream.clone();
        tokio::spawn(async move {
            if let Err(e) = handle_tcp(&engine, &stream, client, addr).await {
                debug!(stream = %stream.id, client = %addr, "tcp session ended: {}", e);
            }
        });
    }
}

async fn handle_tcp(
    engine: &ProxyEngine,
    stream: &StreamConfig,
    client: TcpStream,
    addr: SocketAddr,
) -> Result<(), String> {
    let connect_timeout = Duration::from_millis(stream.connect_timeout_ms);
    let mut last_err = String::from("no upstream targets available");
    for target in targets_for(engine, stream, addr) {
        let dest = target_addr(&target.url);
        match tokio::time::timeout(connect_timeout, TcpStream::connect(dest)).await {
            Ok(Ok(upstream)) => {
                let _ = upstream.set_nodelay(true);
                engine
                    .health_checker
                    .mark_success(&stream.upstream, &target.url, 0.0);
                engine.pool_manager.inc_conn(&target.url);
                let r = pipe(
                    client,
                    upstream,
                    Duration::from_millis(stream.idle_timeout_ms),
                )
                .await;
                engine.pool_manager.dec_conn(&target.url);
                return r.map_err(|e| e.to_string());
            }
            Ok(Err(e)) => last_err = format!("{}: {}", dest, e),
            Err(_) => last_err = format!("{}: connect timed out", dest),
        }
        engine
            .health_checker
            .mark_failure(&stream.upstream, &target.url, target.max_fails);
    }
    Err(last_err)
}

/// Copies both directions until either side closes or nothing moves for `idle`.
async fn pipe(client: TcpStream, upstream: TcpStream, idle: Duration) -> std::io::Result<()> {
    let (cr, cw) = client.into_split();
    let (ur, uw) = upstream.into_split();
    let activity = Arc::new(std::sync::atomic::AtomicU64::new(crate::clock::mono_ms()));
    let up = copy_half(cr, uw, activity.clone());
    let down = copy_half(ur, cw, activity.clone());
    let watchdog = async {
        loop {
            tokio::time::sleep(idle / 4 + Duration::from_millis(1)).await;
            let quiet = crate::clock::mono_ms()
                .saturating_sub(activity.load(std::sync::atomic::Ordering::Relaxed));
            if quiet >= idle.as_millis() as u64 {
                return;
            }
        }
    };
    tokio::select! {
        r = async { tokio::try_join!(up, down).map(|_| ()) } => r,
        _ = watchdog => Ok(()),
    }
}

async fn copy_half(
    mut from: impl AsyncRead + Unpin,
    mut to: impl AsyncWrite + Unpin,
    activity: Arc<std::sync::atomic::AtomicU64>,
) -> std::io::Result<()> {
    let mut buf = vec![0u8; 32 * 1024];
    loop {
        let n = from.read(&mut buf).await?;
        if n == 0 {
            to.shutdown().await.ok();
            return Ok(());
        }
        to.write_all(&buf[..n]).await?;
        activity.store(
            crate::clock::mono_ms(),
            std::sync::atomic::Ordering::Relaxed,
        );
    }
}

struct UdpSession {
    upstream: Arc<UdpSocket>,
    last_seen: Arc<std::sync::atomic::AtomicU64>,
}

async fn run_udp(engine: Arc<ProxyEngine>, stream: &StreamConfig) -> std::io::Result<()> {
    let socket = Arc::new(UdpSocket::bind(&stream.listen).await?);
    info!(
        "UDP stream '{}' listening on {} -> {}",
        stream.id, stream.listen, stream.upstream
    );
    let sessions: Arc<Mutex<HashMap<SocketAddr, UdpSession>>> = Arc::default();
    let idle = Duration::from_millis(stream.idle_timeout_ms.max(1000));

    {
        let sessions = sessions.clone();
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(idle / 2).await;
                let now = crate::clock::mono_ms();
                sessions.lock().await.retain(|_, s| {
                    now.saturating_sub(s.last_seen.load(std::sync::atomic::Ordering::Relaxed))
                        < idle.as_millis() as u64
                });
            }
        });
    }

    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let (n, client) = socket.recv_from(&mut buf).await?;
        let mut map = sessions.lock().await;
        if let std::collections::hash_map::Entry::Vacant(e) = map.entry(client) {
            let Some(target) = targets_for(&engine, stream, client).into_iter().next() else {
                continue;
            };
            let upstream = UdpSocket::bind(if client.is_ipv4() {
                "0.0.0.0:0"
            } else {
                "[::]:0"
            })
            .await?;
            if let Err(e) = upstream.connect(target_addr(&target.url)).await {
                warn!(stream = %stream.id, target = %target.url, "udp connect failed: {}", e);
                continue;
            }
            let upstream = Arc::new(upstream);
            let last_seen = Arc::new(std::sync::atomic::AtomicU64::new(crate::clock::mono_ms()));
            let (reply_up, reply_sock, reply_seen) =
                (upstream.clone(), socket.clone(), last_seen.clone());
            tokio::spawn(async move {
                let mut rbuf = vec![0u8; 64 * 1024];
                loop {
                    match tokio::time::timeout(idle, reply_up.recv(&mut rbuf)).await {
                        Ok(Ok(m)) => {
                            reply_seen.store(
                                crate::clock::mono_ms(),
                                std::sync::atomic::Ordering::Relaxed,
                            );
                            let _ = reply_sock.send_to(&rbuf[..m], client).await;
                        }
                        _ => return,
                    }
                }
            });
            e.insert(UdpSession {
                upstream,
                last_seen,
            });
        }
        if let Some(s) = map.get(&client) {
            s.last_seen.store(
                crate::clock::mono_ms(),
                std::sync::atomic::Ordering::Relaxed,
            );
            let _ = s.upstream.send(&buf[..n]).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn target_addresses() {
        assert_eq!(target_addr("tcp://db:5432"), "db:5432");
        assert_eq!(target_addr("10.0.0.1:53"), "10.0.0.1:53");
        assert_eq!(target_addr("http://a:80/x"), "a:80");
    }

    #[tokio::test]
    async fn tcp_pipe_relays_both_ways() {
        let echo = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let echo_addr = echo.local_addr().unwrap();
        tokio::spawn(async move {
            let (mut s, _) = echo.accept().await.unwrap();
            let mut b = [0u8; 5];
            s.read_exact(&mut b).await.unwrap();
            s.write_all(&b).await.unwrap();
        });
        let front = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let front_addr = front.local_addr().unwrap();
        tokio::spawn(async move {
            let (c, _) = front.accept().await.unwrap();
            let u = TcpStream::connect(echo_addr).await.unwrap();
            pipe(c, u, Duration::from_secs(5)).await.unwrap();
        });
        let mut client = TcpStream::connect(front_addr).await.unwrap();
        client.write_all(b"hello").await.unwrap();
        let mut got = [0u8; 5];
        client.read_exact(&mut got).await.unwrap();
        assert_eq!(&got, b"hello");
    }
}
