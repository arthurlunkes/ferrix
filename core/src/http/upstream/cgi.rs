use axum::body::{Body, Bytes};
use axum::http::{HeaderMap, HeaderName, HeaderValue, Response, StatusCode};
use std::collections::HashMap;
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

use crate::config::CgiProtocol;

const MAX_REPLY: usize = 64 << 20;

trait Io: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> Io for T {}

async fn connect(addr: &str, timeout: Duration) -> std::io::Result<Box<dyn Io>> {
    let addr = addr
        .trim_start_matches("http://")
        .trim_start_matches("fastcgi://");
    let fut = async {
        #[cfg(unix)]
        if let Some(path) = addr.strip_prefix("unix:") {
            let s = tokio::net::UnixStream::connect(path).await?;
            return Ok(Box::new(s) as Box<dyn Io>);
        }
        let s = tokio::net::TcpStream::connect(addr).await?;
        let _ = s.set_nodelay(true);
        Ok(Box::new(s) as Box<dyn Io>)
    };
    tokio::time::timeout(timeout, fut)
        .await
        .map_err(|_| std::io::Error::new(std::io::ErrorKind::TimedOut, "connect timed out"))?
}

const FCGI_BEGIN: u8 = 1;
const FCGI_END: u8 = 3;
const FCGI_PARAMS: u8 = 4;
const FCGI_STDIN: u8 = 5;
const FCGI_STDOUT: u8 = 6;
const FCGI_STDERR: u8 = 7;

fn fcgi_record(out: &mut Vec<u8>, ty: u8, content: &[u8]) {
    let pad = (8 - content.len() % 8) % 8;
    out.extend([1, ty, 0, 1]);
    out.extend((content.len() as u16).to_be_bytes());
    out.extend([pad as u8, 0]);
    out.extend(content);
    out.extend(std::iter::repeat_n(0, pad));
}

fn fcgi_len(out: &mut Vec<u8>, n: usize) {
    if n < 128 {
        out.push(n as u8);
    } else {
        out.extend(((n as u32) | 0x8000_0000).to_be_bytes());
    }
}

pub fn fastcgi_request(params: &[(String, String)], body: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(512 + body.len());
    fcgi_record(&mut out, FCGI_BEGIN, &[0, 1, 0, 0, 0, 0, 0, 0]);
    let mut p = Vec::new();
    for (k, v) in params {
        fcgi_len(&mut p, k.len());
        fcgi_len(&mut p, v.len());
        p.extend(k.as_bytes());
        p.extend(v.as_bytes());
    }
    for chunk in p.chunks(65535) {
        fcgi_record(&mut out, FCGI_PARAMS, chunk);
    }
    fcgi_record(&mut out, FCGI_PARAMS, &[]);
    for chunk in body.chunks(65535) {
        fcgi_record(&mut out, FCGI_STDIN, chunk);
    }
    fcgi_record(&mut out, FCGI_STDIN, &[]);
    out
}

async fn fastcgi_read(io: &mut (dyn Io + '_)) -> std::io::Result<Vec<u8>> {
    let mut stdout = Vec::new();
    loop {
        let mut h = [0u8; 8];
        match io.read_exact(&mut h).await {
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(stdout),
            Err(e) => return Err(e),
        }
        let len = u16::from_be_bytes([h[4], h[5]]) as usize;
        let pad = h[6] as usize;
        let mut content = vec![0u8; len + pad];
        io.read_exact(&mut content).await?;
        content.truncate(len);
        match h[1] {
            FCGI_STDOUT => {
                stdout.extend(content);
                if stdout.len() > MAX_REPLY {
                    return Err(std::io::Error::other("FastCGI reply too large"));
                }
            }
            FCGI_STDERR => {
                tracing::warn!(stderr = %String::from_utf8_lossy(&content), "FastCGI stderr")
            }
            FCGI_END => return Ok(stdout),
            _ => {}
        }
    }
}

pub fn scgi_request(params: &[(String, String)], body: &[u8]) -> Vec<u8> {
    let mut h = Vec::new();
    let cl = params
        .iter()
        .find(|(k, _)| k == "CONTENT_LENGTH")
        .map(|(_, v)| v.as_str())
        .filter(|v| !v.is_empty())
        .unwrap_or("0");
    for (k, v) in [("CONTENT_LENGTH", cl), ("SCGI", "1")] {
        h.extend(k.as_bytes());
        h.push(0);
        h.extend(v.as_bytes());
        h.push(0);
    }
    for (k, v) in params.iter().filter(|(k, _)| k != "CONTENT_LENGTH") {
        h.extend(k.as_bytes());
        h.push(0);
        h.extend(v.as_bytes());
        h.push(0);
    }
    let mut out = format!("{}:", h.len()).into_bytes();
    out.extend(h);
    out.push(b',');
    out.extend(body);
    out
}

pub fn uwsgi_request(params: &[(String, String)], body: &[u8]) -> Vec<u8> {
    let mut vars = Vec::new();
    for (k, v) in params {
        vars.extend((k.len() as u16).to_le_bytes());
        vars.extend(k.as_bytes());
        vars.extend((v.len() as u16).to_le_bytes());
        vars.extend(v.as_bytes());
    }
    let mut out = vec![0];
    out.extend((vars.len() as u16).to_le_bytes());
    out.push(0);
    out.extend(vars);
    out.extend(body);
    out
}

pub fn parse_reply(raw: &[u8]) -> Result<Response<Body>, String> {
    let split = raw
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .map(|i| (i, 4))
        .or_else(|| raw.windows(2).position(|w| w == b"\n\n").map(|i| (i, 2)))
        .ok_or("upstream sent no header terminator")?;
    let head = std::str::from_utf8(&raw[..split.0]).map_err(|_| "invalid header bytes")?;
    let body = Bytes::copy_from_slice(&raw[split.0 + split.1..]);

    let mut status = StatusCode::OK;
    let mut headers = HeaderMap::new();
    for (i, line) in head.lines().enumerate() {
        let line = line.trim_end_matches('\r');
        if i == 0 && line.starts_with("HTTP/") {
            if let Some(code) = line.split_whitespace().nth(1).and_then(|c| c.parse().ok()) {
                status = StatusCode::from_u16(code).unwrap_or(StatusCode::OK);
            }
            continue;
        }
        let Some((k, v)) = line.split_once(':') else {
            continue;
        };
        let v = v.trim();
        if k.eq_ignore_ascii_case("status") {
            if let Some(code) = v.split_whitespace().next().and_then(|c| c.parse().ok()) {
                status = StatusCode::from_u16(code).unwrap_or(StatusCode::OK);
            }
            continue;
        }
        if k.eq_ignore_ascii_case("transfer-encoding") || k.eq_ignore_ascii_case("connection") {
            continue;
        }
        if let (Ok(n), Ok(v)) = (
            HeaderName::from_bytes(k.as_bytes()),
            HeaderValue::from_str(v),
        ) {
            headers.append(n, v);
        }
    }
    if status == StatusCode::OK && headers.contains_key(axum::http::header::LOCATION) {
        status = StatusCode::FOUND;
    }
    headers.insert(
        axum::http::header::CONTENT_LENGTH,
        HeaderValue::from(body.len()),
    );
    let mut resp = Response::new(Body::from(body));
    *resp.status_mut() = status;
    *resp.headers_mut() = headers;
    Ok(resp)
}

pub async fn call(
    protocol: CgiProtocol,
    addr: &str,
    params: &[(String, String)],
    body: &[u8],
    connect_timeout: Duration,
    read_timeout: Duration,
) -> Result<Response<Body>, String> {
    let mut io = connect(addr, connect_timeout)
        .await
        .map_err(|e| format!("{addr}: {e}"))?;
    let req = match protocol {
        CgiProtocol::Fastcgi => fastcgi_request(params, body),
        CgiProtocol::Scgi => scgi_request(params, body),
        CgiProtocol::Uwsgi => uwsgi_request(params, body),
    };
    let exchange = async {
        io.write_all(&req).await?;
        io.flush().await?;
        match protocol {
            CgiProtocol::Fastcgi => fastcgi_read(&mut *io).await,
            _ => {
                let mut out = Vec::new();
                (&mut io)
                    .take(MAX_REPLY as u64)
                    .read_to_end(&mut out)
                    .await?;
                Ok(out)
            }
        }
    };
    let raw = tokio::time::timeout(read_timeout, exchange)
        .await
        .map_err(|_| "upstream timed out".to_string())?
        .map_err(|e| e.to_string())?;
    parse_reply(&raw)
}

#[allow(clippy::too_many_arguments)]
pub fn build_params(
    parts: &axum::http::request::Parts,
    document_root: &str,
    script_name: &str,
    path_info: &str,
    client_ip: std::net::IpAddr,
    is_tls: bool,
    body_len: usize,
    extra: &HashMap<String, String>,
) -> Vec<(String, String)> {
    let host = parts
        .headers
        .get(axum::http::header::HOST)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    let (server_name, server_port) = match host.rsplit_once(':') {
        Some((h, p)) if p.chars().all(|c| c.is_ascii_digit()) => (h, p.to_string()),
        _ => (host, if is_tls { "443" } else { "80" }.to_string()),
    };
    let root = document_root.trim_end_matches('/');
    let mut p: Vec<(String, String)> = vec![
        ("GATEWAY_INTERFACE".into(), "CGI/1.1".into()),
        ("SERVER_SOFTWARE".into(), "ferrix".into()),
        ("SERVER_PROTOCOL".into(), format!("{:?}", parts.version)),
        ("REQUEST_METHOD".into(), parts.method.to_string()),
        (
            "REQUEST_URI".into(),
            parts
                .uri
                .path_and_query()
                .map(|p| p.as_str())
                .unwrap_or("/")
                .into(),
        ),
        ("DOCUMENT_URI".into(), parts.uri.path().into()),
        (
            "QUERY_STRING".into(),
            parts.uri.query().unwrap_or("").into(),
        ),
        ("DOCUMENT_ROOT".into(), root.into()),
        ("SCRIPT_NAME".into(), script_name.into()),
        ("SCRIPT_FILENAME".into(), format!("{root}{script_name}")),
        ("PATH_INFO".into(), path_info.into()),
        ("REMOTE_ADDR".into(), client_ip.to_string()),
        ("SERVER_NAME".into(), server_name.into()),
        ("SERVER_PORT".into(), server_port),
        (
            "CONTENT_LENGTH".into(),
            if body_len > 0 {
                body_len.to_string()
            } else {
                String::new()
            },
        ),
        (
            "CONTENT_TYPE".into(),
            parts
                .headers
                .get(axum::http::header::CONTENT_TYPE)
                .and_then(|v| v.to_str().ok())
                .unwrap_or("")
                .into(),
        ),
        ("REDIRECT_STATUS".into(), "200".into()),
    ];
    if is_tls {
        p.push(("HTTPS".into(), "on".into()));
    }
    for (k, v) in &parts.headers {
        let name = format!("HTTP_{}", k.as_str().to_ascii_uppercase().replace('-', "_"));
        if name == "HTTP_CONTENT_TYPE" || name == "HTTP_CONTENT_LENGTH" || name == "HTTP_PROXY" {
            continue;
        }
        if let Ok(v) = v.to_str() {
            p.push((name, v.to_string()));
        }
    }
    for (k, v) in extra {
        let v = v
            .replace("$document_root", root)
            .replace("$fastcgi_script_name", script_name)
            .replace("$fastcgi_path_info", path_info);
        match p.iter_mut().find(|(n, _)| n == k) {
            Some(slot) => slot.1 = v,
            None => p.push((k.clone(), v)),
        }
    }
    p
}

pub fn split_script(path: &str, index: Option<&str>) -> (String, String) {
    if path.ends_with('/') {
        return (
            format!("{path}{}", index.unwrap_or("index.php")),
            String::new(),
        );
    }
    for ext in [".php/", ".py/", ".cgi/"] {
        if let Some(i) = path.find(ext) {
            let cut = i + ext.len() - 1;
            return (path[..cut].to_string(), path[cut..].to_string());
        }
    }
    (path.to_string(), String::new())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cgi_reply_with_status() {
        let r =
            parse_reply(b"Status: 404 Not Found\r\nContent-Type: text/plain\r\n\r\nnope").unwrap();
        assert_eq!(r.status(), 404);
        assert_eq!(r.headers()["content-type"], "text/plain");
        let r = parse_reply(b"HTTP/1.1 201 Created\r\nX-A: 1\r\n\r\n").unwrap();
        assert_eq!(r.status(), 201);
        let r = parse_reply(b"Location: /x\n\n").unwrap();
        assert_eq!(r.status(), 302);
    }

    #[test]
    fn split_paths() {
        assert_eq!(
            split_script("/a/index.php/x/y", None),
            ("/a/index.php".into(), "/x/y".into())
        );
        assert_eq!(
            split_script("/", Some("app.php")),
            ("/app.php".into(), String::new())
        );
    }

    #[test]
    fn encodings() {
        let p = vec![("A".to_string(), "b".to_string())];
        assert_eq!(
            scgi_request(&p, b"hi"),
            b"28:CONTENT_LENGTH\x000\x00SCGI\x001\x00A\x00b\x00,hi".to_vec()
        );
        assert_eq!(
            uwsgi_request(&p, b""),
            vec![0, 6, 0, 0, 1, 0, b'A', 1, 0, b'b']
        );
        let f = fastcgi_request(&p, b"");
        assert_eq!(&f[..8], &[1, FCGI_BEGIN, 0, 1, 0, 8, 0, 0]);
    }

    #[tokio::test]
    async fn fastcgi_roundtrip() {
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = l.local_addr().unwrap();
        tokio::spawn(async move {
            let (mut s, _) = l.accept().await.unwrap();
            let mut got = Vec::new();
            let mut buf = [0u8; 4096];
            loop {
                let n = s.read(&mut buf).await.unwrap();
                got.extend(&buf[..n]);
                if got
                    .windows(8)
                    .any(|w| w == [1, FCGI_STDIN, 0, 1, 0, 0, 0, 0])
                {
                    break;
                }
            }
            assert!(String::from_utf8_lossy(&got).contains("SCRIPT_FILENAME/srv/i.php"));
            let mut out = Vec::new();
            fcgi_record(
                &mut out,
                FCGI_STDOUT,
                b"Content-Type: text/html\r\n\r\n<h1>ok</h1>",
            );
            fcgi_record(&mut out, FCGI_END, &[0; 8]);
            s.write_all(&out).await.unwrap();
        });
        let params = vec![("SCRIPT_FILENAME".to_string(), "/srv/i.php".to_string())];
        let r = call(
            CgiProtocol::Fastcgi,
            &addr.to_string(),
            &params,
            b"",
            Duration::from_secs(1),
            Duration::from_secs(2),
        )
        .await
        .unwrap();
        let b = axum::body::to_bytes(r.into_body(), 1024).await.unwrap();
        assert_eq!(&b[..], b"<h1>ok</h1>");
    }
}
