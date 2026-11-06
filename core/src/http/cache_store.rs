use axum::body::Bytes;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;

use crate::config::{CacheStoreConfig, CacheStoreKind};

pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[derive(Debug, Clone, PartialEq)]
pub struct StoredEntry {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Bytes,
    pub expires_ms: u64,
    pub stale_until_ms: u64,
}

const MAGIC: &[u8; 4] = b"FXC1";

pub fn encode(e: &StoredEntry) -> Vec<u8> {
    let mut out = Vec::with_capacity(e.body.len() + 256);
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(&e.status.to_be_bytes());
    out.extend_from_slice(&e.expires_ms.to_be_bytes());
    out.extend_from_slice(&e.stale_until_ms.to_be_bytes());
    out.extend_from_slice(&(e.headers.len() as u32).to_be_bytes());
    for (k, v) in &e.headers {
        out.extend_from_slice(&(k.len() as u32).to_be_bytes());
        out.extend_from_slice(k.as_bytes());
        out.extend_from_slice(&(v.len() as u32).to_be_bytes());
        out.extend_from_slice(v.as_bytes());
    }
    out.extend_from_slice(&e.body);
    out
}

pub fn decode(b: &[u8]) -> Option<StoredEntry> {
    let mut p = 0usize;
    let take = |p: &mut usize, n: usize| -> Option<&[u8]> {
        let s = b.get(*p..*p + n)?;
        *p += n;
        Some(s)
    };
    if take(&mut p, 4)? != MAGIC {
        return None;
    }
    let status = u16::from_be_bytes(take(&mut p, 2)?.try_into().ok()?);
    let expires_ms = u64::from_be_bytes(take(&mut p, 8)?.try_into().ok()?);
    let stale_until_ms = u64::from_be_bytes(take(&mut p, 8)?.try_into().ok()?);
    let n = u32::from_be_bytes(take(&mut p, 4)?.try_into().ok()?) as usize;
    let mut headers = Vec::with_capacity(n.min(128));
    for _ in 0..n {
        let kl = u32::from_be_bytes(take(&mut p, 4)?.try_into().ok()?) as usize;
        let k = String::from_utf8(take(&mut p, kl)?.to_vec()).ok()?;
        let vl = u32::from_be_bytes(take(&mut p, 4)?.try_into().ok()?) as usize;
        let v = String::from_utf8(take(&mut p, vl)?.to_vec()).ok()?;
        headers.push((k, v));
    }
    Some(StoredEntry {
        status,
        headers,
        body: Bytes::copy_from_slice(&b[p..]),
        expires_ms,
        stale_until_ms,
    })
}

fn md5_hex(key: &str) -> String {
    use md5::{Digest, Md5};
    Md5::digest(key.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

pub fn disk_path(dir: &Path, key: &str) -> PathBuf {
    let h = md5_hex(key);
    dir.join(&h[31..]).join(&h[29..31]).join(h)
}

static LAST_SWEEP: LazyLock<Mutex<HashMap<PathBuf, u64>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

fn sweep(dir: &Path, max_bytes: u64) {
    let now = now_ms();
    let mut files = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&d) else {
            continue;
        };
        for e in rd.flatten() {
            let path = e.path();
            let Ok(md) = e.metadata() else { continue };
            if md.is_dir() {
                stack.push(path);
            } else {
                let mut head = [0u8; 22];
                let expired = std::fs::File::open(&path)
                    .and_then(|mut f| std::io::Read::read_exact(&mut f, &mut head))
                    .ok()
                    .map(|_| u64::from_be_bytes(head[14..22].try_into().unwrap_or([0; 8])) < now)
                    .unwrap_or(true);
                if expired {
                    let _ = std::fs::remove_file(&path);
                } else {
                    files.push((md.modified().unwrap_or(UNIX_EPOCH), md.len(), path));
                }
            }
        }
    }
    let mut total: u64 = files.iter().map(|f| f.1).sum();
    if max_bytes == 0 || total <= max_bytes {
        return;
    }
    files.sort_by_key(|f| f.0);
    for (_, len, path) in files {
        if total <= max_bytes {
            break;
        }
        if std::fs::remove_file(&path).is_ok() {
            total -= len;
        }
    }
}

fn maybe_sweep(dir: &Path, max_bytes: u64) {
    let now = now_ms();
    if let Ok(mut m) = LAST_SWEEP.lock() {
        let last = m.entry(dir.to_path_buf()).or_insert(0);
        if now.saturating_sub(*last) < 60_000 {
            return;
        }
        *last = now;
    }
    let dir = dir.to_path_buf();
    tokio::task::spawn_blocking(move || sweep(&dir, max_bytes));
}

async fn disk_get(dir: &Path, key: &str) -> Option<StoredEntry> {
    let data = tokio::fs::read(disk_path(dir, key)).await.ok()?;
    decode(&data).filter(|e| e.stale_until_ms > now_ms())
}

async fn disk_put(dir: &Path, key: &str, e: &StoredEntry, max_bytes: u64) {
    let path = disk_path(dir, key);
    if let Some(parent) = path.parent() {
        if tokio::fs::create_dir_all(parent).await.is_err() {
            return;
        }
    }
    let tmp = path.with_extension(format!("tmp{}", uuid::Uuid::new_v4().simple()));
    if tokio::fs::write(&tmp, encode(e)).await.is_ok()
        && tokio::fs::rename(&tmp, &path).await.is_err()
    {
        let _ = tokio::fs::remove_file(&tmp).await;
    }
    maybe_sweep(dir, max_bytes);
}

struct RedisTarget {
    addr: String,
    password: Option<String>,
    username: Option<String>,
    db: Option<u32>,
}

fn parse_redis_url(url: &str) -> Option<RedisTarget> {
    let rest = url.strip_prefix("redis://")?;
    let (auth, hostpart) = match rest.rsplit_once('@') {
        Some((a, h)) => (Some(a), h),
        None => (None, rest),
    };
    let (addr, db) = match hostpart.split_once('/') {
        Some((a, d)) => (a, d.parse().ok()),
        None => (hostpart, None),
    };
    let (username, password) = match auth {
        Some(a) => match a.split_once(':') {
            Some((u, p)) => ((!u.is_empty()).then(|| u.to_string()), Some(p.to_string())),
            None => (None, Some(a.to_string())),
        },
        None => (None, None),
    };
    let addr = if addr.contains(':') {
        addr.to_string()
    } else {
        format!("{addr}:6379")
    };
    Some(RedisTarget {
        addr,
        password,
        username,
        db,
    })
}

type Conn = BufReader<TcpStream>;

static POOLS: LazyLock<Mutex<HashMap<String, Vec<Conn>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

fn command(args: &[&[u8]]) -> Vec<u8> {
    let mut out = format!("*{}\r\n", args.len()).into_bytes();
    for a in args {
        out.extend_from_slice(format!("${}\r\n", a.len()).as_bytes());
        out.extend_from_slice(a);
        out.extend_from_slice(b"\r\n");
    }
    out
}

enum Reply {
    Ok,
    Nil,
    Bulk(Vec<u8>),
    Int,
    Err(String),
}

async fn read_reply(c: &mut Conn) -> std::io::Result<Reply> {
    let mut line = String::new();
    if c.read_line(&mut line).await? == 0 {
        return Err(std::io::Error::from(std::io::ErrorKind::UnexpectedEof));
    }
    let line = line.trim_end();
    let (kind, rest) = line.split_at(1.min(line.len()));
    Ok(match kind {
        "+" => Reply::Ok,
        ":" => Reply::Int,
        "-" => Reply::Err(rest.to_string()),
        "$" => {
            let n: i64 = rest.parse().unwrap_or(-1);
            if n < 0 {
                Reply::Nil
            } else {
                let mut buf = vec![0u8; n as usize + 2];
                c.read_exact(&mut buf).await?;
                buf.truncate(n as usize);
                Reply::Bulk(buf)
            }
        }
        _ => Reply::Err(format!("unexpected reply {line}")),
    })
}

async fn redis_conn(url: &str) -> Option<Conn> {
    if let Some(c) = POOLS.lock().ok()?.get_mut(url).and_then(|v| v.pop()) {
        return Some(c);
    }
    let t = parse_redis_url(url)?;
    let tcp = tokio::time::timeout(Duration::from_secs(2), TcpStream::connect(&t.addr))
        .await
        .ok()?
        .ok()?;
    let _ = tcp.set_nodelay(true);
    let mut c = BufReader::new(tcp);
    if let Some(p) = &t.password {
        let cmd = match &t.username {
            Some(u) => command(&[b"AUTH", u.as_bytes(), p.as_bytes()]),
            None => command(&[b"AUTH", p.as_bytes()]),
        };
        c.get_mut().write_all(&cmd).await.ok()?;
        if let Reply::Err(e) = read_reply(&mut c).await.ok()? {
            tracing::error!(error = %e, "redis AUTH failed");
            return None;
        }
    }
    if let Some(db) = t.db {
        c.get_mut()
            .write_all(&command(&[b"SELECT", db.to_string().as_bytes()]))
            .await
            .ok()?;
        read_reply(&mut c).await.ok()?;
    }
    Some(c)
}

fn release(url: &str, c: Conn) {
    if let Ok(mut p) = POOLS.lock() {
        let v = p.entry(url.to_string()).or_default();
        if v.len() < 32 {
            v.push(c);
        }
    }
}

async fn redis_call(url: &str, cmd: Vec<u8>) -> Option<Reply> {
    let mut c = redis_conn(url).await?;
    let r = tokio::time::timeout(Duration::from_secs(2), async {
        c.get_mut().write_all(&cmd).await?;
        read_reply(&mut c).await
    })
    .await
    .ok()?;
    match r {
        Ok(reply) => {
            release(url, c);
            Some(reply)
        }
        Err(e) => {
            tracing::debug!(error = %e, "redis call failed");
            None
        }
    }
}

pub struct Store<'a> {
    pub cfg: &'a CacheStoreConfig,
}

impl Store<'_> {
    fn redis_key(&self, key: &str) -> String {
        format!(
            "{}{}",
            self.cfg.prefix.as_deref().unwrap_or("ferrix:cache:"),
            key
        )
    }

    pub async fn get(&self, key: &str) -> Option<StoredEntry> {
        match self.cfg.kind {
            CacheStoreKind::Memory => None,
            CacheStoreKind::Disk => disk_get(Path::new(self.cfg.path.as_deref()?), key).await,
            CacheStoreKind::Redis => {
                let url = self.cfg.url.as_deref()?;
                match redis_call(url, command(&[b"GET", self.redis_key(key).as_bytes()])).await? {
                    Reply::Bulk(b) => decode(&b).filter(|e| e.stale_until_ms > now_ms()),
                    _ => None,
                }
            }
        }
    }

    pub async fn put(&self, key: &str, e: &StoredEntry) {
        match self.cfg.kind {
            CacheStoreKind::Memory => {}
            CacheStoreKind::Disk => {
                if let Some(p) = self.cfg.path.as_deref() {
                    disk_put(Path::new(p), key, e, self.cfg.max_bytes).await;
                }
            }
            CacheStoreKind::Redis => {
                let Some(url) = self.cfg.url.as_deref() else {
                    return;
                };
                let ttl = e.stale_until_ms.saturating_sub(now_ms()).max(1000);
                let cmd = command(&[
                    b"SET",
                    self.redis_key(key).as_bytes(),
                    &encode(e),
                    b"PX",
                    ttl.to_string().as_bytes(),
                ]);
                if let Some(Reply::Err(err)) = redis_call(url, cmd).await {
                    tracing::warn!(error = %err, "redis SET failed");
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry() -> StoredEntry {
        StoredEntry {
            status: 200,
            headers: vec![("content-type".into(), "text/plain".into())],
            body: Bytes::from_static(b"hello"),
            expires_ms: now_ms() + 60_000,
            stale_until_ms: now_ms() + 120_000,
        }
    }

    #[test]
    fn encode_roundtrip() {
        let e = entry();
        assert_eq!(decode(&encode(&e)).unwrap(), e);
        assert!(decode(b"nope").is_none());
    }

    #[test]
    fn redis_urls() {
        let t = parse_redis_url("redis://user:pw@cache.local:6380/2").unwrap();
        assert_eq!(
            (
                t.addr.as_str(),
                t.username.as_deref(),
                t.password.as_deref(),
                t.db
            ),
            ("cache.local:6380", Some("user"), Some("pw"), Some(2))
        );
        let t = parse_redis_url("redis://:pw@10.0.0.1").unwrap();
        assert_eq!(
            (t.addr.as_str(), t.username, t.password.as_deref()),
            ("10.0.0.1:6379", None, Some("pw"))
        );
    }

    #[tokio::test]
    async fn disk_store_roundtrip_and_sweep() {
        let dir = std::env::temp_dir().join(format!("ferrix-dc-{}", uuid::Uuid::new_v4()));
        let cfg = CacheStoreConfig {
            kind: CacheStoreKind::Disk,
            path: Some(dir.to_string_lossy().into()),
            ..Default::default()
        };
        let s = Store { cfg: &cfg };
        s.put("GET:x:/a", &entry()).await;
        assert_eq!(
            s.get("GET:x:/a").await.unwrap().body,
            Bytes::from_static(b"hello")
        );
        let mut old = entry();
        old.expires_ms = 1;
        old.stale_until_ms = 1;
        s.put("GET:x:/old", &old).await;
        assert!(s.get("GET:x:/old").await.is_none());
        sweep(&dir, 0);
        assert!(!disk_path(&dir, "GET:x:/old").exists());
        assert!(disk_path(&dir, "GET:x:/a").exists());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn redis_store_against_a_fake_server() {
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = l.local_addr().unwrap();
        tokio::spawn(async move {
            let (s, _) = l.accept().await.unwrap();
            let mut c = BufReader::new(s);
            let mut stored: Option<Vec<u8>> = None;
            loop {
                let mut line = String::new();
                if c.read_line(&mut line).await.unwrap_or(0) == 0 {
                    return;
                }
                let n: usize = line.trim()[1..].parse().unwrap();
                let mut args = Vec::new();
                for _ in 0..n {
                    let mut l2 = String::new();
                    c.read_line(&mut l2).await.unwrap();
                    let len: usize = l2.trim()[1..].parse().unwrap();
                    let mut buf = vec![0u8; len + 2];
                    c.read_exact(&mut buf).await.unwrap();
                    buf.truncate(len);
                    args.push(buf);
                }
                let reply = match args[0].as_slice() {
                    b"SET" => {
                        assert_eq!(args[1], b"ferrix:cache:k");
                        stored = Some(args[2].clone());
                        b"+OK\r\n".to_vec()
                    }
                    b"GET" => match &stored {
                        Some(v) => {
                            let mut r = format!("${}\r\n", v.len()).into_bytes();
                            r.extend_from_slice(v);
                            r.extend_from_slice(b"\r\n");
                            r
                        }
                        None => b"$-1\r\n".to_vec(),
                    },
                    _ => b"-ERR\r\n".to_vec(),
                };
                c.get_mut().write_all(&reply).await.unwrap();
            }
        });
        let cfg = CacheStoreConfig {
            kind: CacheStoreKind::Redis,
            url: Some(format!("redis://{addr}")),
            ..Default::default()
        };
        let s = Store { cfg: &cfg };
        assert!(s.get("k").await.is_none());
        s.put("k", &entry()).await;
        assert_eq!(s.get("k").await.unwrap().status, 200);
    }
}
