use axum::body::Bytes;
use axum::http::HeaderValue;
use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use std::time::Duration;

use crate::http::cache_store::{now_ms, StoredEntry};

pub const MAX_CACHEABLE_BODY_BYTES: u64 = 2 * 1024 * 1024;

const MAX_ENTRIES: usize = 10_000;

#[derive(Debug, Clone)]
pub struct CachedResponse {
    pub status: u16,

    pub headers: Vec<(String, String)>,
    pub body: Bytes,
    expires_ms: u64,
    stale_until_ms: u64,
    inserted_seq: u64,
}

impl CachedResponse {
    pub fn is_fresh(&self) -> bool {
        self.expires_ms > now_ms()
    }

    pub fn from_stored(e: StoredEntry) -> Self {
        Self {
            status: e.status,
            headers: e.headers,
            body: e.body,
            expires_ms: e.expires_ms,
            stale_until_ms: e.stale_until_ms,
            inserted_seq: 0,
        }
    }

    pub fn to_stored(&self) -> StoredEntry {
        StoredEntry {
            status: self.status,
            headers: self.headers.clone(),
            body: self.body.clone(),
            expires_ms: self.expires_ms,
            stale_until_ms: self.stale_until_ms,
        }
    }

    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }
}

struct Entry {
    response: CachedResponse,
}

pub struct ResponseCache {
    entries: RwLock<HashMap<String, Entry>>,
    seq: std::sync::atomic::AtomicU64,
    locks: std::sync::Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
    uses: std::sync::Mutex<HashMap<String, u32>>,
}

impl ResponseCache {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            entries: RwLock::new(HashMap::new()),
            seq: std::sync::atomic::AtomicU64::new(0),
            locks: std::sync::Mutex::new(HashMap::new()),
            uses: std::sync::Mutex::new(HashMap::new()),
        })
    }

    pub fn get(&self, key: &str) -> Option<CachedResponse> {
        self.get_any(key).filter(|r| r.is_fresh())
    }

    pub fn note_use(&self, key: &str, min_uses: u32) -> bool {
        if min_uses <= 1 {
            return true;
        }
        let Ok(mut m) = self.uses.lock() else {
            return true;
        };
        if m.len() > 100_000 {
            m.clear();
        }
        let n = m.entry(key.to_string()).or_insert(0);
        *n += 1;
        if *n >= min_uses {
            m.remove(key);
            return true;
        }
        false
    }

    pub fn get_any(&self, key: &str) -> Option<CachedResponse> {
        let now = now_ms();
        {
            let entries = self.entries.read().ok()?;
            let entry = entries.get(key)?;
            if entry.response.stale_until_ms > now {
                return Some(entry.response.clone());
            }
        }

        if let Ok(mut entries) = self.entries.write() {
            entries.remove(key);
        }
        None
    }

    pub fn refresh(&self, key: &str, valid_for: Duration, keep_stale: Duration) {
        if let Ok(mut entries) = self.entries.write() {
            if let Some(e) = entries.get_mut(key) {
                e.response.expires_ms = now_ms() + valid_for.as_millis() as u64;
                e.response.stale_until_ms = e.response.expires_ms + keep_stale.as_millis() as u64;
            }
        }
    }

    pub fn lock_for(&self, key: &str) -> Arc<tokio::sync::Mutex<()>> {
        let mut locks = match self.locks.lock() {
            Ok(l) => l,
            Err(p) => p.into_inner(),
        };
        if locks.len() > 1024 {
            locks.retain(|_, l| Arc::strong_count(l) > 1);
        }
        locks
            .entry(key.to_string())
            .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
            .clone()
    }

    pub fn put(
        &self,
        key: String,
        status: u16,
        headers: Vec<(String, String)>,
        body: Bytes,
        valid_for: Duration,
    ) {
        self.put_keeping(key, status, headers, body, valid_for, Duration::ZERO)
    }

    pub fn put_keeping(
        &self,
        key: String,
        status: u16,
        headers: Vec<(String, String)>,
        body: Bytes,
        valid_for: Duration,
        keep_stale: Duration,
    ) {
        let expires_ms = now_ms() + valid_for.as_millis() as u64;
        self.insert(
            key,
            CachedResponse {
                status,
                headers,
                body,
                expires_ms,
                stale_until_ms: expires_ms + keep_stale.as_millis() as u64,
                inserted_seq: 0,
            },
        );
    }

    pub fn insert(&self, key: String, mut response: CachedResponse) {
        response.inserted_seq = self.seq.fetch_add(1, std::sync::atomic::Ordering::Relaxed);

        let Ok(mut entries) = self.entries.write() else {
            return;
        };
        if entries.len() >= MAX_ENTRIES && !entries.contains_key(&key) {
            if let Some(oldest_key) = entries
                .iter()
                .min_by_key(|(_, e)| e.response.inserted_seq)
                .map(|(k, _)| k.clone())
            {
                entries.remove(&oldest_key);
            }
        }
        entries.insert(key, Entry { response });
    }

    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.entries.read().map(|e| e.len()).unwrap_or(0)
    }

    #[cfg(test)]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

pub fn cache_key(method: &str, host: &str, path_and_query: &str) -> String {
    format!("{}:{}:{}", method, host, path_and_query)
}

pub fn cacheable_headers(headers: &axum::http::HeaderMap) -> Vec<(String, String)> {
    headers
        .iter()
        .filter(|(name, _)| {
            let n = name.as_str().to_ascii_lowercase();
            !matches!(
                n.as_str(),
                "connection"
                    | "keep-alive"
                    | "proxy-authenticate"
                    | "proxy-authorization"
                    | "te"
                    | "trailer"
                    | "transfer-encoding"
                    | "upgrade"
                    | "set-cookie"
                    | "content-length"
            )
        })
        .filter_map(|(name, value)| {
            value
                .to_str()
                .ok()
                .map(|v| (name.as_str().to_string(), v.to_string()))
        })
        .collect()
}

pub fn has_set_cookie(headers: &axum::http::HeaderMap) -> bool {
    headers.contains_key(axum::http::header::SET_COOKIE)
}

pub fn upstream_forbids_cache(headers: &axum::http::HeaderMap) -> bool {
    headers
        .get(axum::http::header::CACHE_CONTROL)
        .and_then(|v| v.to_str().ok())
        .map(|v| {
            let lower = v.to_ascii_lowercase();
            lower.contains("no-store") || lower.contains("no-cache") || lower.contains("private")
        })
        .unwrap_or(false)
}

pub fn header_value(s: &str) -> Option<HeaderValue> {
    HeaderValue::from_str(s).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn miss_on_empty_cache() {
        let c = ResponseCache::new();
        assert!(c.get("k").is_none());
    }

    #[test]
    fn put_then_get_is_a_hit() {
        let c = ResponseCache::new();
        c.put(
            "k".to_string(),
            200,
            vec![("content-type".to_string(), "text/plain".to_string())],
            Bytes::from_static(b"hello"),
            Duration::from_secs(60),
        );
        let hit = c.get("k").expect("deveria ter entrada");
        assert_eq!(hit.status, 200);
        assert_eq!(&hit.body[..], b"hello");
    }

    #[test]
    fn expired_entry_is_a_miss_and_gets_evicted() {
        let c = ResponseCache::new();
        c.put(
            "k".to_string(),
            200,
            vec![],
            Bytes::from_static(b"x"),
            Duration::from_millis(0),
        );
        std::thread::sleep(Duration::from_millis(5));
        assert!(c.get("k").is_none());
        assert_eq!(c.len(), 0);
    }

    #[test]
    fn cache_is_keyed_by_method_host_and_path() {
        let c = ResponseCache::new();
        c.put(
            cache_key("GET", "a.com", "/x"),
            200,
            vec![],
            Bytes::from_static(b"a"),
            Duration::from_secs(60),
        );
        assert!(c.get(&cache_key("GET", "a.com", "/x")).is_some());
        assert!(c.get(&cache_key("HEAD", "a.com", "/x")).is_none());
        assert!(c.get(&cache_key("GET", "b.com", "/x")).is_none());
        assert!(c.get(&cache_key("GET", "a.com", "/y")).is_none());
    }

    #[test]
    fn full_cache_evicts_the_oldest_insertion_to_make_room() {
        let c = ResponseCache::new();
        for i in 0..MAX_ENTRIES {
            c.put(
                format!("k{}", i),
                200,
                vec![],
                Bytes::from_static(b"x"),
                Duration::from_secs(60),
            );
        }
        assert_eq!(c.len(), MAX_ENTRIES);
        c.put(
            "new".to_string(),
            200,
            vec![],
            Bytes::from_static(b"x"),
            Duration::from_secs(60),
        );
        assert_eq!(c.len(), MAX_ENTRIES);
        assert!(c.get("k0").is_none());
        assert!(c.get("new").is_some());
    }

    #[test]
    fn set_cookie_and_hop_by_hop_headers_are_stripped() {
        let mut headers = axum::http::HeaderMap::new();
        headers.insert(axum::http::header::SET_COOKIE, "sess=abc".parse().unwrap());
        headers.insert(
            axum::http::header::CONTENT_TYPE,
            "text/html".parse().unwrap(),
        );
        headers.insert(
            axum::http::header::CONNECTION,
            "keep-alive".parse().unwrap(),
        );

        let kept = cacheable_headers(&headers);
        assert!(kept.iter().any(|(k, _)| k == "content-type"));
        assert!(!kept.iter().any(|(k, _)| k == "set-cookie"));
        assert!(!kept.iter().any(|(k, _)| k == "connection"));

        assert!(has_set_cookie(&headers));
    }
}

use crate::config::{CacheConfig, VirtualHost};
use crate::http::ProxyEngine;
use axum::body::Body;
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};

impl ProxyEngine {
    pub(super) async fn cache_lookup(
        &self,
        store: &crate::config::CacheStoreConfig,
        key: &str,
    ) -> Option<CachedResponse> {
        match store.kind {
            crate::config::CacheStoreKind::Memory => self.response_cache.get_any(key),
            _ => crate::http::cache_store::Store { cfg: store }
                .get(key)
                .await
                .map(CachedResponse::from_stored),
        }
    }

    pub(super) async fn cache_insert(
        &self,
        store: &crate::config::CacheStoreConfig,
        key: String,
        entry: CachedResponse,
    ) {
        match store.kind {
            crate::config::CacheStoreKind::Memory => self.response_cache.insert(key, entry),
            _ => {
                crate::http::cache_store::Store { cfg: store }
                    .put(&key, &entry.to_stored())
                    .await
            }
        }
    }

    pub(super) async fn store_if_cacheable(
        &self,
        store: &crate::config::CacheStoreConfig,
        key: String,
        cfg: &CacheConfig,
        resp: Response,
    ) -> Response {
        let max_body = cfg
            .max_body_bytes
            .unwrap_or(crate::http::cache::MAX_CACHEABLE_BODY_BYTES);
        let valid = validity(cfg, resp.status().as_u16(), resp.headers());
        let cacheable = valid.is_some()
            && resp
                .headers()
                .get(header::CONTENT_LENGTH)
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.parse::<u64>().ok())
                .is_some_and(|len| len <= max_body)
            && self.response_cache.note_use(&key, cfg.min_uses);

        if !cacheable {
            let mut r = resp;
            if let Some(v) = crate::http::cache::header_value("MISS") {
                r.headers_mut().insert("x-cache-status", v);
            }
            return r;
        }

        let headers = crate::http::cache::cacheable_headers(resp.headers());
        let (parts, body) = resp.into_parts();
        match axum::body::to_bytes(body, max_body as usize).await {
            Ok(bytes) => {
                let valid = valid.unwrap_or_default();
                let expires_ms = now_ms() + valid.as_millis() as u64;
                let entry = CachedResponse {
                    status: parts.status.as_u16(),
                    headers,
                    body: bytes.clone(),
                    expires_ms,
                    stale_until_ms: expires_ms + keep_stale(cfg).as_millis() as u64,
                    inserted_seq: 0,
                };
                self.cache_insert(store, key, entry).await;
                let mut r = Response::from_parts(parts, Body::from(bytes));
                if let Some(v) = crate::http::cache::header_value("MISS") {
                    r.headers_mut().insert("x-cache-status", v);
                }
                r
            }
            Err(_) => (
                StatusCode::BAD_GATEWAY,
                "502 Bad Gateway - Ferrix Proxy Error: failed to read response body from upstream",
            )
                .into_response(),
        }
    }
}

pub(super) fn validity(
    cfg: &CacheConfig,
    status: u16,
    headers: &axum::http::HeaderMap,
) -> Option<Duration> {
    let ignores = |h: &str| cfg.ignore_headers.iter().any(|i| i.eq_ignore_ascii_case(h));
    if !ignores("Set-Cookie") && crate::http::cache::has_set_cookie(headers) {
        return None;
    }
    if headers
        .get(header::VARY)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.trim() == "*")
        && !ignores("Vary")
    {
        return None;
    }
    if !ignores("X-Accel-Expires") {
        if let Some(n) = headers
            .get("x-accel-expires")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.trim().parse::<u64>().ok())
        {
            return (n > 0).then(|| Duration::from_secs(n));
        }
    }
    if !ignores("Cache-Control") {
        if let Some(cc) = headers
            .get(header::CACHE_CONTROL)
            .and_then(|v| v.to_str().ok())
        {
            let lower = cc.to_ascii_lowercase();
            if lower.contains("no-store") || lower.contains("no-cache") || lower.contains("private")
            {
                return None;
            }
            let age = |name: &str| {
                lower.split(',').map(str::trim).find_map(|d| {
                    d.strip_prefix(name)?
                        .strip_prefix('=')?
                        .trim()
                        .parse::<u64>()
                        .ok()
                })
            };
            if let Some(n) = age("s-maxage").or_else(|| age("max-age")) {
                return (n > 0).then(|| Duration::from_secs(n));
            }
        }
    }
    if !ignores("Expires") {
        if let Some(exp) = headers
            .get(header::EXPIRES)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| chrono::DateTime::parse_from_rfc2822(v).ok())
        {
            let secs = exp.timestamp() - chrono::Utc::now().timestamp();
            return (secs > 0).then(|| Duration::from_secs(secs as u64));
        }
    }
    if !cfg.valid_by_status.is_empty() {
        return cfg
            .valid_by_status
            .get(&status.to_string())
            .or_else(|| cfg.valid_by_status.get("any"))
            .map(|s| Duration::from_secs(*s));
    }
    matches!(status, 200 | 301 | 302).then(|| Duration::from_secs(cfg.valid_secs))
}

fn keep_stale(cfg: &CacheConfig) -> Duration {
    if cfg.use_stale.is_empty() && !cfg.revalidate {
        Duration::ZERO
    } else {
        Duration::from_secs(cfg.stale_keep_secs)
    }
}

pub(super) fn stale_allowed(cfg: &CacheConfig, status: u16) -> bool {
    cfg.use_stale.iter().any(|c| match c.as_str() {
        "error" => status == 502,
        "timeout" => status == 504,
        other => {
            other
                .strip_prefix("http_")
                .and_then(|n| n.parse::<u16>().ok())
                == Some(status)
        }
    })
}

fn tag(mut r: Response, status: &str) -> Response {
    if let Some(v) = crate::http::cache::header_value(status) {
        r.headers_mut().insert("x-cache-status", v);
    }
    r
}

impl ProxyEngine {
    #[allow(clippy::too_many_arguments)]
    pub(super) async fn serve_with_cache(
        &self,
        compiled: &crate::http::matcher::CompiledConfig,
        route: &crate::config::RouteRule,
        upstream_name: &str,
        upstream_cfg: &crate::config::UpstreamConfig,
        targets: Vec<&crate::config::UpstreamTarget>,
        is_tls: bool,
        client_ip: std::net::IpAddr,
        mut parts: axum::http::request::Parts,
        body: Body,
        key: String,
        cfg: &CacheConfig,
    ) -> Response {
        let cache = &self.response_cache;
        let store = &compiled.config.http.cache_store;
        let stale = match self.cache_lookup(store, &key).await {
            Some(e) if e.is_fresh() => return tag(build_cached_response(&e), "HIT"),
            other => other,
        };
        let updating = cfg.use_stale.iter().any(|c| c == "updating");

        let mut _guard = None;
        if cfg.lock || (updating && stale.is_some()) {
            let lock = cache.lock_for(&key);
            match (&stale, lock.clone().try_lock_owned()) {
                (_, Ok(g)) => _guard = Some(g),
                (Some(s), Err(_)) if updating => return tag(build_cached_response(s), "UPDATING"),
                (_, Err(_)) => {
                    let wait = Duration::from_millis(cfg.lock_timeout_ms.unwrap_or(5000));
                    if let Ok(g) = tokio::time::timeout(wait, lock.lock_owned()).await {
                        if let Some(e) = self
                            .cache_lookup(store, &key)
                            .await
                            .filter(|e| e.is_fresh())
                        {
                            return tag(build_cached_response(&e), "HIT");
                        }
                        _guard = Some(g);
                    }
                }
            }
        }

        if let (Some(s), true, Some(guard)) =
            (&stale, cfg.background_update && updating, _guard.take())
        {
            if let Some(target) = targets.first() {
                let base = target.url.clone();
                let path = super::build_target_path(&parts.uri, parts.uri.path(), route);
                let mut headers = parts.headers.clone();
                super::proxy::prepare_forwarded_headers(
                    &mut headers,
                    route,
                    client_ip,
                    is_tls,
                    super::proxy::proxy_host(upstream_cfg),
                    compiled.config.http.proxy_forwarded_headers,
                );
                let http = &compiled.config.http;
                let client = crate::http::upstream::UpstreamClient::new(
                    Duration::from_millis(http.proxy_connect_timeout_ms),
                    Duration::from_millis(http.proxy_read_timeout_ms),
                    upstream_cfg.verify_tls,
                )
                .with_tls(upstream_cfg.tls.as_ref());
                let cache = cache.clone();
                let key = key.clone();
                let cfg_owned = cfg.clone();
                let store_owned = store.clone();
                let keep = keep_stale(cfg);
                tokio::spawn(async move {
                    let _guard = guard;
                    let Ok(r) = client
                        .send(
                            &axum::http::Method::GET,
                            &base,
                            &path,
                            &headers,
                            crate::http::upstream::OutgoingBody::Empty,
                        )
                        .await
                    else {
                        return;
                    };
                    let Some(valid) = validity(&cfg_owned, r.status().as_u16(), r.headers()) else {
                        return;
                    };
                    let status = r.status().as_u16();
                    let headers = crate::http::cache::cacheable_headers(r.headers());
                    let max = cfg_owned
                        .max_body_bytes
                        .unwrap_or(crate::http::cache::MAX_CACHEABLE_BODY_BYTES);
                    if let Ok(b) = axum::body::to_bytes(r.into_body(), max as usize).await {
                        let expires_ms = now_ms() + valid.as_millis() as u64;
                        let entry = CachedResponse {
                            status,
                            headers,
                            body: b,
                            expires_ms,
                            stale_until_ms: expires_ms + keep.as_millis() as u64,
                            inserted_seq: 0,
                        };
                        match store_owned.kind {
                            crate::config::CacheStoreKind::Memory => cache.insert(key, entry),
                            _ => {
                                crate::http::cache_store::Store { cfg: &store_owned }
                                    .put(&key, &entry.to_stored())
                                    .await
                            }
                        }
                    }
                });
            }
            return tag(build_cached_response(s), "UPDATING");
        }

        let mut added_validators = false;
        if let (true, Some(s)) = (cfg.revalidate, &stale) {
            let client_asked = parts.headers.contains_key(header::IF_NONE_MATCH)
                || parts.headers.contains_key(header::IF_MODIFIED_SINCE);
            if !client_asked {
                if let Some(v) = s.header("etag").and_then(crate::http::cache::header_value) {
                    parts.headers.insert(header::IF_NONE_MATCH, v);
                    added_validators = true;
                }
                if let Some(v) = s
                    .header("last-modified")
                    .and_then(crate::http::cache::header_value)
                {
                    parts.headers.insert(header::IF_MODIFIED_SINCE, v);
                    added_validators = true;
                }
            }
        }

        let resp = self
            .proxy_http_request(
                compiled,
                route,
                upstream_name,
                upstream_cfg,
                targets,
                is_tls,
                client_ip,
                axum::extract::Request::from_parts(parts, body),
            )
            .await;

        if let Some(s) = &stale {
            if added_validators && resp.status() == StatusCode::NOT_MODIFIED {
                let valid = validity(cfg, s.status, resp.headers())
                    .unwrap_or(Duration::from_secs(cfg.valid_secs));
                let mut fresh = s.clone();
                fresh.expires_ms = now_ms() + valid.as_millis() as u64;
                fresh.stale_until_ms = fresh.expires_ms + keep_stale(cfg).as_millis() as u64;
                self.cache_insert(store, key.clone(), fresh).await;
                return tag(build_cached_response(s), "REVALIDATED");
            }
            if stale_allowed(cfg, resp.status().as_u16()) {
                return tag(build_cached_response(s), "STALE");
            }
        }
        Box::pin(self.store_if_cacheable(store, key, cfg, resp)).await
    }
}

pub(super) fn cache_bypass_matches(cfg: &CacheConfig, parts: &axum::http::request::Parts) -> bool {
    for name in &cfg.bypass_headers {
        if let Some(v) = parts.headers.get(name.as_str()) {
            if !v.is_empty() {
                return true;
            }
        }
    }

    if !cfg.bypass_cookies.is_empty() {
        if let Some(cookie_header) = parts
            .headers
            .get(header::COOKIE)
            .and_then(|v| v.to_str().ok())
        {
            for name in &cfg.bypass_cookies {
                let hit = cookie_header
                    .split(';')
                    .filter_map(|kv| kv.trim().split_once('='))
                    .any(|(k, _)| k == name);
                if hit {
                    return true;
                }
            }
        }
    }

    if !cfg.bypass_query_params.is_empty() {
        if let Some(query) = parts.uri.query() {
            for name in &cfg.bypass_query_params {
                let hit = query
                    .split('&')
                    .filter_map(|kv| kv.split_once('='))
                    .any(|(k, _)| k == name);
                if hit {
                    return true;
                }
            }
        }
    }

    false
}

pub(super) fn cache_key_for(vhost: &VirtualHost, parts: &axum::http::request::Parts) -> String {
    let host = parts
        .headers
        .get(header::HOST)
        .and_then(|v| v.to_str().ok())
        .unwrap_or(&vhost.name);
    let path_and_query = parts
        .uri
        .path_and_query()
        .map(|pq| pq.as_str())
        .unwrap_or("/");
    crate::http::cache::cache_key(parts.method.as_str(), host, path_and_query)
}

pub(super) fn build_cached_response(cached: &crate::http::cache::CachedResponse) -> Response {
    let mut builder = Response::builder().status(cached.status);
    for (k, v) in &cached.headers {
        builder = builder.header(k, v);
    }
    builder
        .body(Body::from(cached.body.clone()))
        .unwrap_or_else(|_| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                "500 Internal Server Error",
            )
                .into_response()
        })
}

#[cfg(test)]
mod proxy_cache_tests {
    use super::*;
    use crate::config::store::ConfigStore;
    use crate::health::HealthChecker;
    use crate::metrics::MetricsCollector;
    use axum::extract::Request;
    use std::sync::Arc;

    fn parts_full(uri: &str, headers: &[(&str, &str)]) -> axum::http::request::Parts {
        let mut builder = Request::builder().uri(uri);
        for (k, v) in headers {
            builder = builder.header(*k, *v);
        }
        builder.body(Body::empty()).unwrap().into_parts().0
    }

    #[test]
    fn no_bypass_configured_never_bypasses() {
        let cfg = crate::config::CacheConfig::default();
        let parts = parts_full("/x", &[("cookie", "a=1"), ("upgrade", "websocket")]);
        assert!(!cache_bypass_matches(&cfg, &parts));
    }

    #[test]
    fn a_non_empty_bypass_header_skips_the_cache() {
        let cfg = crate::config::CacheConfig {
            bypass_headers: vec!["Upgrade".to_string()],
            ..Default::default()
        };
        let with = parts_full("/x", &[("upgrade", "websocket")]);
        assert!(cache_bypass_matches(&cfg, &with));

        let without = parts_full("/x", &[]);
        assert!(!cache_bypass_matches(&cfg, &without));
    }

    #[test]
    fn a_matching_bypass_cookie_skips_the_cache() {
        let cfg = crate::config::CacheConfig {
            bypass_cookies: vec!["session".to_string()],
            ..Default::default()
        };
        let with = parts_full("/x", &[("cookie", "theme=dark; session=abc123")]);
        assert!(cache_bypass_matches(&cfg, &with));

        let without = parts_full("/x", &[("cookie", "theme=dark")]);
        assert!(!cache_bypass_matches(&cfg, &without));
    }

    #[test]
    fn a_matching_bypass_query_param_skips_the_cache() {
        let cfg = crate::config::CacheConfig {
            bypass_query_params: vec!["nocache".to_string()],
            ..Default::default()
        };
        let with = parts_full("/x?nocache=1", &[]);
        assert!(cache_bypass_matches(&cfg, &with));

        let without = parts_full("/x?other=1", &[]);
        assert!(!cache_bypass_matches(&cfg, &without));
    }

    #[test]
    fn cache_key_distinguishes_method_host_and_path() {
        let vhost = VirtualHost {
            id: "v".into(),
            name: "fallback".into(),
            server_names: vec!["*".into()],
            listen_port: 80,
            headers: std::collections::HashMap::new(),
            headers_always: Default::default(),
            error_redirects: Vec::new(),
            recursive_error_pages: false,
            rate_limit: None,
            limit_conn: None,
            allowed_ips: Vec::new(),
            denied_ips: Vec::new(),
            basic_auth: None,
            satisfy: crate::config::Satisfy::All,
            tls: None,
            routes: Vec::new(),
            error_pages: Default::default(),
            conditions: Vec::new(),
            rewrites: Vec::new(),
        };
        let get_x = parts_full("/x", &[("host", "a.com")]);
        let head_x = Request::builder()
            .uri("/x")
            .method("HEAD")
            .header("host", "a.com")
            .body(Body::empty())
            .unwrap()
            .into_parts()
            .0;
        let get_y = parts_full("/y", &[("host", "a.com")]);
        let get_x_other_host = parts_full("/x", &[("host", "b.com")]);

        assert_ne!(
            cache_key_for(&vhost, &get_x),
            cache_key_for(&vhost, &head_x)
        );
        assert_ne!(cache_key_for(&vhost, &get_x), cache_key_for(&vhost, &get_y));
        assert_ne!(
            cache_key_for(&vhost, &get_x),
            cache_key_for(&vhost, &get_x_other_host)
        );
    }

    #[tokio::test]
    async fn store_if_cacheable_tags_miss_and_skips_storage_without_content_length() {
        let store = ConfigStore::new(crate::config::FerrixConfig::default(), "test.json");
        let metrics = MetricsCollector::new(10);
        let health = Arc::new(HealthChecker::new());
        let engine = ProxyEngine::new(Arc::new(store), metrics, health);

        let resp = Response::builder()
            .status(200)
            .body(Body::from_stream(futures_util::stream::iter([Ok::<
                _,
                std::io::Error,
            >(
                Bytes::from_static(b"chunk"),
            )])))
            .unwrap();
        let cfg = crate::config::CacheConfig::default();

        let out = engine
            .store_if_cacheable(&Default::default(), "k".to_string(), &cfg, resp)
            .await;
        assert_eq!(out.headers().get("x-cache-status").unwrap(), "MISS");
        assert!(engine.response_cache.get("k").is_none());
    }

    #[tokio::test]
    async fn store_if_cacheable_stores_and_replays_a_small_response_with_content_length() {
        let store = ConfigStore::new(crate::config::FerrixConfig::default(), "test.json");
        let metrics = MetricsCollector::new(10);
        let health = Arc::new(HealthChecker::new());
        let engine = ProxyEngine::new(Arc::new(store), metrics, health);

        let body_bytes = b"hello cache".to_vec();
        let resp = Response::builder()
            .status(200)
            .header(header::CONTENT_LENGTH, body_bytes.len())
            .body(Body::from(body_bytes.clone()))
            .unwrap();
        let cfg = crate::config::CacheConfig::default();

        let out = engine
            .store_if_cacheable(&Default::default(), "k2".to_string(), &cfg, resp)
            .await;
        assert_eq!(out.headers().get("x-cache-status").unwrap(), "MISS");

        let cached = engine
            .response_cache
            .get("k2")
            .expect("deveria ter guardado");
        assert_eq!(&cached.body[..], &body_bytes[..]);
        let replayed = build_cached_response(&cached);
        assert_eq!(replayed.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn store_if_cacheable_never_stores_a_response_with_set_cookie() {
        let store = ConfigStore::new(crate::config::FerrixConfig::default(), "test.json");
        let metrics = MetricsCollector::new(10);
        let health = Arc::new(HealthChecker::new());
        let engine = ProxyEngine::new(Arc::new(store), metrics, health);

        let body_bytes = b"secret".to_vec();
        let resp = Response::builder()
            .status(200)
            .header(header::CONTENT_LENGTH, body_bytes.len())
            .header(header::SET_COOKIE, "sess=abc")
            .body(Body::from(body_bytes))
            .unwrap();
        let cfg = crate::config::CacheConfig::default();

        engine
            .store_if_cacheable(&Default::default(), "k3".to_string(), &cfg, resp)
            .await;
        assert!(engine.response_cache.get("k3").is_none());
    }
}

#[cfg(test)]
mod stale_tests {
    use super::*;
    use crate::config::store::ConfigStore;
    use crate::config::{CacheConfig, FerrixConfig, RouteRule, UpstreamConfig, UpstreamTarget};
    use crate::health::HealthChecker;
    use crate::metrics::MetricsCollector;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    async fn backend(hits: Arc<AtomicUsize>) -> String {
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = l.local_addr().unwrap();
        tokio::spawn(async move {
            loop {
                let Ok((mut s, _)) = l.accept().await else {
                    return;
                };
                let hits = hits.clone();
                tokio::spawn(async move {
                    let mut buf = vec![0u8; 4096];
                    let n = s.read(&mut buf).await.unwrap_or(0);
                    hits.fetch_add(1, Ordering::SeqCst);
                    let req = String::from_utf8_lossy(&buf[..n]).to_ascii_lowercase();
                    let resp = if req.contains("if-none-match: \"v1\"") {
                        "HTTP/1.1 304 Not Modified\r\netag: \"v1\"\r\nconnection: close\r\n\r\n"
                            .to_string()
                    } else {
                        "HTTP/1.1 200 OK\r\netag: \"v1\"\r\ncontent-length: 2\r\nconnection: close\r\n\r\nv1".to_string()
                    };
                    let _ = s.write_all(resp.as_bytes()).await;
                });
            }
        });
        format!("http://{addr}")
    }

    fn engine() -> ProxyEngine {
        let store = ConfigStore::new(FerrixConfig::default(), "test.json");
        ProxyEngine::new(
            Arc::new(store),
            MetricsCollector::new(10),
            Arc::new(HealthChecker::new()),
        )
    }

    async fn get(e: &ProxyEngine, url: &str, cfg: &CacheConfig) -> (String, String) {
        let compiled =
            crate::http::matcher::CompiledConfig::build(Arc::new(FerrixConfig::default()));
        let route = RouteRule::default();
        let target = UpstreamTarget {
            url: url.into(),
            weight: 1,
            max_fails: 100,
            fail_timeout_secs: 1,
            backup: false,
            down: false,
            max_conns: None,
        };
        let up = UpstreamConfig {
            name: "u".into(),
            algorithm: Default::default(),
            targets: vec![target.clone()],
            health_check: None,
            verify_tls: true,
            hash_key: None,
            host_header: None,
            sticky: None,
            tls: None,
        };
        let parts = axum::extract::Request::builder()
            .uri("/x")
            .header("host", "h")
            .body(Body::empty())
            .unwrap()
            .into_parts()
            .0;
        let r = e
            .serve_with_cache(
                &compiled,
                &route,
                "u",
                &up,
                vec![&target],
                false,
                "127.0.0.1".parse().unwrap(),
                parts,
                Body::empty(),
                "k".into(),
                cfg,
            )
            .await;
        let tag = r
            .headers()
            .get("x-cache-status")
            .map(|v| v.to_str().unwrap().to_string())
            .unwrap_or_default();
        let b = axum::body::to_bytes(r.into_body(), 1024).await.unwrap();
        (tag, String::from_utf8_lossy(&b).into())
    }

    #[tokio::test]
    async fn revalidate_then_stale_on_error() {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let hits = Arc::new(AtomicUsize::new(0));
        let url = backend(hits.clone()).await;
        let e = engine();
        let cfg = CacheConfig {
            valid_secs: 0,
            revalidate: true,
            use_stale: vec!["error".into()],
            ..Default::default()
        };
        assert_eq!(get(&e, &url, &cfg).await, ("MISS".into(), "v1".into()));
        assert_eq!(
            get(&e, &url, &cfg).await,
            ("REVALIDATED".into(), "v1".into())
        );
        assert_eq!(hits.load(Ordering::SeqCst), 2);
        assert_eq!(
            get(&e, "http://127.0.0.1:1", &cfg).await,
            ("STALE".into(), "v1".into())
        );
    }

    #[test]
    fn stale_conditions() {
        let cfg = CacheConfig {
            use_stale: vec!["error".into(), "timeout".into(), "http_503".into()],
            ..Default::default()
        };
        assert!(stale_allowed(&cfg, 502) && stale_allowed(&cfg, 504) && stale_allowed(&cfg, 503));
        assert!(!stale_allowed(&cfg, 500) && !stale_allowed(&cfg, 200));
    }
}

#[cfg(test)]
mod validity_tests {
    use super::*;
    use axum::http::HeaderMap;

    fn h(pairs: &[(&str, &str)]) -> HeaderMap {
        let mut m = HeaderMap::new();
        for (k, v) in pairs {
            m.insert(
                axum::http::HeaderName::from_bytes(k.as_bytes()).unwrap(),
                v.parse().unwrap(),
            );
        }
        m
    }

    #[test]
    fn nginx_precedence() {
        let mut cfg = CacheConfig::default();
        assert_eq!(validity(&cfg, 200, &h(&[])), Some(Duration::from_secs(60)));
        assert_eq!(validity(&cfg, 404, &h(&[])), None);
        assert_eq!(
            validity(&cfg, 200, &h(&[("cache-control", "public, max-age=7")])),
            Some(Duration::from_secs(7))
        );
        assert_eq!(
            validity(&cfg, 200, &h(&[("cache-control", "s-maxage=9, max-age=7")])),
            Some(Duration::from_secs(9))
        );
        assert_eq!(
            validity(&cfg, 200, &h(&[("cache-control", "private")])),
            None
        );
        assert_eq!(
            validity(
                &cfg,
                200,
                &h(&[("x-accel-expires", "3"), ("cache-control", "max-age=7")])
            ),
            Some(Duration::from_secs(3))
        );
        assert_eq!(validity(&cfg, 200, &h(&[("set-cookie", "a=1")])), None);
        assert_eq!(validity(&cfg, 200, &h(&[("vary", "*")])), None);
        cfg.valid_by_status = [("404".to_string(), 10), ("any".to_string(), 2)]
            .into_iter()
            .collect();
        assert_eq!(validity(&cfg, 404, &h(&[])), Some(Duration::from_secs(10)));
        assert_eq!(validity(&cfg, 500, &h(&[])), Some(Duration::from_secs(2)));
        cfg.ignore_headers = vec!["Cache-Control".into(), "Set-Cookie".into()];
        assert_eq!(
            validity(
                &cfg,
                404,
                &h(&[("cache-control", "no-store"), ("set-cookie", "a")])
            ),
            Some(Duration::from_secs(10))
        );
    }
}
