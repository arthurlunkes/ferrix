//! Cache de resposta HTTP, equivalente a `proxy_cache` do nginx.
//!
//! Deliberadamente simples: uma tabela em memória, sem persistência em disco e sem
//! separação por zona — uma configuração real nginx costuma ter uma zona por site, mas
//! aqui todo mundo compartilha a mesma tabela, diferenciado pela própria chave (que já
//! inclui host e caminho). O que importa é o efeito prático que `proxy_cache_bypass`
//! promete: menos ida ao upstream em rotas de leitura pesada.

use axum::body::Bytes;
use axum::http::HeaderValue;
use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

/// Acima disso, uma resposta não é guardada — cache em memória sem limite de corpo
/// individual derrubaria o processo com um arquivo grande servido por engano via proxy.
pub const MAX_CACHEABLE_BODY_BYTES: u64 = 2 * 1024 * 1024;

/// Nenhuma configuração define um teto explícito de entradas; sem um, uma rota cacheável
/// com muita variação de chave (busca com querystring livre, por exemplo) cresceria sem
/// parar. Quando cheio, a entrada mais antiga por inserção sai — não é LRU de verdade,
/// mas evita crescimento ilimitado sem precisar de mais uma dependência.
const MAX_ENTRIES: usize = 10_000;

#[derive(Debug, Clone)]
pub struct CachedResponse {
    pub status: u16,
    /// Só os headers que fazem sentido reproduzir; hop-by-hop já foi filtrado por quem
    /// constrói a entrada, mas isso é responsabilidade de quem monta o `CachedResponse`.
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

    /// `None` tanto para "nunca guardado" quanto para "guardado, mas venceu" — quem chama
    /// não precisa diferenciar os dois casos, os dois significam "vá ao upstream".
    pub fn get(&self, key: &str) -> Option<CachedResponse> {
        let now = Instant::now();
        {
            let entries = self.entries.read().ok()?;
            let entry = entries.get(key)?;
            if entry.response.expires_at > now {
                return Some(entry.response.clone());
            }
        }
        // Venceu — remove daqui em vez de esperar o próximo `put` disputar o slot.
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

/// A chave inclui método (GET e HEAD não devem colidir), host e caminho+query — o mesmo
/// que o template padrão do nginx (`$scheme$proxy_host$request_uri`), menos o esquema
/// porque uma mesma origem servindo HTTP e HTTPS quer a mesma entrada nos dois.
pub fn cache_key(method: &str, host: &str, path_and_query: &str) -> String {
    format!("{}:{}:{}", method, host, path_and_query)
}

/// Só cabeçalhos seguros de reproduzir numa resposta futura para outro cliente. Filtra
/// hop-by-hop (tratados à parte no proxy) e `Set-Cookie` — o padrão do próprio nginx é
/// não cachear respostas com `Set-Cookie` justamente para não vazar sessão de um cliente
/// para outro; aqui o corpo ainda é cacheado, mas o cookie de sessão nunca é reproduzido.
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

/// Nunca guardar uma resposta que trouxe `Set-Cookie` — mesmo critério do nginx.
pub fn has_set_cookie(headers: &axum::http::HeaderMap) -> bool {
    headers.contains_key(axum::http::header::SET_COOKIE)
}

/// O nginx respeita por padrão um `Cache-Control: no-store/no-cache/private` que o
/// backend mandou, a menos que `proxy_ignore_headers` diga o contrário — o Ferrix não
/// implementa esse override, então respeita sempre. É o backend dizendo "esta resposta
/// é sensível ou muda por usuário"; ignorar isso é como vaza sessão de um cliente pra
/// outro em cache compartilhado.
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
