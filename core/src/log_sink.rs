use std::collections::HashMap;
use std::io::Write;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};

use crate::config::LogTarget;

pub struct Line {
    pub severity: u8,
    pub text: String,
}

pub type Sink = Sender<Line>;

static SINKS: LazyLock<Mutex<HashMap<LogTarget, Sink>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

pub fn sink(target: &LogTarget) -> Option<Sink> {
    if matches!(target, LogTarget::Off) {
        return None;
    }
    let mut map = SINKS.lock().ok()?;
    if let Some(tx) = map.get(target) {
        return Some(tx.clone());
    }
    let (tx, rx) = mpsc::channel::<Line>();
    let t = target.clone();
    std::thread::Builder::new()
        .name("ferrix-log".into())
        .spawn(move || run(t, rx))
        .ok()?;
    map.insert(target.clone(), tx.clone());
    Some(tx)
}

pub fn send(target: &LogTarget, severity: u8, text: String) {
    if let Some(tx) = sink(target) {
        if tx.send(Line { severity, text }).is_err() {
            if let Ok(mut m) = SINKS.lock() {
                m.remove(target);
            }
        }
    }
}

fn run(target: LogTarget, rx: Receiver<Line>) {
    match target {
        LogTarget::Off => {}
        LogTarget::Stdout => stream(std::io::stdout(), rx),
        LogTarget::Stderr => stream(std::io::stderr(), rx),
        LogTarget::File {
            path,
            max_bytes,
            keep,
        } => file(&path, max_bytes, keep, rx),
        LogTarget::Syslog {
            addr,
            facility,
            tag,
        } => syslog(
            addr.as_deref(),
            facility_code(facility.as_deref()),
            tag.as_deref().unwrap_or("ferrix"),
            rx,
        ),
        LogTarget::Http {
            url,
            batch_size,
            flush_ms,
            headers,
        } => http(
            &url,
            batch_size.unwrap_or(500).max(1),
            Duration::from_millis(flush_ms.unwrap_or(1000).max(10)),
            &headers,
            rx,
        ),
    }
}

fn ensure_newline(mut s: String) -> String {
    if !s.ends_with('\n') {
        s.push('\n');
    }
    s
}

fn stream<W: Write>(w: W, rx: Receiver<Line>) {
    let mut w = std::io::LineWriter::new(w);
    while let Ok(l) = rx.recv() {
        let _ = w.write_all(ensure_newline(l.text).as_bytes());
    }
}

fn open_append(path: &str) -> Option<std::fs::File> {
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|e| eprintln!("ferrix: cannot open log file {}: {}", path, e))
        .ok()
}

pub fn rotate(path: &str, keep: u32) {
    if keep == 0 {
        let _ = std::fs::remove_file(path);
        return;
    }
    for i in (1..keep).rev() {
        let _ = std::fs::rename(format!("{}.{}", path, i), format!("{}.{}", path, i + 1));
    }
    let _ = std::fs::rename(path, format!("{}.1", path));
}

fn file(path: &str, max_bytes: u64, keep: u32, rx: Receiver<Line>) {
    let Some(f) = open_append(path) else {
        while rx.recv().is_ok() {}
        return;
    };
    let mut size = f.metadata().map(|m| m.len()).unwrap_or(0);
    let mut w = std::io::BufWriter::new(f);
    while let Ok(first) = rx.recv() {
        let mut next = Some(first);
        while let Some(l) = next {
            let text = ensure_newline(l.text);
            size += text.len() as u64;
            let _ = w.write_all(text.as_bytes());
            next = rx.try_recv().ok();
        }
        let _ = w.flush();
        if max_bytes > 0 && size >= max_bytes {
            rotate(path, keep);
            if let Some(f) = open_append(path) {
                w = std::io::BufWriter::new(f);
                size = 0;
            }
        }
    }
}

fn facility_code(name: Option<&str>) -> u8 {
    match name.unwrap_or("local7") {
        "kern" => 0,
        "user" => 1,
        "mail" => 2,
        "daemon" => 3,
        "auth" => 4,
        "syslog" => 5,
        "local0" => 16,
        "local1" => 17,
        "local2" => 18,
        "local3" => 19,
        "local4" => 20,
        "local5" => 21,
        "local6" => 22,
        _ => 23,
    }
}

pub fn syslog_frame(facility: u8, severity: u8, tag: &str, text: &str) -> String {
    let time = chrono::Local::now().format("%b %e %H:%M:%S");
    format!(
        "<{}>{} {}: {}",
        facility as u16 * 8 + severity as u16,
        time,
        tag,
        text.trim_end_matches('\n')
    )
}

fn syslog(addr: Option<&str>, facility: u8, tag: &str, rx: Receiver<Line>) {
    let addr = addr.unwrap_or("/dev/log");
    let send: Box<dyn Fn(&[u8])> = if addr.starts_with('/') {
        #[cfg(unix)]
        {
            let Ok(s) = std::os::unix::net::UnixDatagram::unbound() else {
                return;
            };
            let p = addr.to_string();
            Box::new(move |b| {
                let _ = s.send_to(b, &p);
            })
        }
        #[cfg(not(unix))]
        {
            return;
        }
    } else {
        let Ok(s) = std::net::UdpSocket::bind(if addr.starts_with('[') {
            "[::]:0"
        } else {
            "0.0.0.0:0"
        }) else {
            return;
        };
        let a = addr.to_string();
        Box::new(move |b| {
            let _ = s.send_to(b, a.as_str());
        })
    };
    while let Ok(l) = rx.recv() {
        send(syslog_frame(facility, l.severity, tag, &l.text).as_bytes());
    }
}

struct HttpUrl {
    host: String,
    addr: String,
    path: String,
}

pub fn parse_http_url(url: &str) -> Result<(String, String, String), String> {
    let rest = url
        .strip_prefix("http://")
        .ok_or_else(|| "only http:// URLs are supported; put an agent such as Vector or Fluent Bit in front for TLS".to_string())?;
    let (hostport, path) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, "/"),
    };
    if hostport.is_empty() {
        return Err("missing host".into());
    }
    let addr = if hostport
        .rsplit_once(':')
        .is_some_and(|(_, p)| p.parse::<u16>().is_ok())
    {
        hostport.to_string()
    } else {
        format!("{}:80", hostport)
    };
    Ok((hostport.to_string(), addr, path.to_string()))
}

fn post(url: &HttpUrl, headers: &std::collections::BTreeMap<String, String>, body: &[u8]) {
    use std::io::Read;
    use std::net::ToSocketAddrs;
    let Some(sa) = url.addr.to_socket_addrs().ok().and_then(|mut a| a.next()) else {
        return;
    };
    let Ok(mut s) = std::net::TcpStream::connect_timeout(&sa, Duration::from_secs(3)) else {
        return;
    };
    let _ = s.set_write_timeout(Some(Duration::from_secs(5)));
    let _ = s.set_read_timeout(Some(Duration::from_secs(5)));
    let mut head = format!(
        "POST {} HTTP/1.1\r\nHost: {}\r\nContent-Length: {}\r\nConnection: close\r\n",
        url.path,
        url.host,
        body.len()
    );
    if !headers
        .keys()
        .any(|k| k.eq_ignore_ascii_case("content-type"))
    {
        head.push_str("Content-Type: application/x-ndjson\r\n");
    }
    for (k, v) in headers {
        head.push_str(&format!("{}: {}\r\n", k, v));
    }
    head.push_str("\r\n");
    if s.write_all(head.as_bytes())
        .and_then(|_| s.write_all(body))
        .is_ok()
    {
        let mut buf = [0u8; 512];
        let _ = s.read(&mut buf);
    }
}

fn http(
    url: &str,
    batch: usize,
    flush: Duration,
    headers: &std::collections::BTreeMap<String, String>,
    rx: Receiver<Line>,
) {
    let Ok((host, addr, path)) = parse_http_url(url) else {
        while rx.recv().is_ok() {}
        return;
    };
    let url = HttpUrl { host, addr, path };
    let mut buf: Vec<u8> = Vec::new();
    let mut count = 0usize;
    let mut deadline = Instant::now() + flush;
    loop {
        let wait = deadline.saturating_duration_since(Instant::now());
        match rx.recv_timeout(wait) {
            Ok(l) => {
                buf.extend_from_slice(ensure_newline(l.text).as_bytes());
                count += 1;
                if count < batch {
                    continue;
                }
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => {
                if !buf.is_empty() {
                    post(&url, headers, &buf);
                }
                return;
            }
        }
        if !buf.is_empty() {
            post(&url, headers, &buf);
            buf.clear();
            count = 0;
        }
        deadline = Instant::now() + flush;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn url_parsing() {
        let (h, a, p) = parse_http_url("http://loki:3100/loki/api/v1/push").unwrap();
        assert_eq!(
            (h.as_str(), a.as_str(), p.as_str()),
            ("loki:3100", "loki:3100", "/loki/api/v1/push")
        );
        let (_, a, p) = parse_http_url("http://vector").unwrap();
        assert_eq!((a.as_str(), p.as_str()), ("vector:80", "/"));
        assert!(parse_http_url("https://x").is_err());
    }

    #[test]
    fn syslog_priority() {
        let f = syslog_frame(23, 6, "ferrix", "hello\n");
        assert!(f.starts_with("<190>"), "{f}");
        assert!(f.ends_with(" ferrix: hello"), "{f}");
    }

    #[test]
    fn file_rotation() {
        let dir = std::env::temp_dir().join(format!("ferrix-rot-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("a.log");
        let p = path.to_str().unwrap().to_string();
        let t = LogTarget::File {
            path: p.clone(),
            max_bytes: 10,
            keep: 2,
        };
        for i in 0..3 {
            send(&t, 6, format!("line-number-{}", i));
            for _ in 0..100 {
                if std::fs::metadata(format!("{}.1", p)).is_ok() && i == 0 {
                    break;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
        }
        std::thread::sleep(Duration::from_millis(200));
        assert!(std::fs::metadata(format!("{}.1", p)).is_ok());
        assert!(std::fs::metadata(format!("{}.2", p)).is_ok());
        assert!(std::fs::metadata(format!("{}.3", p)).is_err());
        let _ = std::fs::remove_dir_all(dir);
    }
}
