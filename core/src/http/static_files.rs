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

    // Compressed copies, built on first request for each encoding (gzip_static-style).
    // `None` inside means compression didn't make the file smaller.
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
        // Same shape as nginx: "<mtime hex>-<size hex>".
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

/// What the static handler needs from the request.
pub struct StaticRequest<'a> {
    pub headers: &'a HeaderMap,
    /// Encoding negotiated for this request when compression is enabled on the route.
    pub encoding: Option<(CompressionAlgorithm, &'a CompressionConfig)>,
    /// `http.static_cache` limits.
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

        // Read inline, like nginx: cacheable files are at most 1 MB and usually already
        // in the kernel's page cache, so this takes microseconds. Going through the
        // blocking pool instead spawned extra threads on a cold start (every connection
        // missing the cache at once), each keeping its own allocator heap alive.
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

// Single byte range only (what browsers and download managers send); multi-range
// requests are answered with the whole file, which RFC 9110 allows.
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

/// Lets the plain-HTTP server send large bodies straight from the page cache with
/// `sendfile(2)`; other connections ignore the hint and use the body.
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
    // A plain open is a fast syscall; going through tokio::fs would park the request on
    // the blocking thread pool for it.
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

// A synchronous stat, like nginx does: the inode is hot in the kernel cache, so this costs
// microseconds, while tokio::fs would hop to (and grow) the blocking thread pool.
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
}
