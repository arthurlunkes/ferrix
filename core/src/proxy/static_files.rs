use axum::body::{Body, Bytes};
use axum::http::{header, HeaderValue, Response, StatusCode};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, LazyLock, RwLock};
use std::time::{Duration, Instant, SystemTime};
use tracing::warn;

/// Arquivos até este tamanho ficam inteiros em memória. Acima disso, o arquivo é lido do
/// disco a cada requisição, como antes.
const MAX_CACHED_FILE_BYTES: u64 = 1024 * 1024;
/// Teto total do cache, para um diretório cheio de arquivos não esgotar a memória.
const MAX_CACHE_BYTES: u64 = 64 * 1024 * 1024;
/// Por quanto tempo uma entrada é servida sem reconsultar o disco — o mesmo papel do
/// `open_file_cache_valid` do nginx. Um arquivo editado aparece em no máximo 1 segundo.
const REVALIDATE_AFTER: Duration = Duration::from_secs(1);

/// Resolução de uma requisição já pronta para virar resposta: o arquivo escolhido (depois
/// de `try_files`), seu conteúdo e os cabeçalhos pré-montados.
struct CachedFile {
    body: Bytes,
    content_type: HeaderValue,
    content_length: HeaderValue,
    /// `mtime` e tamanho do arquivo quando foi lido; mudou qualquer um, relê.
    modified: Option<SystemTime>,
    len: u64,
    file_path: PathBuf,
    /// Milissegundos desde `EPOCH` da última vez que o disco foi consultado.
    checked_at: AtomicU64,
}

#[derive(Default)]
struct FileCache {
    entries: HashMap<String, Arc<CachedFile>>,
    total_bytes: u64,
}

/// Chave: raiz + caminho pedido + `try_files`. A mesma URL pode cair em arquivos diferentes
/// em rotas diferentes, então a raiz e o `try_files` entram na chave.
static EPOCH: LazyLock<Instant> = LazyLock::new(Instant::now);

fn now_ms() -> u64 {
    EPOCH.elapsed().as_millis() as u64
}

static CACHE: LazyLock<RwLock<FileCache>> = LazyLock::new(|| RwLock::new(FileCache::default()));

pub struct StaticFileServer;

impl StaticFileServer {
    pub async fn serve_file(
        root_dir: &str,
        path: &str,
        try_files: Option<&str>,
    ) -> Result<Response<Body>, StatusCode> {
        let key = format!("{}\0{}\0{}", root_dir, path, try_files.unwrap_or(""));

        let hit = CACHE.read().ok().and_then(|c| c.entries.get(&key).cloned());
        if let Some(entry) = hit {
            let age = now_ms().saturating_sub(entry.checked_at.load(Ordering::Relaxed));
            let fresh = age < REVALIDATE_AFTER.as_millis() as u64;
            if fresh || still_same_file(&entry).await {
                return Ok(build_response(&entry));
            }
            if let Ok(mut c) = CACHE.write() {
                if let Some(old) = c.entries.remove(&key) {
                    c.total_bytes -= old.len;
                }
            }
        }

        let root = root_dir.to_string();
        let req_path = path.to_string();
        let tf = try_files.map(|s| s.to_string());
        // Toda a resolução usa chamadas de sistema bloqueantes (stat, canonicalize, read);
        // fora do runtime, para não travar as outras conexões da mesma thread.
        let loaded = tokio::task::spawn_blocking(move || load_file(&root, &req_path, tf.as_deref()))
            .await
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)??;

        match loaded {
            Loaded::Cacheable(entry) => {
                let entry = Arc::new(entry);
                if let Ok(mut c) = CACHE.write() {
                    if c.total_bytes + entry.len <= MAX_CACHE_BYTES {
                        c.total_bytes += entry.len;
                        if let Some(old) = c.entries.insert(key, entry.clone()) {
                            c.total_bytes -= old.len;
                        }
                    }
                }
                Ok(build_response(&entry))
            }
            Loaded::Large(resp) => Ok(resp),
        }
    }
}

enum Loaded {
    Cacheable(CachedFile),
    Large(Response<Body>),
}

fn build_response(entry: &CachedFile) -> Response<Body> {
    let mut resp = Response::new(Body::from(entry.body.clone()));
    let h = resp.headers_mut();
    h.insert(header::CONTENT_TYPE, entry.content_type.clone());
    h.insert(header::CONTENT_LENGTH, entry.content_length.clone());
    h.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("public, max-age=3600"),
    );
    resp
}

/// Reconsulta o disco depois de `REVALIDATE_AFTER`. Requisições simultâneas na virada
/// podem fazer o `stat` em paralelo; é raro e barato, então não vale um lock.
async fn still_same_file(entry: &Arc<CachedFile>) -> bool {
    let meta = tokio::fs::metadata(&entry.file_path).await;
    let same = match meta {
        Ok(m) => m.is_file() && m.len() == entry.len && m.modified().ok() == entry.modified,
        Err(_) => false,
    };
    if same {
        entry.checked_at.store(now_ms(), Ordering::Relaxed);
    }
    same
}

fn resolve_path(base_root: &Path, clean_path: &str, try_files: Option<&str>) -> Option<PathBuf> {
    // Try direct file first
    let candidate = base_root.join(clean_path);
    if candidate.is_file() {
        return Some(candidate);
    } else if candidate.is_dir() {
        let index_html = candidate.join("index.html");
        if index_html.is_file() {
            return Some(index_html);
        }
    }

    // Try_files fallback handling (e.g. "$uri $uri/ /index.html")
    for token in try_files?.split_whitespace() {
        let p = match token {
            "$uri" => base_root.join(clean_path),
            "$uri/" => base_root.join(clean_path).join("index.html"),
            // Explicit fallback path like /index.html
            other => base_root.join(other.trim_start_matches('/')),
        };
        if p.is_file() {
            return Some(p);
        }
    }
    None
}

fn load_file(root_dir: &str, path: &str, try_files: Option<&str>) -> Result<Loaded, StatusCode> {
    let clean_path = path.trim_start_matches('/');
    let base_root = PathBuf::from(root_dir);

    let file_path = resolve_path(&base_root, clean_path, try_files).ok_or(StatusCode::NOT_FOUND)?;

    // Double check canonical path stays within root
    if let (Ok(canon_file), Ok(canon_root)) = (file_path.canonicalize(), base_root.canonicalize()) {
        if !canon_file.starts_with(&canon_root) {
            warn!(file = %file_path.display(), root = %base_root.display(), "Directory traversal attempt blocked");
            return Err(StatusCode::FORBIDDEN);
        }
    }

    let metadata = std::fs::metadata(&file_path).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let len = metadata.len();
    let mime = mime_guess::from_path(&file_path).first_or_octet_stream();
    let content_type = HeaderValue::from_str(mime.as_ref())
        .unwrap_or(HeaderValue::from_static("application/octet-stream"));

    if len > MAX_CACHED_FILE_BYTES {
        // Grande demais para a memória: segue em streaming direto do disco.
        let file = std::fs::File::open(&file_path).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
        let stream = tokio_util::io::ReaderStream::new(tokio::fs::File::from_std(file));
        let mut resp = Response::new(Body::from_stream(stream));
        let h = resp.headers_mut();
        h.insert(header::CONTENT_TYPE, content_type);
        h.insert(header::CONTENT_LENGTH, HeaderValue::from(len));
        h.insert(
            header::CACHE_CONTROL,
            HeaderValue::from_static("public, max-age=3600"),
        );
        return Ok(Loaded::Large(resp));
    }

    let contents = std::fs::read(&file_path).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let len = contents.len() as u64;
    Ok(Loaded::Cacheable(CachedFile {
        body: Bytes::from(contents),
        content_type,
        content_length: HeaderValue::from(len),
        modified: metadata.modified().ok(),
        len,
        file_path,
        checked_at: AtomicU64::new(now_ms()),
    }))
}
