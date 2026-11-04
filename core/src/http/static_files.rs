use axum::body::{Body, Bytes};
use axum::http::{header, HeaderMap, HeaderValue, Response, StatusCode};
use std::collections::HashMap;
use std::io::SeekFrom;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, LazyLock, RwLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::io::{AsyncReadExt, AsyncSeekExt};
use tokio::sync::OnceCell;
use tracing::warn;

use crate::config::{CompressionAlgorithm, CompressionConfig, StaticCacheConfig};
use crate::http::filters::compress;

const REVALIDATE_AFTER: Duration = Duration::from_secs(1);

struct CachedFile {
    body: Bytes,
    meta: FileMeta,

    file_path: PathBuf,

    checked_at: AtomicU64,

    variants: [OnceCell<Option<Bytes>>; 3],
}

#[derive(Clone)]
struct FileMeta {
    content_type: HeaderValue,
    len: u64,
    modified: Option<SystemTime>,
    etag: HeaderValue,
    last_modified: Option<HeaderValue>,
}

impl FileMeta {
    fn new(path: &Path, metadata: &std::fs::Metadata) -> Self {
        let mime = mime_guess::from_path(path).first_or_octet_stream();
        let content_type = HeaderValue::from_str(mime.as_ref())
            .unwrap_or(HeaderValue::from_static("application/octet-stream"));
        let modified = metadata.modified().ok();
        let mtime = modified
            .and_then(|m| m.duration_since(UNIX_EPOCH).ok())
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let etag = HeaderValue::from_str(&format!("\"{:x}-{:x}\"", mtime, metadata.len()))
            .unwrap_or(HeaderValue::from_static("\"0\""));
        let last_modified = modified.and_then(|m| {
            let dt: chrono::DateTime<chrono::Utc> = m.into();
            HeaderValue::from_str(&dt.format("%a, %d %b %Y %H:%M:%S GMT").to_string()).ok()
        });
        Self {
            content_type,
            len: metadata.len(),
            modified,
            etag,
            last_modified,
        }
    }

    fn mtime_secs(&self) -> Option<i64> {
        self.modified
            .and_then(|m| m.duration_since(UNIX_EPOCH).ok())
            .map(|d| d.as_secs() as i64)
    }

    fn apply(&self, h: &mut HeaderMap) {
        h.insert(header::CONTENT_TYPE, self.content_type.clone());
        h.insert(header::ETAG, self.etag.clone());
        if let Some(lm) = &self.last_modified {
            h.insert(header::LAST_MODIFIED, lm.clone());
        }
        h.insert(
            header::CACHE_CONTROL,
            HeaderValue::from_static("public, max-age=3600"),
        );
    }
}

#[derive(Default)]
struct FileCache {
    entries: HashMap<String, Arc<CachedFile>>,
    total_bytes: u64,
}

static CACHE: LazyLock<RwLock<FileCache>> = LazyLock::new(|| RwLock::new(FileCache::default()));

pub struct StaticRequest<'a> {
    pub headers: &'a HeaderMap,
    pub encoding: Option<(CompressionAlgorithm, &'a CompressionConfig)>,
    pub cache: &'a StaticCacheConfig,
}

pub struct StaticFileServer;

impl StaticFileServer {
    pub async fn serve_file(
        root_dir: &str,
        path: &str,
        try_files: Option<&str>,
        req: &StaticRequest<'_>,
    ) -> Result<Response<Body>, StatusCode> {
        let key = format!("{}\0{}\0{}", root_dir, path, try_files.unwrap_or(""));

        let hit = CACHE.read().ok().and_then(|c| c.entries.get(&key).cloned());
        if let Some(entry) = hit {
            let age = crate::event::clock::mono_ms()
                .saturating_sub(entry.checked_at.load(Ordering::Relaxed));
            let fresh = age < REVALIDATE_AFTER.as_millis() as u64;
            if fresh || still_same_file(&entry).await {
                return Ok(respond_cached(&entry, req).await);
            }
            if let Ok(mut c) = CACHE.write() {
                if let Some(old) = c.entries.remove(&key) {
                    c.total_bytes -= old.meta.len;
                }
            }
        }

        let loaded = load_file(root_dir, path, try_files, req.cache.max_file_bytes)?;

        match loaded {
            Loaded::Cacheable(entry) => {
                let entry: Arc<CachedFile> = Arc::from(entry);
                if let Ok(mut c) = CACHE.write() {
                    if c.total_bytes + entry.meta.len <= req.cache.max_total_bytes {
                        c.total_bytes += entry.meta.len;
                        if let Some(old) = c.entries.insert(key, entry.clone()) {
                            c.total_bytes -= old.meta.len;
                        }
                    }
                }
                Ok(respond_cached(&entry, req).await)
            }
            Loaded::Large(path, meta) => respond_large(path, meta, req).await,
        }
    }
}

enum Loaded {
    Cacheable(Box<CachedFile>),
    Large(PathBuf, FileMeta),
}

enum Conditional {
    NotModified,
    Proceed,
}

fn check_conditional(meta: &FileMeta, headers: &HeaderMap) -> Conditional {
    if let Some(inm) = headers
        .get(header::IF_NONE_MATCH)
        .and_then(|v| v.to_str().ok())
    {
        let ours = meta.etag.to_str().unwrap_or("");
        let matches = inm
            .split(',')
            .map(str::trim)
            .any(|t| t == "*" || t.trim_start_matches("W/") == ours.trim_start_matches("W/"));
        return if matches {
            Conditional::NotModified
        } else {
            Conditional::Proceed
        };
    }
    if let (Some(ims), Some(mtime)) = (
        headers
            .get(header::IF_MODIFIED_SINCE)
            .and_then(|v| v.to_str().ok())
            .and_then(parse_http_date),
        meta.mtime_secs(),
    ) {
        if mtime <= ims {
            return Conditional::NotModified;
        }
    }
    Conditional::Proceed
}

fn parse_http_date(s: &str) -> Option<i64> {
    chrono::DateTime::parse_from_rfc2822(s.trim())
        .ok()
        .map(|d| d.timestamp())
}

fn not_modified(meta: &FileMeta) -> Response<Body> {
    let mut resp = Response::new(Body::empty());
    *resp.status_mut() = StatusCode::NOT_MODIFIED;
    let h = resp.headers_mut();
    h.insert(header::ETAG, meta.etag.clone());
    if let Some(lm) = &meta.last_modified {
        h.insert(header::LAST_MODIFIED, lm.clone());
    }
    h.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("public, max-age=3600"),
    );
    resp
}

enum RangeReq {
    None,
    Satisfiable(u64, u64),
    Unsatisfiable,
}

fn parse_range(headers: &HeaderMap, meta: &FileMeta) -> RangeReq {
    let Some(range) = headers.get(header::RANGE).and_then(|v| v.to_str().ok()) else {
        return RangeReq::None;
    };
    if let Some(if_range) = headers.get(header::IF_RANGE).and_then(|v| v.to_str().ok()) {
        let etag_ok = if_range == meta.etag.to_str().unwrap_or("");
        let date_ok = meta
            .last_modified
            .as_ref()
            .is_some_and(|lm| lm.to_str().ok() == Some(if_range));
        if !etag_ok && !date_ok {
            return RangeReq::None;
        }
    }
    let Some(spec) = range.trim().strip_prefix("bytes=") else {
        return RangeReq::None;
    };
    if spec.contains(',') {
        return RangeReq::None;
    }
    let Some((start, end)) = spec.trim().split_once('-') else {
        return RangeReq::None;
    };
    let len = meta.len;
    let (start, end) = match (start.trim(), end.trim()) {
        ("", "") => return RangeReq::None,
        ("", suffix) => match suffix.parse::<u64>() {
            Ok(0) => return RangeReq::Unsatisfiable,
            Ok(n) => (len.saturating_sub(n), len.saturating_sub(1)),
            Err(_) => return RangeReq::None,
        },
        (s, e) => {
            let Ok(s) = s.parse::<u64>() else {
                return RangeReq::None;
            };
            let e = if e.is_empty() {
                len.saturating_sub(1)
            } else {
                match e.parse::<u64>() {
                    Ok(e) => e.min(len.saturating_sub(1)),
                    Err(_) => return RangeReq::None,
                }
            };
            (s, e)
        }
    };
    if len == 0 || start >= len || start > end {
        return RangeReq::Unsatisfiable;
    }
    RangeReq::Satisfiable(start, end)
}

fn unsatisfiable(meta: &FileMeta) -> Response<Body> {
    let mut resp = Response::new(Body::empty());
    *resp.status_mut() = StatusCode::RANGE_NOT_SATISFIABLE;
    if let Ok(v) = HeaderValue::from_str(&format!("bytes */{}", meta.len)) {
        resp.headers_mut().insert(header::CONTENT_RANGE, v);
    }
    resp
}

fn partial(meta: &FileMeta, start: u64, end: u64, body: Body) -> Response<Body> {
    let mut resp = Response::new(body);
    *resp.status_mut() = StatusCode::PARTIAL_CONTENT;
    let h = resp.headers_mut();
    meta.apply(h);
    h.insert(header::CONTENT_LENGTH, HeaderValue::from(end - start + 1));
    if let Ok(v) = HeaderValue::from_str(&format!("bytes {}-{}/{}", start, end, meta.len)) {
        h.insert(header::CONTENT_RANGE, v);
    }
    h.insert(header::ACCEPT_RANGES, HeaderValue::from_static("bytes"));
    resp
}

fn variant_index(algo: CompressionAlgorithm) -> usize {
    match algo {
        CompressionAlgorithm::Zstd => 0,
        CompressionAlgorithm::Br => 1,
        CompressionAlgorithm::Gzip => 2,
    }
}

async fn respond_cached(entry: &CachedFile, req: &StaticRequest<'_>) -> Response<Body> {
    let meta = &entry.meta;
    if let Conditional::NotModified = check_conditional(meta, req.headers) {
        return not_modified(meta);
    }
    match parse_range(req.headers, meta) {
        RangeReq::Satisfiable(s, e) => {
            let body = Body::from(entry.body.slice(s as usize..=e as usize));
            let mut resp = partial(meta, s, e, body);
            mark_sendfile(&mut resp, &entry.file_path, s, e - s + 1);
            return resp;
        }
        RangeReq::Unsatisfiable => return unsatisfiable(meta),
        RangeReq::None => {}
    }

    if let Some((algo, cfg)) = req.encoding {
        if meta.len >= cfg.min_length && compress::type_matches(Some(&meta.content_type), cfg) {
            let variant = entry.variants[variant_index(algo)]
                .get_or_init(|| async {
                    let level = cfg.level;
                    let data = entry.body.clone();
                    compress::compress_bytes(data, algo, level)
                        .await
                        .ok()
                        .filter(|c| (c.len() as u64) < meta.len)
                })
                .await;
            if let Some(bytes) = variant {
                let mut resp = Response::new(Body::from(bytes.clone()));
                let h = resp.headers_mut();
                meta.apply(h);
                h.insert(header::CONTENT_LENGTH, HeaderValue::from(bytes.len()));
                compress::mark_encoded(h, algo);
                return resp;
            }
        }
    }

    let mut resp = Response::new(Body::from(entry.body.clone()));
    let h = resp.headers_mut();
    meta.apply(h);
    h.insert(header::CONTENT_LENGTH, HeaderValue::from(meta.len));
    h.insert(header::ACCEPT_RANGES, HeaderValue::from_static("bytes"));
    mark_sendfile(&mut resp, &entry.file_path, 0, meta.len);
    resp
}

fn mark_sendfile(resp: &mut Response<Body>, path: &Path, offset: u64, len: u64) {
    if len >= crate::http::server::SENDFILE_MIN {
        resp.extensions_mut().insert(crate::http::server::SendFile {
            path: path.to_path_buf(),
            offset,
            len,
        });
    }
}

async fn respond_large(
    path: PathBuf,
    meta: FileMeta,
    req: &StaticRequest<'_>,
) -> Result<Response<Body>, StatusCode> {
    if let Conditional::NotModified = check_conditional(&meta, req.headers) {
        return Ok(not_modified(&meta));
    }
    let mut file = std::fs::File::open(&path)
        .map(tokio::fs::File::from_std)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    match parse_range(req.headers, &meta) {
        RangeReq::Satisfiable(s, e) => {
            file.seek(SeekFrom::Start(s))
                .await
                .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
            let stream =
                tokio_util::io::ReaderStream::with_capacity(file.take(e - s + 1), 64 * 1024);
            Ok(partial(&meta, s, e, Body::from_stream(stream)))
        }
        RangeReq::Unsatisfiable => Ok(unsatisfiable(&meta)),
        RangeReq::None => {
            let stream = tokio_util::io::ReaderStream::with_capacity(file, 64 * 1024);
            let mut resp = Response::new(Body::from_stream(stream));
            let h = resp.headers_mut();
            meta.apply(h);
            h.insert(header::CONTENT_LENGTH, HeaderValue::from(meta.len));
            h.insert(header::ACCEPT_RANGES, HeaderValue::from_static("bytes"));
            mark_sendfile(&mut resp, &path, 0, meta.len);
            Ok(resp)
        }
    }
}

async fn still_same_file(entry: &Arc<CachedFile>) -> bool {
    let meta = std::fs::metadata(&entry.file_path);
    let same = match meta {
        Ok(m) => {
            m.is_file() && m.len() == entry.meta.len && m.modified().ok() == entry.meta.modified
        }
        Err(_) => false,
    };
    if same {
        entry
            .checked_at
            .store(crate::event::clock::mono_ms(), Ordering::Relaxed);
    }
    same
}

fn resolve_path(base_root: &Path, clean_path: &str, try_files: Option<&str>) -> Option<PathBuf> {
    let candidate = base_root.join(clean_path);
    if candidate.is_file() {
        return Some(candidate);
    } else if candidate.is_dir() {
        let index_html = candidate.join("index.html");
        if index_html.is_file() {
            return Some(index_html);
        }
    }

    for token in try_files?.split_whitespace() {
        let p = match token {
            "$uri" => base_root.join(clean_path),
            "$uri/" => base_root.join(clean_path).join("index.html"),

            other => base_root.join(other.trim_start_matches('/')),
        };
        if p.is_file() {
            return Some(p);
        }
    }
    None
}

fn load_file(
    root_dir: &str,
    path: &str,
    try_files: Option<&str>,
    max_cached_file: u64,
) -> Result<Loaded, StatusCode> {
    let clean_path = path.trim_start_matches('/');
    let base_root = PathBuf::from(root_dir);

    let file_path = resolve_path(&base_root, clean_path, try_files).ok_or(StatusCode::NOT_FOUND)?;

    if let (Ok(canon_file), Ok(canon_root)) = (file_path.canonicalize(), base_root.canonicalize()) {
        if !canon_file.starts_with(&canon_root) {
            warn!(file = %file_path.display(), root = %base_root.display(), "Directory traversal attempt blocked");
            return Err(StatusCode::FORBIDDEN);
        }
    }

    let metadata = std::fs::metadata(&file_path).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let meta = FileMeta::new(&file_path, &metadata);

    if meta.len > max_cached_file {
        return Ok(Loaded::Large(file_path, meta));
    }

    let contents = std::fs::read(&file_path).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let mut meta = meta;
    meta.len = contents.len() as u64;
    Ok(Loaded::Cacheable(Box::new(CachedFile {
        body: Bytes::from(contents),
        meta,
        file_path,
        checked_at: AtomicU64::new(crate::event::clock::mono_ms()),
        variants: Default::default(),
    })))
}

/// nginx `empty_gif`: the classic 43-byte transparent 1×1 GIF.
pub const EMPTY_GIF: &[u8] = &[
    0x47, 0x49, 0x46, 0x38, 0x39, 0x61, 0x01, 0x00, 0x01, 0x00, 0x80, 0x01, 0x00, 0x00, 0x00, 0x00,
    0xff, 0xff, 0xff, 0x21, 0xf9, 0x04, 0x01, 0x00, 0x00, 0x00, 0x00, 0x2c, 0x00, 0x00, 0x00, 0x00,
    0x01, 0x00, 0x01, 0x00, 0x00, 0x02, 0x02, 0x44, 0x01, 0x00, 0x3b,
];

pub fn empty_gif() -> Response<Body> {
    let mut r = Response::new(Body::from(EMPTY_GIF));
    r.headers_mut()
        .insert(header::CONTENT_TYPE, HeaderValue::from_static("image/gif"));
    r
}

/// nginx `random_index`: for `dir/` returns `dir/<random visible file>`.
pub fn random_index(root_dir: &str, path: &str) -> Option<String> {
    if !path.ends_with('/') {
        return None;
    }
    let base = PathBuf::from(root_dir).canonicalize().ok()?;
    let dir = base
        .join(path.trim_start_matches('/'))
        .canonicalize()
        .ok()?;
    if !dir.starts_with(&base) {
        return None;
    }
    let files: Vec<String> = std::fs::read_dir(&dir)
        .ok()?
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_ok_and(|t| t.is_file()))
        .filter_map(|e| e.file_name().into_string().ok())
        .filter(|n| !n.starts_with('.'))
        .collect();
    if files.is_empty() {
        return None;
    }
    let pick = (crate::event::clock::mono_ms() as usize ^ uuid::Uuid::new_v4().as_u128() as usize)
        % files.len();
    Some(format!("{path}{}", files[pick]))
}

/// FLV header sent before data served from an offset (nginx `ngx_http_flv_module`).
const FLV_HEADER: &[u8] = b"FLV\x01\x05\0\0\0\x09\0\0\0\0";

/// nginx `flv`: `file.flv?start=N` streams from byte N with an FLV header in
/// front. `None` = not applicable (no/zero start), serve the file normally.
pub async fn flv(root_dir: &str, path: &str, query: Option<&str>) -> Option<Response<Body>> {
    if !path.ends_with(".flv") {
        return None;
    }
    let start: u64 = query?
        .split('&')
        .find_map(|kv| kv.strip_prefix("start="))?
        .parse()
        .ok()?;
    if start == 0 {
        return None;
    }
    let base = PathBuf::from(root_dir).canonicalize().ok()?;
    let file = base
        .join(path.trim_start_matches('/'))
        .canonicalize()
        .ok()?;
    if !file.starts_with(&base) {
        return None;
    }
    let mut f = tokio::fs::File::open(&file).await.ok()?;
    let len = f.metadata().await.ok()?.len();
    if start >= len {
        return None;
    }
    f.seek(SeekFrom::Start(start)).await.ok()?;
    let stream = futures_util::StreamExt::chain(
        futures_util::stream::once(async {
            Ok::<_, std::io::Error>(Bytes::from_static(FLV_HEADER))
        }),
        tokio_util::io::ReaderStream::new(f),
    );
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "video/x-flv")
        .header(
            header::CONTENT_LENGTH,
            len - start + FLV_HEADER.len() as u64,
        )
        .body(Body::from_stream(stream))
        .ok()
}

/// nginx `ngx_http_autoindex_module`: HTML listing of `root_dir/path` when it is a
/// directory. Returns `None` if it isn't one (or escapes the root), so the caller
/// answers 404. Directories that end without `/` get a 301 to `uri/`, like nginx.
pub fn autoindex(root_dir: &str, path: &str, uri: &str) -> Option<Response<Body>> {
    let base_root = PathBuf::from(root_dir).canonicalize().ok()?;
    let dir = base_root
        .join(path.trim_start_matches('/'))
        .canonicalize()
        .ok()?;
    if !dir.starts_with(&base_root) || !dir.is_dir() {
        return None;
    }
    if !uri.ends_with('/') {
        return Response::builder()
            .status(StatusCode::MOVED_PERMANENTLY)
            .header(header::LOCATION, format!("{uri}/"))
            .body(Body::empty())
            .ok();
    }

    let mut entries: Vec<(String, bool, u64, String)> = std::fs::read_dir(&dir)
        .ok()?
        .filter_map(|e| e.ok())
        .filter_map(|e| {
            let name = e.file_name().into_string().ok()?;
            if name.starts_with('.') {
                return None;
            }
            let md = e.metadata().ok()?;
            let mtime = md
                .modified()
                .ok()
                .map(|m| {
                    let dt: chrono::DateTime<chrono::Utc> = m.into();
                    dt.format("%d-%b-%Y %H:%M").to_string()
                })
                .unwrap_or_default();
            Some((name, md.is_dir(), md.len(), mtime))
        })
        .collect();
    // Directories first, then by name: same order as nginx.
    entries.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));

    let title = html_escape(uri);
    let mut html = format!(
        "<html>\r\n<head><title>Index of {title}</title></head>\r\n<body>\r\n\
         <h1>Index of {title}</h1><hr><pre><a href=\"../\">../</a>\r\n"
    );
    for (name, is_dir, len, mtime) in entries {
        let shown = if is_dir {
            format!("{name}/")
        } else {
            name.clone()
        };
        let href = percent_encode_segment(&shown);
        let size = if is_dir {
            "-".to_string()
        } else {
            len.to_string()
        };
        let pad = 51usize.saturating_sub(shown.chars().count()).max(1);
        html.push_str(&format!(
            "<a href=\"{href}\">{}</a>{}{mtime} {size:>19}\r\n",
            html_escape(&shown),
            " ".repeat(pad)
        ));
    }
    html.push_str("</pre><hr></body>\r\n</html>\r\n");

    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "text/html; charset=utf-8")
        .body(Body::from(html))
        .ok()
}

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn percent_encode_segment(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b"-._~/".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use http_body_util::BodyExt;

    fn dir_with(name: &str, contents: &str) -> tempdir::Dir {
        tempdir::Dir::new(name, contents)
    }

    mod tempdir {
        pub struct Dir(pub std::path::PathBuf);
        impl Dir {
            pub fn new(file: &str, contents: &str) -> Self {
                let p = std::env::temp_dir().join(format!(
                    "ferrix-static-{}-{}",
                    std::process::id(),
                    uuid::Uuid::new_v4()
                ));
                std::fs::create_dir_all(&p).unwrap();
                std::fs::write(p.join(file), contents).unwrap();
                Dir(p)
            }
            pub fn root(&self) -> &str {
                self.0.to_str().unwrap()
            }
        }
        impl Drop for Dir {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
    }

    async fn get(root: &str, path: &str, headers: HeaderMap) -> Response<Body> {
        let req = StaticRequest {
            headers: &headers,
            encoding: None,
            cache: &StaticCacheConfig::default(),
        };
        StaticFileServer::serve_file(root, path, None, &req)
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn etag_revalidation_returns_304() {
        let d = dir_with("a.txt", "hello world");
        let first = get(d.root(), "/a.txt", HeaderMap::new()).await;
        let etag = first.headers()[header::ETAG].clone();
        assert!(first.headers().contains_key(header::LAST_MODIFIED));

        let mut h = HeaderMap::new();
        h.insert(header::IF_NONE_MATCH, etag);
        let second = get(d.root(), "/a.txt", h).await;
        assert_eq!(second.status(), StatusCode::NOT_MODIFIED);
    }

    #[tokio::test]
    async fn files_above_max_file_bytes_are_not_cached() {
        let d = dir_with("big.txt", &"x".repeat(4096));
        let headers = HeaderMap::new();
        let limits = StaticCacheConfig {
            max_file_bytes: 1024,
            ..Default::default()
        };
        let req = StaticRequest {
            headers: &headers,
            encoding: None,
            cache: &limits,
        };
        let r = StaticFileServer::serve_file(d.root(), "/big.txt", None, &req)
            .await
            .unwrap();
        assert_eq!(r.status(), StatusCode::OK);
        let key = format!("{}\0/big.txt\0", d.root());
        assert!(!CACHE.read().unwrap().entries.contains_key(&key));
    }

    #[tokio::test]
    async fn byte_ranges() {
        let d = dir_with("r.txt", "0123456789");
        let mut h = HeaderMap::new();
        h.insert(header::RANGE, HeaderValue::from_static("bytes=2-5"));
        let r = get(d.root(), "/r.txt", h).await;
        assert_eq!(r.status(), StatusCode::PARTIAL_CONTENT);
        assert_eq!(r.headers()[header::CONTENT_RANGE], "bytes 2-5/10");
        assert_eq!(r.into_body().collect().await.unwrap().to_bytes(), "2345");

        let mut h = HeaderMap::new();
        h.insert(header::RANGE, HeaderValue::from_static("bytes=-3"));
        let r = get(d.root(), "/r.txt", h).await;
        assert_eq!(r.into_body().collect().await.unwrap().to_bytes(), "789");

        let mut h = HeaderMap::new();
        h.insert(header::RANGE, HeaderValue::from_static("bytes=50-"));
        let r = get(d.root(), "/r.txt", h).await;
        assert_eq!(r.status(), StatusCode::RANGE_NOT_SATISFIABLE);
    }

    #[tokio::test]
    async fn precompressed_variant_is_served() {
        let text = "compress me ".repeat(500);
        let d = dir_with("c.txt", &text);
        let cfg = CompressionConfig {
            enabled: true,
            ..Default::default()
        };
        let headers = HeaderMap::new();
        let req = StaticRequest {
            headers: &headers,
            encoding: Some((CompressionAlgorithm::Gzip, &cfg)),
            cache: &StaticCacheConfig::default(),
        };
        let r = StaticFileServer::serve_file(d.root(), "/c.txt", None, &req)
            .await
            .unwrap();
        assert_eq!(r.headers()[header::CONTENT_ENCODING], "gzip");
        let len: usize = r.headers()[header::CONTENT_LENGTH]
            .to_str()
            .unwrap()
            .parse()
            .unwrap();
        assert!(len < text.len());
        assert_eq!(r.into_body().collect().await.unwrap().to_bytes().len(), len);
    }

    #[test]
    fn autoindex_lists_directory() {
        let d = dir_with("a&b.txt", "hi");
        std::fs::create_dir(d.0.join("sub")).unwrap();
        let r = autoindex(d.root(), "/", "/").unwrap();
        assert_eq!(r.status(), StatusCode::OK);
        let r = autoindex(d.root(), "/sub", "/sub").unwrap();
        assert_eq!(r.status(), StatusCode::MOVED_PERMANENTLY);
        assert!(autoindex(d.root(), "/a&b.txt", "/a&b.txt").is_none());
        assert!(autoindex(d.root(), "/../", "/../").is_none());
    }

    #[tokio::test]
    async fn autoindex_html_escapes_and_sorts_dirs_first() {
        let d = dir_with("a&b.txt", "hi");
        std::fs::create_dir(d.0.join("zdir")).unwrap();
        let r = autoindex(d.root(), "/", "/").unwrap();
        let body = r.into_body().collect().await.unwrap().to_bytes();
        let html = std::str::from_utf8(&body).unwrap();
        assert!(html.contains("href=\"a%26b.txt\">a&amp;b.txt</a>"));
        assert!(html.find("zdir/").unwrap() < html.find("a&amp;b").unwrap());
    }

    #[tokio::test]
    async fn flv_start_prepends_header() {
        let d = dir_with("v.flv", "0123456789");
        assert!(flv(d.root(), "/v.flv", None).await.is_none());
        assert!(flv(d.root(), "/v.flv", Some("start=0")).await.is_none());
        let r = flv(d.root(), "/v.flv", Some("start=7")).await.unwrap();
        let b = r.into_body().collect().await.unwrap().to_bytes();
        assert_eq!(&b[..13], FLV_HEADER);
        assert_eq!(&b[13..], b"789");
    }

    #[test]
    fn random_index_picks_a_visible_file() {
        let d = dir_with("a.html", "a");
        std::fs::write(d.0.join(".hidden"), "x").unwrap();
        for _ in 0..10 {
            assert_eq!(random_index(d.root(), "/").as_deref(), Some("/a.html"));
        }
        assert!(random_index(d.root(), "/a.html").is_none());
        assert_eq!(EMPTY_GIF.len(), 43);
    }
}
