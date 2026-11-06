use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream, UdpSocket};
use tokio::sync::Mutex;
use tracing::{debug, error, info, warn};

use crate::config::{StreamConfig, StreamProtocol, UpstreamTarget};
use crate::http::ProxyEngine;

pub fn target_addr(url: &str) -> &str {
    let rest = url.split_once("://").map(|(_, r)| r).unwrap_or(url);
    rest.split('/').next().unwrap_or(rest)
}

fn targets_for(engine: &ProxyEngine, upstream: &str, client: SocketAddr) -> Vec<UpstreamTarget> {
    let config = engine.store.config();
    match config.upstreams.get(upstream) {
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
    let acceptor = match &stream.tls {
        Some(t) => Some(
            tls_acceptor(&t.cert_path, &t.key_path)
                .map_err(|e| std::io::Error::other(format!("stream '{}': {}", stream.id, e)))?,
        ),
        None => None,
    };
    info!(
        "TCP stream '{}' listening on {} -> {}",
        stream.id, stream.listen, stream.upstream
    );
    let (allow, _) = crate::net::cidr::IpMatcher::compile(&stream.allowed_ips);
    let (deny, _) = crate::net::cidr::IpMatcher::compile(&stream.denied_ips);
    let acl = Arc::new((allow, deny));
    let per_ip: Arc<std::sync::Mutex<HashMap<std::net::IpAddr, u32>>> = Arc::default();
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
        let acceptor = acceptor.clone();
        let acl = acl.clone();
        let per_ip = per_ip.clone();
        tokio::spawn(async move {
            let r = handle_tcp(&engine, &stream, client, addr, acceptor, &acl, &per_ip).await;
            if let Err(e) = r {
                debug!(stream = %stream.id, client = %addr, "tcp session ended: {}", e);
            }
        });
    }
}

fn tls_acceptor(cert: &str, key: &str) -> Result<tokio_rustls::TlsAcceptor, String> {
    let ck = crate::tls::sni::load_certified_key(cert, key)?;
    let config = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_cert_resolver(Arc::new(rustls::sign::SingleCertAndKey::from(ck)));
    Ok(tokio_rustls::TlsAcceptor::from(Arc::new(config)))
}

trait Io: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> Io for T {}

struct ConnSlot<'a> {
    map: &'a std::sync::Mutex<HashMap<std::net::IpAddr, u32>>,
    ip: std::net::IpAddr,
}

impl Drop for ConnSlot<'_> {
    fn drop(&mut self) {
        if let Ok(mut m) = self.map.lock() {
            if let Some(n) = m.get_mut(&self.ip) {
                *n -= 1;
                if *n == 0 {
                    m.remove(&self.ip);
                }
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn handle_tcp(
    engine: &ProxyEngine,
    stream: &StreamConfig,
    client: TcpStream,
    addr: SocketAddr,
    acceptor: Option<tokio_rustls::TlsAcceptor>,
    acl: &(crate::net::cidr::IpMatcher, crate::net::cidr::IpMatcher),
    per_ip: &std::sync::Mutex<HashMap<std::net::IpAddr, u32>>,
) -> Result<(), String> {
    let connect_timeout = Duration::from_millis(stream.connect_timeout_ms);
    let mut client = client;
    let mut addr = addr;
    if stream.accept_proxy_protocol {
        if let Some(real) = crate::net::proxy_protocol::read(&mut client, connect_timeout)
            .await
            .map_err(|e| e.to_string())?
        {
            addr = real;
        }
    }

    if !crate::http::access::AccessControl::is_ip_allowed(addr.ip(), &acl.0, &acl.1) {
        return Err("denied by allow/deny".into());
    }

    let _slot = match stream.limit_conn {
        Some(max) => {
            let mut m = per_ip.lock().map_err(|_| "limit_conn lock")?;
            let n = m.entry(addr.ip()).or_insert(0);
            if *n >= max {
                return Err("limit_conn reached".into());
            }
            *n += 1;
            Some(ConnSlot {
                map: per_ip,
                ip: addr.ip(),
            })
        }
        None => None,
    };

    let upstream_name = if stream.sni_routes.is_empty() {
        stream.upstream.clone()
    } else {
        let name = preread_sni(&client, connect_timeout).await;
        crate::net::sni::lookup(&stream.sni_routes, name.as_deref())
            .cloned()
            .unwrap_or_else(|| stream.upstream.clone())
    };
    let local = client.local_addr().unwrap_or(addr);

    let mut client_io: Box<dyn Io> = match acceptor {
        Some(a) => Box::new(
            tokio::time::timeout(connect_timeout, a.accept(client))
                .await
                .map_err(|_| "TLS handshake timed out")?
                .map_err(|e| format!("TLS handshake: {e}"))?,
        ),
        None => Box::new(client),
    };

    if let Some(text) = &stream.return_text {
        client_io
            .write_all(text.as_bytes())
            .await
            .map_err(|e| e.to_string())?;
        let _ = client_io.shutdown().await;
        return Ok(());
    }

    let idle = Duration::from_millis(stream.idle_timeout_ms);
    let rates = (stream.upload_rate, stream.download_rate);

    if let Some(dest) = &stream.pass {
        let upstream = connect_upstream(stream, dest, connect_timeout, addr, local).await?;
        return pipe(client_io, upstream, idle, rates)
            .await
            .map_err(|e| e.to_string());
    }

    let mut last_err = String::from("no upstream targets available");
    for target in targets_for(engine, &upstream_name, addr) {
        let dest = target_addr(&target.url);
        match connect_upstream(stream, dest, connect_timeout, addr, local).await {
            Ok(upstream) => {
                engine
                    .health_checker
                    .mark_success(&upstream_name, &target.url, 0.0);
                engine.pool_manager.inc_conn(&target.url);
                let r = pipe(client_io, upstream, idle, rates).await;
                engine.pool_manager.dec_conn(&target.url);
                return r.map_err(|e| e.to_string());
            }
            Err(e) => last_err = e,
        }
        engine
            .health_checker
            .mark_failure(&upstream_name, &target.url, target.max_fails);
    }
    Err(last_err)
}

async fn connect_upstream(
    stream: &StreamConfig,
    dest: &str,
    timeout: Duration,
    client: SocketAddr,
    local: SocketAddr,
) -> Result<Box<dyn Io>, String> {
    let mut tcp = match tokio::time::timeout(timeout, TcpStream::connect(dest)).await {
        Ok(Ok(s)) => s,
        Ok(Err(e)) => return Err(format!("{dest}: {e}")),
        Err(_) => return Err(format!("{dest}: connect timed out")),
    };
    let _ = tcp.set_nodelay(true);
    if stream.send_proxy_protocol {
        crate::net::proxy_protocol::write(&mut tcp, client, local)
            .await
            .map_err(|e| format!("{dest}: {e}"))?;
    }
    let Some(t) = &stream.upstream_tls else {
        return Ok(Box::new(tcp));
    };
    let host = dest.rsplit_once(':').map(|(h, _)| h).unwrap_or(dest);
    let host = host.trim_start_matches('[').trim_end_matches(']');
    let name = t
        .settings
        .server_name
        .as_deref()
        .unwrap_or(host)
        .to_string();
    let connector = match crate::http::upstream::tls_client::get(Some(&t.settings), t.verify) {
        Some(c) => c.connector.clone(),
        None => crate::http::upstream::h1::tls_connector(t.verify),
    };
    let server_name = rustls::pki_types::ServerName::try_from(name).map_err(|e| e.to_string())?;
    let tls = tokio::time::timeout(timeout, connector.connect(server_name, tcp))
        .await
        .map_err(|_| format!("{dest}: TLS handshake timed out"))?
        .map_err(|e| format!("{dest}: TLS: {e}"))?;
    Ok(Box::new(tls))
}

async fn preread_sni(client: &TcpStream, timeout: Duration) -> Option<String> {
    let mut buf = vec![0u8; 16 * 1024];
    let read = async {
        loop {
            let n = client.peek(&mut buf).await.ok()?;
            if n == 0 {
                return None;
            }
            match crate::net::sni::parse(&buf[..n]) {
                Ok(name) => return name,
                Err(()) if n == buf.len() => return None,
                Err(()) => tokio::time::sleep(Duration::from_millis(5)).await,
            }
        }
    };
    tokio::time::timeout(timeout, read).await.ok().flatten()
}

async fn pipe(
    client: Box<dyn Io>,
    upstream: Box<dyn Io>,
    idle: Duration,
    (upload_rate, download_rate): (u64, u64),
) -> std::io::Result<()> {
    let (cr, cw) = tokio::io::split(client);
    let (ur, uw) = tokio::io::split(upstream);
    let activity = Arc::new(std::sync::atomic::AtomicU64::new(
        crate::event::clock::mono_ms(),
    ));
    let up = copy_half(cr, uw, activity.clone(), upload_rate);
    let down = copy_half(ur, cw, activity.clone(), download_rate);
    let watchdog = async {
        loop {
            tokio::time::sleep(idle / 4 + Duration::from_millis(1)).await;
            let quiet = crate::event::clock::mono_ms()
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
    rate: u64,
) -> std::io::Result<()> {
    let size = if rate > 0 {
        (rate / 8).clamp(1024, 32 * 1024) as usize
    } else {
        32 * 1024
    };
    let mut buf = vec![0u8; size];
    let start = tokio::time::Instant::now();
    let mut sent: u64 = 0;
    loop {
        let n = from.read(&mut buf).await?;
        if n == 0 {
            to.shutdown().await.ok();
            return Ok(());
        }
        to.write_all(&buf[..n]).await?;
        sent += n as u64;
        if rate > 0 {
            let due = start + Duration::from_secs_f64(sent as f64 / rate as f64);
            tokio::time::sleep_until(due).await;
        }
        activity.store(
            crate::event::clock::mono_ms(),
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
                let now = crate::event::clock::mono_ms();
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
            let Some(target) = targets_for(&engine, &stream.upstream, client)
                .into_iter()
                .next()
            else {
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
            let last_seen = Arc::new(std::sync::atomic::AtomicU64::new(
                crate::event::clock::mono_ms(),
            ));
            let (reply_up, reply_sock, reply_seen) =
                (upstream.clone(), socket.clone(), last_seen.clone());
            tokio::spawn(async move {
                let mut rbuf = vec![0u8; 64 * 1024];
                loop {
                    match tokio::time::timeout(idle, reply_up.recv(&mut rbuf)).await {
                        Ok(Ok(m)) => {
                            reply_seen.store(
                                crate::event::clock::mono_ms(),
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
                crate::event::clock::mono_ms(),
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
            pipe(Box::new(c), Box::new(u), Duration::from_secs(5), (0, 0))
                .await
                .unwrap();
        });
        let mut client = TcpStream::connect(front_addr).await.unwrap();
        client.write_all(b"hello").await.unwrap();
        let mut got = [0u8; 5];
        client.read_exact(&mut got).await.unwrap();
        assert_eq!(&got, b"hello");
    }
}

#[cfg(test)]
mod stream_feature_tests {
    use super::*;
    use crate::config::store::ConfigStore;
    use crate::config::{FerrixConfig, StreamTlsConfig, UpstreamConfig, UpstreamTarget};
    use crate::health::HealthChecker;
    use crate::metrics::MetricsCollector;

    const DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/tls");

    async fn free_port() -> u16 {
        TcpListener::bind("127.0.0.1:0")
            .await
            .unwrap()
            .local_addr()
            .unwrap()
            .port()
    }

    async fn start(mut st: StreamConfig, upstream: Option<String>) -> String {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let mut cfg = FerrixConfig::default();
        if let Some(u) = upstream {
            cfg.upstreams.insert(
                "echo".into(),
                UpstreamConfig {
                    name: "echo".into(),
                    algorithm: Default::default(),
                    targets: vec![UpstreamTarget {
                        url: format!("tcp://{u}"),
                        weight: 1,
                        max_fails: 3,
                        fail_timeout_secs: 10,
                        backup: false,
                        down: false,
                        max_conns: None,
                    }],
                    health_check: None,
                    verify_tls: true,
                    hash_key: None,
                    host_header: None,
                    sticky: None,
                    tls: None,
                },
            );
            st.upstream = "echo".into();
        }
        st.listen = format!("127.0.0.1:{}", free_port().await);
        st.connect_timeout_ms = 2000;
        st.idle_timeout_ms = 5000;
        let listen = st.listen.clone();
        let engine = Arc::new(ProxyEngine::new(
            Arc::new(ConfigStore::new(cfg, "t.json")),
            MetricsCollector::new(0),
            Arc::new(HealthChecker::new()),
        ));
        spawn(engine, st);
        tokio::time::sleep(Duration::from_millis(100)).await;
        listen
    }

    async fn echo() -> String {
        let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let a = l.local_addr().unwrap().to_string();
        tokio::spawn(async move {
            while let Ok((mut s, _)) = l.accept().await {
                tokio::spawn(async move {
                    let mut b = [0u8; 256];
                    while let Ok(n) = s.read(&mut b).await {
                        if n == 0 || s.write_all(&b[..n]).await.is_err() {
                            break;
                        }
                    }
                });
            }
        });
        a
    }

    #[tokio::test]
    async fn return_text_without_upstream() {
        let addr = start(
            StreamConfig {
                return_text: Some("hello\n".into()),
                ..Default::default()
            },
            None,
        )
        .await;
        let mut s = TcpStream::connect(addr).await.unwrap();
        let mut out = String::new();
        s.read_to_string(&mut out).await.unwrap();
        assert_eq!(out, "hello\n");
    }

    #[tokio::test]
    async fn denied_clients_are_closed() {
        let up = echo().await;
        let st = StreamConfig {
            denied_ips: vec!["127.0.0.1".into()],
            ..Default::default()
        };
        let addr = start(st, Some(up)).await;
        let mut s = TcpStream::connect(addr).await.unwrap();
        let _ = s.write_all(b"x").await;
        let mut b = [0u8; 8];
        let n = tokio::time::timeout(Duration::from_secs(2), s.read(&mut b))
            .await
            .unwrap()
            .unwrap_or(0);
        assert_eq!(n, 0);
    }

    #[tokio::test]
    async fn tls_termination_relays_to_a_plain_upstream() {
        let up = echo().await;
        let st = StreamConfig {
            tls: Some(StreamTlsConfig {
                cert_path: format!("{DIR}/server.pem"),
                key_path: format!("{DIR}/server.key"),
            }),
            ..Default::default()
        };
        let addr = start(st, Some(up)).await;
        let mut roots = rustls::RootCertStore::empty();
        let pem = std::fs::read(format!("{DIR}/ca.pem")).unwrap();
        for c in rustls_pemfile::certs(&mut pem.as_slice()) {
            roots.add(c.unwrap()).unwrap();
        }
        let cfg = rustls::ClientConfig::builder()
            .with_root_certificates(roots)
            .with_no_client_auth();
        let connector = tokio_rustls::TlsConnector::from(Arc::new(cfg));
        let tcp = TcpStream::connect(addr).await.unwrap();
        let name = rustls::pki_types::ServerName::try_from("backend.internal").unwrap();
        let mut tls = connector.connect(name, tcp).await.unwrap();
        tls.write_all(b"ping").await.unwrap();
        let mut b = [0u8; 4];
        tls.read_exact(&mut b).await.unwrap();
        assert_eq!(&b, b"ping");
    }
}
