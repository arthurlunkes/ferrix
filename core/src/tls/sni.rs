use arc_swap::ArcSwap;
use rustls::server::{ClientHello, ResolvesServerCert};
use rustls::sign::CertifiedKey;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tokio_rustls::TlsAcceptor;
use tracing::error;

use crate::config::store::ConfigStore;
use crate::config::FerrixConfig;

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
    wildcard: Vec<(String, bool, Arc<CertifiedKey>)>,
    default: Option<Arc<CertifiedKey>>,
    rejected: Vec<String>,
    reject_default: bool,
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
        let waits_for_acme = config
            .vhosts
            .iter()
            .any(|v| v.listen_port == port && v.tls.as_ref().is_some_and(|t| t.acme));
        if table.default.is_none() && !waits_for_acme && !table.reject_default {
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
                error!(port = self.port, "reloading TLS certificates failed: {}", e);
                let kept = Arc::new(CertTable {
                    config: Some(config),
                    generation,
                    exact: table.exact.clone(),
                    wildcard: table.wildcard.clone(),
                    default: table.default.clone(),
                    rejected: table.rejected.clone(),
                    reject_default: table.reject_default,
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
            if table.rejected.contains(&name) {
                return None;
            }
            if let Some(k) = table.exact.get(&name) {
                return Some(k.clone());
            }
            for (suffix, bare, k) in &table.wildcard {
                if name.ends_with(suffix.as_str()) || (*bare && name == suffix[1..]) {
                    return Some(k.clone());
                }
            }
        }
        if table.reject_default {
            return None;
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
        if tls.reject_handshake {
            let names: Vec<String> = vhost
                .server_names
                .iter()
                .map(|n| n.trim().to_ascii_lowercase())
                .filter(|n| !n.is_empty() && n != "_")
                .collect();
            if names.is_empty() {
                table.reject_default = true;
            }
            table.rejected.extend(names);
            continue;
        }
        let ocsp = match &tls.ocsp_file {
            Some(f) => Some(std::fs::read(f).map_err(|e| {
                format!("virtual host '{}': OCSP response {}: {}", vhost.name, f, e)
            })?),
            None => None,
        };
        let key = if tls.acme {
            let (cert, key) = crate::tls::acme::cert_paths(&config.acme, &vhost.id);
            if !cert.is_file() {
                continue;
            }
            (
                cert.to_string_lossy().into_owned(),
                key.to_string_lossy().into_owned(),
            )
        } else {
            (tls.cert_path.clone(), tls.key_path.clone())
        };
        let certified = match loaded.get(&key).filter(|_| ocsp.is_none()) {
            Some(k) => k.clone(),
            None => {
                let mut ck =
                    load_certified_key_with(&key.0, &key.1, tls.key_password_file.as_deref())
                        .map_err(|e| format!("virtual host '{}': {}", vhost.name, e))?;
                ck.ocsp = ocsp;
                let k = Arc::new(ck);
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
    table
        .wildcard
        .sort_by_key(|(s, _, _)| std::cmp::Reverse(s.len()));
    Ok(table)
}

pub fn load_certified_key(cert_path: &str, key_path: &str) -> Result<CertifiedKey, String> {
    load_certified_key_with(cert_path, key_path, None)
}

pub fn load_certified_key_with(
    cert_path: &str,
    key_path: &str,
    password_file: Option<&str>,
) -> Result<CertifiedKey, String> {
    let cert_pem = std::fs::read(cert_path)
        .map_err(|e| format!("cannot read certificate {}: {}", cert_path, e))?;
    let certs: Vec<_> = rustls_pemfile::certs(&mut cert_pem.as_slice())
        .collect::<Result<_, _>>()
        .map_err(|e| format!("invalid certificate {}: {}", cert_path, e))?;
    if certs.is_empty() {
        return Err(format!("no certificate found in {}", cert_path));
    }
    let key = crate::tls::settings::private_key(key_path, password_file)?;
    let signing = rustls::crypto::ring::sign::any_supported_type(&key)
        .map_err(|e| format!("unsupported key {}: {}", key_path, e))?;
    Ok(CertifiedKey::new(certs, signing))
}

#[cfg(feature = "http3")]
pub fn quic_server_config(resolver: Arc<SniResolver>) -> Result<rustls::ServerConfig, String> {
    let settings = resolver.store.config().http.tls.clone();
    let mut config =
        rustls::ServerConfig::builder_with_provider(crate::tls::settings::provider(&settings)?)
            .with_protocol_versions(&[&rustls::version::TLS13])
            .map_err(|e| e.to_string())?
            .with_no_client_auth()
            .with_cert_resolver(resolver);
    config.alpn_protocols = vec![b"h3".to_vec()];
    config.max_early_data_size = if settings.early_data { u32::MAX } else { 0 };
    Ok(config)
}

pub fn tls_acceptor(resolver: Arc<SniResolver>) -> Result<TlsAcceptor, String> {
    use crate::config::HttpProtocol;
    let cfg = resolver.store.config();
    let http = cfg.http.clone();
    let builder = crate::tls::settings::server_builder(&http.tls)?;
    let mut config = match crate::tls::client_cert::verifier_for_port(&cfg, resolver.port)? {
        Some(v) => builder.with_client_cert_verifier(v),
        None => builder.with_no_client_auth(),
    }
    .with_cert_resolver(resolver.clone());
    if cfg!(feature = "http2") && http.serves(HttpProtocol::Http2) {
        config.alpn_protocols.push(b"h2".to_vec());
    }
    if http.serves(HttpProtocol::Http1) {
        config.alpn_protocols.push(b"http/1.1".to_vec());
    }
    crate::tls::settings::apply_tickets(&http.tls, &mut config);
    Ok(TlsAcceptor::from(Arc::new(config)))
}

#[cfg(test)]
mod ocsp_tests {
    use super::*;

    #[test]
    fn ocsp_file_is_stapled() {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/tls");
        let ocsp = std::env::temp_dir().join(format!("ferrix-ocsp-{}", uuid::Uuid::new_v4()));
        std::fs::write(&ocsp, b"\x30\x03\x0a\x01\x00").unwrap();
        let mut cfg = FerrixConfig::default();
        cfg.vhosts[0].listen_port = 443;
        cfg.vhosts[0].tls = Some(crate::config::TlsConfig {
            cert_path: format!("{dir}/server.pem"),
            key_path: format!("{dir}/server.key"),
            ocsp_file: Some(ocsp.to_string_lossy().into_owned()),
            ..Default::default()
        });
        let table = build_table(&Arc::new(cfg), 443).unwrap();
        assert_eq!(
            table.default.unwrap().ocsp.as_deref(),
            Some(&b"\x30\x03\x0a\x01\x00"[..])
        );
        let _ = std::fs::remove_file(ocsp);
    }
}
