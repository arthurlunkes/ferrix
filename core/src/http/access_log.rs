use std::collections::HashMap;
use std::io::Write;
use std::sync::mpsc::{self, Sender};
use std::sync::{LazyLock, Mutex};

use crate::metrics::logger::RequestLog;

pub const COMBINED: &str = "$remote_addr - $remote_user [$time_local] \"$request\" $status \
$body_bytes_sent \"$http_referer\" \"$http_user_agent\"";

pub struct Extra<'a> {
    pub referer: &'a str,
    pub remote_user: &'a str,
    pub scheme: &'a str,
}

static WRITERS: LazyLock<Mutex<HashMap<String, Sender<String>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

fn writer_for(path: &str) -> Option<Sender<String>> {
    let mut map = WRITERS.lock().ok()?;
    if let Some(tx) = map.get(path) {
        return Some(tx.clone());
    }
    let file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|e| tracing::error!(path, error = %e, "cannot open access log"))
        .ok()?;
    let (tx, rx) = mpsc::channel::<String>();
    std::thread::Builder::new()
        .name("ferrix-access-log".into())
        .spawn(move || {
            let mut w = std::io::BufWriter::new(file);
            while let Ok(line) = rx.recv() {
                let _ = w.write_all(line.as_bytes());
                while let Ok(more) = rx.try_recv() {
                    let _ = w.write_all(more.as_bytes());
                }
                let _ = w.flush();
            }
        })
        .ok()?;
    map.insert(path.to_string(), tx.clone());
    Some(tx)
}

pub fn enabled(cfg: &crate::config::FerrixConfig) -> bool {
    !cfg.logging.access.targets.is_empty() || cfg.http.access_log.is_some()
}

pub fn emit(cfg: &crate::config::FerrixConfig, log: &RequestLog, extra: &Extra<'_>) {
    let access = &cfg.logging.access;
    if access.targets.is_empty() {
        if let Some(al) = &cfg.http.access_log {
            write(&al.path, &al.format, log, extra);
        }
        return;
    }
    let line = if access.format == "json" {
        render_json(log, extra)
    } else {
        render(&access.format, log, extra)
    };
    for t in &access.targets {
        crate::log_sink::send(t, 6, line.clone());
    }
}

pub fn render_json(log: &RequestLog, extra: &Extra<'_>) -> String {
    let mut m = serde_json::Map::new();
    for k in [
        "time_iso8601",
        "remote_addr",
        "remote_user",
        "request_method",
        "request_uri",
        "status",
        "body_bytes_sent",
        "request_time",
        "host",
        "http_referer",
        "http_user_agent",
        "upstream_addr",
        "scheme",
        "request_id",
    ] {
        if let Some(v) = variable(k, log, extra) {
            m.insert(k.into(), v.into());
        }
    }
    serde_json::Value::Object(m).to_string()
}

pub fn write(path: &str, format: &str, log: &RequestLog, extra: &Extra<'_>) {
    let mut line = render(format, log, extra);
    line.push('\n');
    if let Some(tx) = writer_for(path) {
        if tx.send(line).is_err() {
            if let Ok(mut m) = WRITERS.lock() {
                m.remove(path);
            }
        }
    }
}

fn dash(s: &str) -> &str {
    if s.is_empty() {
        "-"
    } else {
        s
    }
}

pub fn render(format: &str, log: &RequestLog, extra: &Extra<'_>) -> String {
    let fmt = if format.is_empty() || format == "combined" {
        COMBINED
    } else {
        format
    };
    let mut out = String::with_capacity(fmt.len() + 64);
    let mut rest = fmt;
    while let Some(i) = rest.find('$') {
        out.push_str(&rest[..i]);
        let after = &rest[i + 1..];
        let (name, tail) = if let Some(braced) = after.strip_prefix('{') {
            match braced.find('}') {
                Some(e) => (&braced[..e], &braced[e + 1..]),
                None => ("", after),
            }
        } else {
            let e = after
                .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
                .unwrap_or(after.len());
            (&after[..e], &after[e..])
        };
        match variable(name, log, extra) {
            Some(v) => out.push_str(&v),
            None => {
                out.push('$');
                out.push_str(name);
            }
        }
        rest = tail;
    }
    out.push_str(rest);
    out
}

fn variable(name: &str, log: &RequestLog, extra: &Extra<'_>) -> Option<String> {
    let path_q = log.uri.path_and_query().map(|p| p.as_str()).unwrap_or("/");
    Some(match name {
        "remote_addr" => log.client_ip.to_string(),
        "remote_user" => dash(extra.remote_user).to_string(),
        "time_local" => log
            .timestamp
            .with_timezone(&chrono::Local)
            .format("%d/%b/%Y:%H:%M:%S %z")
            .to_string(),
        "time_iso8601" => log
            .timestamp
            .with_timezone(&chrono::Local)
            .format("%Y-%m-%dT%H:%M:%S%:z")
            .to_string(),
        "msec" => format!("{:.3}", log.timestamp.timestamp_millis() as f64 / 1000.0),
        "request" => format!("{} {} HTTP/1.1", log.method, path_q),
        "request_method" => log.method.to_string(),
        "request_uri" => path_q.to_string(),
        "uri" => log.uri.path().to_string(),
        "args" | "query_string" => log.uri.query().unwrap_or("").to_string(),
        "status" => log.status.to_string(),
        "body_bytes_sent" | "bytes_sent" => log.bytes_sent.to_string(),
        "request_time" => format!("{:.3}", log.latency_ms / 1000.0),
        "host" => log.host_str().split(':').next().unwrap_or("").to_string(),
        "http_host" => log.host_str().to_string(),
        "http_referer" => dash(extra.referer).to_string(),
        "http_user_agent" => dash(
            log.user_agent
                .as_ref()
                .and_then(|v| v.to_str().ok())
                .unwrap_or(""),
        )
        .to_string(),
        "upstream_addr" => dash(
            log.upstream
                .as_ref()
                .and_then(|v| v.to_str().ok())
                .unwrap_or(""),
        )
        .to_string(),
        "scheme" => extra.scheme.to_string(),
        "request_id" => format!("{:016x}", log.id),
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::{HeaderValue, Method};

    fn log() -> RequestLog {
        RequestLog {
            id: 1,
            timestamp: chrono::Utc::now(),
            client_ip: "10.0.0.1".parse().unwrap(),
            method: Method::GET,
            uri: "/a?b=1".parse().unwrap(),
            host: HeaderValue::from_static("ex.com:8080"),
            status: 200,
            latency_ms: 12.0,
            bytes_sent: 42,
            upstream: None,
            user_agent: Some(HeaderValue::from_static("curl/8")),
            is_websocket: false,
        }
    }

    const EXTRA: Extra<'static> = Extra {
        referer: "",
        remote_user: "",
        scheme: "http",
    };

    #[test]
    fn combined_format() {
        let s = render("combined", &log(), &EXTRA);
        assert!(s.starts_with("10.0.0.1 - - ["), "{s}");
        assert!(
            s.ends_with("\"GET /a?b=1 HTTP/1.1\" 200 42 \"-\" \"curl/8\""),
            "{s}"
        );
    }

    #[test]
    fn custom_and_braced_variables() {
        let s = render("${host}|$request_time|$upstream_addr|$nope", &log(), &EXTRA);
        assert_eq!(s, "ex.com|0.012|-|$nope");
    }

    #[test]
    fn writes_to_file() {
        let p = std::env::temp_dir().join(format!("ferrix-al-{}.log", uuid::Uuid::new_v4()));
        let path = p.to_str().unwrap();
        write(path, "$status $uri", &log(), &EXTRA);
        for _ in 0..50 {
            if std::fs::read_to_string(&p).unwrap_or_default() == "200 /a\n" {
                let _ = std::fs::remove_file(&p);
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        panic!("line not written");
    }
}
