//! TLS certificate selection by SNI (several sites on one port) and the rustls
//! acceptor, the role of nginx's `ngx_event_openssl` + `ngx_http_ssl_module`.

use arc_swap::ArcSwap;
use rustls::server::{ClientHello, ResolvesServerCert};
use rustls::sign::CertifiedKey;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tokio_rustls::TlsAcceptor;
use tracing::error;

use crate::config::store::ConfigStore;
use crate::config::FerrixConfig;

/// Picks the certificate for a TLS handshake from the SNI name, nginx-style: exact
/// `server_names` first, then wildcards (`*.example.com` / `.example.com`), falling back
/// to the first TLS virtual host on the port. Certificates are re-read whenever the
/// active configuration changes, so renewing a certificate only needs a config reload.
pub struct SniResolver {
    store: Arc<ConfigStore>,
    port: u16,
    cache: ArcSwap<CertTable>,
    rebuild: Mutex<()>,
}

#[derive(Default)]
struct CertTable {
    config: Option<Arc<FerrixConfig>>,
    generation: u64,
    exact: HashMap<String, Arc<CertifiedKey>>,
    // (".example.com", also matches bare "example.com", key)
    wildcard: Vec<(String, bool, Arc<CertifiedKey>)>,
    default: Option<Arc<CertifiedKey>>,
}

impl std::fmt::Debug for SniResolver {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SniResolver")
            .field("port", &self.port)
            .finish()
    }
}

impl SniResolver {
    pub fn new(store: Arc<ConfigStore>, port: u16) -> Result<Arc<Self>, String> {
        let resolver = Arc::new(Self {
            store,
            port,
            cache: ArcSwap::from_pointee(CertTable::default()),
            rebuild: Mutex::new(()),
        });
        let config = resolver.store.config();
        let table = build_table(&config, port)?;
        // An ACME host may not have its first certificate yet; the port still starts so
        // it can serve as soon as the certificate is issued.
        let waits_for_acme = config
            .vhosts
            .iter()
            .any(|v| v.listen_port == port && v.tls.as_ref().is_some_and(|t| t.acme));
        if table.default.is_none() && !waits_for_acme {
            return Err(format!("no TLS certificate configured for port {}", port));
        }
        resolver.cache.store(Arc::new(table));
        Ok(resolver)
    }

    fn current(&self) -> Arc<CertTable> {
        let config = self.store.config();
        let generation =
            crate::tls::acme::CERT_GENERATION.load(std::sync::atomic::Ordering::Acquire);
        let up_to_date = |t: &CertTable| {
            t.generation == generation && t.config.as_ref().is_some_and(|c| Arc::ptr_eq(c, &config))
        };
        let table = self.cache.load_full();
        if up_to_date(&table) {
            return table;
        }
        let Ok(_guard) = self.rebuild.lock() else {
            return table;
        };
        let table = self.cache.load_full();
        if up_to_date(&table) {
            return table;
        }
        match build_table(&config, self.port) {
            Ok(mut fresh) => {
                fresh.generation = generation;
                let fresh = Arc::new(fresh);
                self.cache.store(fresh.clone());
                fresh
            }
            Err(e) => {
                // Keep serving the previous certificates rather than failing handshakes.
                error!(port = self.port, "reloading TLS certificates failed: {}", e);
                let kept = Arc::new(CertTable {
                    config: Some(config),
                    generation,
                    exact: table.exact.clone(),
                    wildcard: table.wildcard.clone(),
                    default: table.default.clone(),
                });
                self.cache.store(kept.clone());
                kept
            }
        }
    }
}

impl ResolvesServerCert for SniResolver {
    fn resolve(&self, hello: ClientHello<'_>) -> Option<Arc<CertifiedKey>> {
        let table = self.current();
        if let Some(name) = hello.server_name() {
            let name = name.to_ascii_lowercase();
            if let Some(k) = table.exact.get(&name) {
                return Some(k.clone());
            }
            for (suffix, bare, k) in &table.wildcard {
                if name.ends_with(suffix.as_str()) || (*bare && name == suffix[1..]) {
                    return Some(k.clone());
                }
            }
        }
        table.default.clone()
    }
}

fn build_table(config: &Arc<FerrixConfig>, port: u16) -> Result<CertTable, String> {
    let mut table = CertTable {
        config: Some(config.clone()),
        ..Default::default()
    };
    let mut loaded: HashMap<(String, String), Arc<CertifiedKey>> = HashMap::new();
    for vhost in config.vhosts.iter().filter(|v| v.listen_port == port) {
        let Some(tls) = &vhost.tls else { continue };
        let key = if tls.acme {
            let (cert, key) = crate::tls::acme::cert_paths(&config.acme, &vhost.id);
            if !cert.is_file() {
                continue; // not issued yet
            }
            (
                cert.to_string_lossy().into_owned(),
                key.to_string_lossy().into_owned(),
            )
        } else {
            (tls.cert_path.clone(), tls.key_path.clone())
        };
        let certified = match loaded.get(&key) {
            Some(k) => k.clone(),
            None => {
                let k = Arc::new(
                    load_certified_key(&key.0, &key.1)
                        .map_err(|e| format!("virtual host '{}': {}", vhost.name, e))?,
                );
                loaded.insert(key, k.clone());
                k
            }
        };
        table.default.get_or_insert_with(|| certified.clone());
        for name in &vhost.server_names {
            let name = name.trim().to_ascii_lowercase();
            if let Some(rest) = name.strip_prefix("*.") {
                table
                    .wildcard
                    .push((format!(".{}", rest), false, certified.clone()));
            } else if name.starts_with('.') && name.len() > 1 {
                table.wildcard.push((name, true, certified.clone()));
            } else if !name.is_empty() && name != "_" && !name.starts_with('~') {
                table.exact.entry(name).or_insert_with(|| certified.clone());
            }
        }
    }
    // Longest suffix first so `*.api.example.com` beats `*.example.com`.
    table
        .wildcard
        .sort_by_key(|(s, _, _)| std::cmp::Reverse(s.len()));
    Ok(table)
}

pub fn load_certified_key(cert_path: &str, key_path: &str) -> Result<CertifiedKey, String> {
    let cert_pem = std::fs::read(cert_path)
        .map_err(|e| format!("cannot read certificate {}: {}", cert_path, e))?;
    let key_pem =
        std::fs::read(key_path).map_err(|e| format!("cannot read key {}: {}", key_path, e))?;
    let certs: Vec<_> = rustls_pemfile::certs(&mut cert_pem.as_slice())
        .collect::<Result<_, _>>()
        .map_err(|e| format!("invalid certificate {}: {}", cert_path, e))?;
    if certs.is_empty() {
        return Err(format!("no certificate found in {}", cert_path));
    }
    let key = rustls_pemfile::private_key(&mut key_pem.as_slice())
        .map_err(|e| format!("invalid key {}: {}", key_path, e))?
        .ok_or_else(|| format!("no private key found in {}", key_path))?;
    let signing = rustls::crypto::ring::sign::any_supported_type(&key)
        .map_err(|e| format!("unsupported key {}: {}", key_path, e))?;
    Ok(CertifiedKey::new(certs, signing))
}

pub fn tls_acceptor(resolver: Arc<SniResolver>) -> Result<TlsAcceptor, String> {
    let mut config = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_cert_resolver(resolver);
    config.alpn_protocols = if cfg!(feature = "http2") {
        vec![b"h2".to_vec(), b"http/1.1".to_vec()]
    } else {
        vec![b"http/1.1".to_vec()]
    };
    // Session tickets let returning clients resume without a full handshake.
    if let Ok(ticketer) = rustls::crypto::ring::Ticketer::new() {
        config.ticketer = ticketer;
    }
    Ok(TlsAcceptor::from(Arc::new(config)))
}
