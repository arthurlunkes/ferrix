use axum::body::Bytes;
use axum::http::HeaderValue;
use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

pub const MAX_CACHEABLE_BODY_BYTES: u64 = 2 * 1024 * 1024;

const MAX_ENTRIES: usize = 10_000;

#[derive(Debug, Clone)]
pub struct CachedResponse {
    pub status: u16,

    pub headers: Vec<(String, String)>,
    pub body: Bytes,
    expires_at: Instant,
    inserted_seq: u64,
}

struct Entry {
    response: CachedResponse,
}

pub struct ResponseCache {
    entries: RwLock<HashMap<String, Entry>>,
    seq: std::sync::atomic::AtomicU64,
}

impl ResponseCache {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            entries: RwLock::new(HashMap::new()),
            seq: std::sync::atomic::AtomicU64::new(0),
        })
    }

    pub fn get(&self, key: &str) -> Option<CachedResponse> {
        let now = Instant::now();
        {
            let entries = self.entries.read().ok()?;
            let entry = entries.get(key)?;
            if entry.response.expires_at > now {
                return Some(entry.response.clone());
            }
        }

        if let Ok(mut entries) = self.entries.write() {
            entries.remove(key);
        }
        None
    }

    pub fn put(
        &self,
        key: String,
        status: u16,
        headers: Vec<(String, String)>,
        body: Bytes,
        valid_for: Duration,
    ) {
        let seq = self.seq.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let response = CachedResponse {
            status,
            headers,
            body,
            expires_at: Instant::now() + valid_for,
            inserted_seq: seq,
        };

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

// --- Proxy cache glue (nginx `proxy_cache`): deciding what to cache and replaying it ---

use crate::config::{CacheConfig, VirtualHost};
use crate::http::ProxyEngine;
use axum::body::Body;
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};

impl ProxyEngine {
    pub(super) async fn store_if_cacheable(
        &self,
        key: String,
        cfg: &CacheConfig,
        resp: Response,
    ) -> Response {
        let status = resp.status();
        let cacheable = status == StatusCode::OK
            && !crate::http::cache::has_set_cookie(resp.headers())
            && !crate::http::cache::upstream_forbids_cache(resp.headers())
            && resp
                .headers()
                .get(header::CONTENT_LENGTH)
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.parse::<u64>().ok())
                .is_some_and(|len| len <= crate::http::cache::MAX_CACHEABLE_BODY_BYTES);

        if !cacheable {
            let mut r = resp;
            if let Some(v) = crate::http::cache::header_value("MISS") {
                r.headers_mut().insert("x-cache-status", v);
            }
            return r;
        }

        let headers = crate::http::cache::cacheable_headers(resp.headers());
        let (parts, body) = resp.into_parts();
        match axum::body::to_bytes(body, crate::http::cache::MAX_CACHEABLE_BODY_BYTES as usize)
            .await
        {
            Ok(bytes) => {
                self.response_cache.put(
                    key,
                    parts.status.as_u16(),
                    headers,
                    bytes.clone(),
                    Duration::from_secs(cfg.valid_secs),
                );
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
            rate_limit: None,
            limit_conn: None,
            allowed_ips: Vec::new(),
            denied_ips: Vec::new(),
            basic_auth: None,
            satisfy: crate::config::Satisfy::All,
            tls: None,
            routes: Vec::new(),
            error_pages: Default::default(),
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

        let out = engine.store_if_cacheable("k".to_string(), &cfg, resp).await;
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
            .store_if_cacheable("k2".to_string(), &cfg, resp)
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
            .store_if_cacheable("k3".to_string(), &cfg, resp)
            .await;
        assert!(engine.response_cache.get("k3").is_none());
    }
}
