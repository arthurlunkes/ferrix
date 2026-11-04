use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use ferrix::config::store::ConfigStore;
use ferrix::config::{ConfigFormat, FerrixConfig};
use ferrix::health::HealthChecker;
use ferrix::http::ProxyEngine;
use ferrix::metrics::MetricsCollector;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

type Seen = Arc<Mutex<Vec<String>>>;

#[derive(Clone, Copy)]
enum Reply {
    Fixed(&'static str),
    Chunked,
}

async fn upstream(reply: Reply) -> (SocketAddr, Seen) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let seen: Seen = Arc::default();
    let log = seen.clone();
    tokio::spawn(async move {
        loop {
            let Ok((mut sock, _)) = listener.accept().await else {
                return;
            };
            let log = log.clone();
            tokio::spawn(async move {
                let mut buf = Vec::new();
                loop {
                    let mut chunk = [0u8; 4096];
                    let n = match sock.read(&mut chunk).await {
                        Ok(0) | Err(_) => return,
                        Ok(n) => n,
                    };
                    buf.extend_from_slice(&chunk[..n]);
                    while let Some(end) = find(&buf, b"\r\n\r\n") {
                        let head = String::from_utf8_lossy(&buf[..end]).to_string();
                        let len = header(&head, "content-length")
                            .and_then(|v| v.parse::<usize>().ok())
                            .unwrap_or(0);
                        while buf.len() < end + 4 + len {
                            let n = sock.read(&mut chunk).await.unwrap_or(0);
                            if n == 0 {
                                return;
                            }
                            buf.extend_from_slice(&chunk[..n]);
                        }
                        let body =
                            String::from_utf8_lossy(&buf[end + 4..end + 4 + len]).to_string();
                        log.lock().unwrap().push(format!("{head}\r\n\r\n{body}"));
                        buf.drain(..end + 4 + len);
                        let out = match reply {
                            Reply::Fixed(text) => format!(
                                "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nServer: upstream\r\nConnection: keep-alive\r\nContent-Length: {}\r\n\r\n{}",
                                text.len(),
                                text
                            ),
                            Reply::Chunked => "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n5\r\nhello\r\n6\r\n world\r\n0\r\n\r\n".to_string(),
                        };
                        if sock.write_all(out.as_bytes()).await.is_err() {
                            return;
                        }
                    }
                }
            });
        }
    });
    (addr, seen)
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

fn header<'a>(head: &'a str, name: &str) -> Option<&'a str> {
    head.lines().skip(1).find_map(|line| {
        let (k, v) = line.split_once(':')?;
        k.trim().eq_ignore_ascii_case(name).then(|| v.trim())
    })
}

async fn closed_port() -> u16 {
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    l.local_addr().unwrap().port()
}

async fn ferrix(yaml: String) -> SocketAddr {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let mut config = FerrixConfig::parse(&yaml, ConfigFormat::Yaml).unwrap();
    config.admin.enabled = false;
    let store = Arc::new(ConfigStore::new(config, "e2e.yaml"));
    let engine = Arc::new(ProxyEngine::new(
        store,
        MetricsCollector::new(0),
        Arc::new(HealthChecker::new()),
    ));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(ferrix::event::accept::serve(
        listener,
        engine,
        addr.port(),
        None,
    ));
    addr
}

fn site(port: u16, upstream_urls: &[String], extra_route: &str) -> String {
    let targets: String = upstream_urls
        .iter()
        .map(|u| format!("      - url: {u}\n"))
        .collect();
    format!(
        "http:\n  listen: 127.0.0.1:{port}\n  worker_threads: 1\nupstreams:\n  be:\n    name: be\n    targets:\n{targets}vhosts:\n  - id: v\n    name: v\n    server_names: [\"*\"]\n    listen_port: {port}\n    routes:\n      - id: api\n        path_prefix: /api\n        strip_prefix: true\n        target_type: proxy_upstream\n        upstream_name: be\n{extra_route}"
    )
}

struct Response {
    status: u16,
    headers: HashMap<String, String>,
    body: String,
}

async fn read_response(sock: &mut TcpStream, buf: &mut Vec<u8>) -> Response {
    let mut chunk = [0u8; 8192];
    let end = loop {
        if let Some(end) = find(buf, b"\r\n\r\n") {
            break end;
        }
        let n = tokio::time::timeout(Duration::from_secs(5), sock.read(&mut chunk))
            .await
            .expect("response timed out")
            .unwrap();
        assert!(n > 0, "connection closed before a response");
        buf.extend_from_slice(&chunk[..n]);
    };
    let head = String::from_utf8_lossy(&buf[..end]).to_string();
    buf.drain(..end + 4);
    let status = head.split(' ').nth(1).unwrap().parse().unwrap();
    let headers: HashMap<String, String> = head
        .lines()
        .skip(1)
        .filter_map(|l| l.split_once(':'))
        .map(|(k, v)| (k.trim().to_ascii_lowercase(), v.trim().to_string()))
        .collect();
    let mut body = Vec::new();
    if let Some(len) = headers.get("content-length") {
        let len: usize = len.parse().unwrap();
        while buf.len() < len {
            let n = sock.read(&mut chunk).await.unwrap();
            assert!(n > 0);
            buf.extend_from_slice(&chunk[..n]);
        }
        body.extend(buf.drain(..len));
    } else if headers
        .get("transfer-encoding")
        .is_some_and(|v| v == "chunked")
    {
        loop {
            let line_end = loop {
                if let Some(i) = find(buf, b"\r\n") {
                    break i;
                }
                let n = sock.read(&mut chunk).await.unwrap();
                buf.extend_from_slice(&chunk[..n]);
            };
            let size =
                usize::from_str_radix(std::str::from_utf8(&buf[..line_end]).unwrap(), 16).unwrap();
            buf.drain(..line_end + 2);
            while buf.len() < size + 2 {
                let n = sock.read(&mut chunk).await.unwrap();
                buf.extend_from_slice(&chunk[..n]);
            }
            body.extend(buf.drain(..size));
            buf.drain(..2);
            if size == 0 {
                break;
            }
        }
    } else {
        loop {
            let n = sock.read(&mut chunk).await.unwrap_or(0);
            if n == 0 {
                break;
            }
            buf.extend_from_slice(&chunk[..n]);
        }
        body.append(buf);
    }
    Response {
        status,
        headers,
        body: String::from_utf8_lossy(&body).to_string(),
    }
}

async fn request(addr: SocketAddr, raw: &str) -> Response {
    let mut sock = TcpStream::connect(addr).await.unwrap();
    sock.write_all(raw.as_bytes()).await.unwrap();
    read_response(&mut sock, &mut Vec::new()).await
}

#[tokio::test]
async fn plain_proxy_rewrites_the_request_and_keeps_the_connection() {
    let (up, seen) = upstream(Reply::Fixed("hello")).await;
    let port = closed_port().await;
    let addr = ferrix(site(port, &[format!("http://{up}")], "")).await;

    let mut sock = TcpStream::connect(addr).await.unwrap();
    let mut buf = Vec::new();
    for i in 0..2 {
        sock.write_all(
            format!("GET /api/items?page={i} HTTP/1.1\r\nHost: example.com\r\nConnection: keep-alive\r\nX-Forwarded-For: 10.0.0.9\r\nAccept: */*\r\n\r\n")
                .as_bytes(),
        )
        .await
        .unwrap();
        let resp = read_response(&mut sock, &mut buf).await;
        assert_eq!(resp.status, 200);
        assert_eq!(resp.body, "hello");
        assert!(resp.headers["server"].starts_with("ferrix"));
        assert_eq!(resp.headers["x-ferrix-upstream"], format!("http://{up}"));
        assert!(!resp.headers.contains_key("connection"));
    }

    let seen = seen.lock().unwrap();
    assert_eq!(seen.len(), 2);
    let first = &seen[0];
    assert!(
        first.starts_with("GET /items?page=0 HTTP/1.1\r\n"),
        "{first}"
    );
    assert_eq!(header(first, "host"), Some(up.to_string().as_str()));
    assert_eq!(
        header(first, "x-forwarded-for"),
        Some("10.0.0.9, 127.0.0.1")
    );
    assert_eq!(header(first, "x-real-ip"), Some("127.0.0.1"));
    assert_eq!(header(first, "x-forwarded-proto"), Some("http"));
    assert_eq!(header(first, "accept"), Some("*/*"));
    assert!(header(first, "connection").is_none());
}

#[tokio::test]
async fn chunked_upstream_responses_are_relayed() {
    let (up, _) = upstream(Reply::Chunked).await;
    let port = closed_port().await;
    let addr = ferrix(site(port, &[format!("http://{up}")], "")).await;
    let resp = request(addr, "GET /api/ HTTP/1.1\r\nHost: x\r\n\r\n").await;
    assert_eq!(resp.status, 200);
    assert_eq!(resp.body, "hello world");
}

#[tokio::test]
async fn http10_clients_get_the_connection_closed() {
    let (up, _) = upstream(Reply::Fixed("old")).await;
    let port = closed_port().await;
    let addr = ferrix(site(port, &[format!("http://{up}")], "")).await;
    let resp = request(addr, "GET /api/ HTTP/1.0\r\nHost: x\r\n\r\n").await;
    assert_eq!(resp.status, 200);
    assert_eq!(resp.body, "old");
    assert_eq!(
        resp.headers.get("connection").map(String::as_str),
        Some("close")
    );
}

#[tokio::test]
async fn a_dead_target_fails_over_to_the_next() {
    let (up, seen) = upstream(Reply::Fixed("alive")).await;
    let dead = closed_port().await;
    let port = closed_port().await;
    let addr = ferrix(site(
        port,
        &[format!("http://127.0.0.1:{dead}"), format!("http://{up}")],
        "",
    ))
    .await;
    for _ in 0..3 {
        let resp = request(addr, "GET /api/ HTTP/1.1\r\nHost: x\r\n\r\n").await;
        assert_eq!(resp.status, 200);
        assert_eq!(resp.body, "alive");
    }
    assert_eq!(seen.lock().unwrap().len(), 3);
}

#[tokio::test]
async fn every_target_down_is_a_bad_gateway() {
    let dead = closed_port().await;
    let port = closed_port().await;
    let addr = ferrix(site(port, &[format!("http://127.0.0.1:{dead}")], "")).await;
    let resp = request(addr, "GET /api/ HTTP/1.1\r\nHost: x\r\n\r\n").await;
    assert_eq!(resp.status, 502);
    assert!(resp.headers["server"].starts_with("ferrix"));
}

#[tokio::test]
async fn request_bodies_reach_the_upstream() {
    let (up, seen) = upstream(Reply::Fixed("stored")).await;
    let port = closed_port().await;
    let addr = ferrix(site(port, &[format!("http://{up}")], "")).await;
    let resp = request(
        addr,
        "POST /api/things HTTP/1.1\r\nHost: x\r\nContent-Type: application/json\r\nContent-Length: 13\r\n\r\n{\"name\":\"a\"}\n",
    )
    .await;
    assert_eq!(resp.status, 200);
    assert_eq!(resp.body, "stored");
    let seen = seen.lock().unwrap();
    assert!(
        seen[0].starts_with("POST /things HTTP/1.1\r\n"),
        "{}",
        seen[0]
    );
    assert!(seen[0].ends_with("{\"name\":\"a\"}\n"), "{}", seen[0]);
}

#[tokio::test]
async fn routes_with_extra_settings_use_the_full_pipeline() {
    let (up, _) = upstream(Reply::Fixed("decorated")).await;
    let port = closed_port().await;
    let extra = "      - id: tagged\n        path_prefix: /tagged\n        strip_prefix: true\n        target_type: proxy_upstream\n        upstream_name: be\n        add_headers:\n          X-Route: tagged\n";
    let addr = ferrix(site(port, &[format!("http://{up}")], extra)).await;
    let resp = request(addr, "GET /tagged/ HTTP/1.1\r\nHost: x\r\n\r\n").await;
    assert_eq!(resp.status, 200);
    assert_eq!(resp.body, "decorated");
    assert_eq!(
        resp.headers.get("x-route").map(String::as_str),
        Some("tagged")
    );
}

#[tokio::test]
async fn the_root_without_a_route_shows_the_welcome_page() {
    let (up, _) = upstream(Reply::Fixed("x")).await;
    let port = closed_port().await;
    let addr = ferrix(site(port, &[format!("http://{up}")], "")).await;
    let resp = request(addr, "GET / HTTP/1.1\r\nHost: x\r\n\r\n").await;
    assert_eq!(resp.status, 200);
    assert!(resp.body.contains("Ferrix is up and running"));
    let missing = request(addr, "GET /nothing HTTP/1.1\r\nHost: x\r\n\r\n").await;
    assert_eq!(missing.status, 404);
}

#[tokio::test]
async fn head_requests_get_no_body() {
    let (up, _) = upstream(Reply::Fixed("body")).await;
    let port = closed_port().await;
    let addr = ferrix(site(port, &[format!("http://{up}")], "")).await;
    let mut sock = TcpStream::connect(addr).await.unwrap();
    sock.write_all(b"HEAD /api/ HTTP/1.1\r\nHost: x\r\n\r\nGET /api/ HTTP/1.1\r\nHost: x\r\n\r\n")
        .await
        .unwrap();
    let mut buf = Vec::new();
    let mut chunk = [0u8; 4096];
    let head_end = loop {
        if let Some(i) = find(&buf, b"\r\n\r\n") {
            break i;
        }
        let n = sock.read(&mut chunk).await.unwrap();
        buf.extend_from_slice(&chunk[..n]);
    };
    assert!(buf.starts_with(b"HTTP/1.1 200"));
    buf.drain(..head_end + 4);
    let next = read_response(&mut sock, &mut buf).await;
    assert_eq!(next.status, 200);
    assert_eq!(next.body, "body");
}
