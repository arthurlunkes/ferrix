//! Per-upstream TLS client settings (nginx `proxy_ssl_certificate`,
//! `proxy_ssl_certificate_key`, `proxy_ssl_trusted_certificate`,
//! `proxy_ssl_name`, `proxy_ssl_verify`).

use std::collections::HashMap;
use std::sync::{Arc, LazyLock, Mutex};
use tokio_rustls::TlsConnector;

use crate::config::UpstreamTlsConfig;

pub struct CustomTls {
    pub connector: TlsConnector,
    /// SNI / certificate name to use instead of the target host.
    pub server_name: Option<String>,
    /// Distinguishes pooled connections made with these settings.
    pub pool_tag: String,
}

/// Built connectors live for the whole process (one per distinct setting).
static CACHE: LazyLock<Mutex<HashMap<String, &'static CustomTls>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

fn read_pem(path: &str) -> Result<Vec<u8>, String> {
    std::fs::read(path).map_err(|e| format!("{path}: {e}"))
}

fn build(cfg: &UpstreamTlsConfig, verify: bool) -> Result<CustomTls, String> {
    let builder = rustls::ClientConfig::builder();
    let builder = if verify {
        let mut roots = rustls::RootCertStore::empty();
        match &cfg.trusted_ca {
            Some(ca) => {
                let pem = read_pem(ca)?;
                for c in rustls_pemfile::certs(&mut pem.as_slice()) {
                    roots
                        .add(c.map_err(|e| format!("{ca}: {e}"))?)
                        .map_err(|e| format!("{ca}: {e}"))?;
                }
            }
            None => roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned()),
        }
        builder.with_root_certificates(roots)
    } else {
        // Same no-verify path as the shared insecure connector.
        return Ok(CustomTls {
            connector: super::h1::tls_connector_with_client_auth(false, client_auth(cfg)?),
            server_name: cfg.server_name.clone(),
            pool_tag: tag(cfg, verify),
        });
    };
    let mut config = match client_auth(cfg)? {
        Some((certs, key)) => builder
            .with_client_auth_cert(certs, key)
            .map_err(|e| e.to_string())?,
        None => builder.with_no_client_auth(),
    };
    config.alpn_protocols = vec![b"http/1.1".to_vec()];
    Ok(CustomTls {
        connector: TlsConnector::from(Arc::new(config)),
        server_name: cfg.server_name.clone(),
        pool_tag: tag(cfg, verify),
    })
}

pub type ClientAuth = Option<(
    Vec<rustls::pki_types::CertificateDer<'static>>,
    rustls::pki_types::PrivateKeyDer<'static>,
)>;

fn client_auth(cfg: &UpstreamTlsConfig) -> Result<ClientAuth, String> {
    let (Some(cert), Some(key)) = (&cfg.client_cert, &cfg.client_key) else {
        return Ok(None);
    };
    let cert_pem = read_pem(cert)?;
    let certs = rustls_pemfile::certs(&mut cert_pem.as_slice())
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| format!("{cert}: {e}"))?;
    let key_pem = read_pem(key)?;
    let key = rustls_pemfile::private_key(&mut key_pem.as_slice())
        .map_err(|e| format!("{key}: {e}"))?
        .ok_or_else(|| format!("{key}: no private key"))?;
    Ok(Some((certs, key)))
}

fn tag(cfg: &UpstreamTlsConfig, verify: bool) -> String {
    format!(
        "{}|{}|{}|{}|{}",
        verify,
        cfg.client_cert.as_deref().unwrap_or(""),
        cfg.client_key.as_deref().unwrap_or(""),
        cfg.trusted_ca.as_deref().unwrap_or(""),
        cfg.server_name.as_deref().unwrap_or("")
    )
}

/// Connector for these settings, built once. Errors (unreadable files) are
/// logged and fall back to the default connector.
pub fn get(cfg: Option<&UpstreamTlsConfig>, verify: bool) -> Option<&'static CustomTls> {
    let cfg = cfg?;
    let key = tag(cfg, verify);
    let mut cache = CACHE.lock().ok()?;
    if let Some(c) = cache.get(&key) {
        return Some(c);
    }
    match build(cfg, verify) {
        Ok(c) => {
            let leaked: &'static CustomTls = Box::leak(Box::new(c));
            cache.insert(key, leaked);
            Some(leaked)
        }
        Err(e) => {
            tracing::error!(error = %e, "upstream TLS settings could not be loaded");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::http::upstream::{OutgoingBody, UpstreamClient};
    use axum::http::{HeaderMap, Method};
    use std::time::Duration;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    const DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/tls");

    fn pem_certs(name: &str) -> Vec<rustls::pki_types::CertificateDer<'static>> {
        let pem = std::fs::read(format!("{DIR}/{name}")).unwrap();
        rustls_pemfile::certs(&mut pem.as_slice())
            .collect::<Result<_, _>>()
            .unwrap()
    }

    /// TLS backend that requires a client certificate signed by the test CA.
    async fn mtls_backend() -> u16 {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let mut roots = rustls::RootCertStore::empty();
        for c in pem_certs("ca.pem") {
            roots.add(c).unwrap();
        }
        let verifier = rustls::server::WebPkiClientVerifier::builder(Arc::new(roots))
            .build()
            .unwrap();
        let key_pem = std::fs::read(format!("{DIR}/server.key")).unwrap();
        let key = rustls_pemfile::private_key(&mut key_pem.as_slice())
            .unwrap()
            .unwrap();
        let config = rustls::ServerConfig::builder()
            .with_client_cert_verifier(verifier)
            .with_single_cert(pem_certs("server.pem"), key)
            .unwrap();
        let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(config));
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = l.local_addr().unwrap().port();
        tokio::spawn(async move {
            while let Ok((tcp, _)) = l.accept().await {
                let acceptor = acceptor.clone();
                tokio::spawn(async move {
                    let Ok(mut s) = acceptor.accept(tcp).await else {
                        return;
                    };
                    let mut buf = [0u8; 2048];
                    let _ = s.read(&mut buf).await;
                    let _ = s
                        .write_all(
                            b"HTTP/1.1 200 OK\r\ncontent-length: 2\r\nconnection: close\r\n\r\nok",
                        )
                        .await;
                    let _ = s.shutdown().await;
                });
            }
        });
        port
    }

    fn client(cfg: &UpstreamTlsConfig) -> UpstreamClient {
        UpstreamClient::new(Duration::from_secs(2), Duration::from_secs(2), true)
            .with_tls(Some(cfg))
    }

    #[tokio::test]
    async fn client_certificate_trusted_ca_and_server_name() {
        let port = mtls_backend().await;
        let base = format!("https://127.0.0.1:{port}");
        let ok = UpstreamTlsConfig {
            client_cert: Some(format!("{DIR}/client.pem")),
            client_key: Some(format!("{DIR}/client.key")),
            trusted_ca: Some(format!("{DIR}/ca.pem")),
            server_name: Some("backend.internal".into()),
        };
        let r = client(&ok)
            .send(
                &Method::GET,
                &base,
                "/",
                &HeaderMap::new(),
                OutgoingBody::Empty,
            )
            .await
            .unwrap();
        assert_eq!(r.status(), 200);

        // No client certificate: the backend refuses the handshake.
        let no_cert = UpstreamTlsConfig {
            client_cert: None,
            client_key: None,
            ..ok.clone()
        };
        assert!(client(&no_cert)
            .send(
                &Method::GET,
                &base,
                "/",
                &HeaderMap::new(),
                OutgoingBody::Empty
            )
            .await
            .map(|r| r.status() != 200)
            .unwrap_or(true));

        // Wrong name: certificate check fails.
        let wrong = UpstreamTlsConfig {
            server_name: Some("other.internal".into()),
            ..ok
        };
        assert!(client(&wrong)
            .send(
                &Method::GET,
                &base,
                "/",
                &HeaderMap::new(),
                OutgoingBody::Empty
            )
            .await
            .is_err());
    }
}
