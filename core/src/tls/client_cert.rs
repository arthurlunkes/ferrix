use base64::Engine;
use std::sync::Arc;

use crate::config::{ClientVerify, FerrixConfig};

#[derive(Debug, Clone)]
pub struct ClientCert {
    pub der: Vec<u8>,
    pub subject: String,
    pub issuer: String,
}

impl ClientCert {
    pub fn from_der(der: &[u8]) -> Self {
        let (subject, issuer) = names(der);
        Self {
            der: der.to_vec(),
            subject,
            issuer,
        }
    }

    pub fn escaped_pem(&self) -> String {
        let b64 = base64::engine::general_purpose::STANDARD.encode(&self.der);
        let mut pem = String::from("-----BEGIN CERTIFICATE-----\n");
        for line in b64.as_bytes().chunks(64) {
            pem.push_str(std::str::from_utf8(line).unwrap_or(""));
            pem.push('\n');
        }
        pem.push_str("-----END CERTIFICATE-----\n");
        let mut out = String::with_capacity(pem.len() * 2);
        for b in pem.bytes() {
            if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) {
                out.push(b as char);
            } else {
                out.push_str(&format!("%{b:02X}"));
            }
        }
        out
    }
}

#[cfg(feature = "acme")]
fn names(der: &[u8]) -> (String, String) {
    match x509_parser::parse_x509_certificate(der) {
        Ok((_, c)) => (c.subject().to_string(), c.issuer().to_string()),
        Err(_) => (String::new(), String::new()),
    }
}

#[cfg(not(feature = "acme"))]
fn names(_der: &[u8]) -> (String, String) {
    (String::new(), String::new())
}

pub fn verifier_for_port(
    config: &FerrixConfig,
    port: u16,
) -> Result<Option<Arc<dyn rustls::server::danger::ClientCertVerifier>>, String> {
    let mut roots = rustls::RootCertStore::empty();
    let mut wanted = false;
    for v in config.vhosts.iter().filter(|v| v.listen_port == port) {
        let Some(tls) = &v.tls else { continue };
        if tls.verify_client == ClientVerify::Off {
            continue;
        }
        wanted = true;
        if let Some(ca) = &tls.client_ca {
            let pem = std::fs::read(ca).map_err(|e| format!("{ca}: {e}"))?;
            for c in rustls_pemfile::certs(&mut pem.as_slice()) {
                roots
                    .add(c.map_err(|e| format!("{ca}: {e}"))?)
                    .map_err(|e| format!("{ca}: {e}"))?;
            }
        }
    }
    if !wanted {
        return Ok(None);
    }
    if roots.is_empty() {
        return Err(format!(
            "port {port}: verify_client is on but no vhost there has a readable client_ca"
        ));
    }
    let v = rustls::server::WebPkiClientVerifier::builder(Arc::new(roots))
        .allow_unauthenticated()
        .build()
        .map_err(|e| e.to_string())?;
    Ok(Some(v))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::TlsConfig;

    const DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/tls");

    fn config(verify: ClientVerify, ca: Option<&str>) -> FerrixConfig {
        let mut c = FerrixConfig::default();
        c.vhosts[0].listen_port = 443;
        c.vhosts[0].tls = Some(TlsConfig {
            verify_client: verify,
            client_ca: ca.map(|f| format!("{DIR}/{f}")),
            ..Default::default()
        });
        c
    }

    #[test]
    fn verifier_only_when_a_vhost_asks() {
        let _ = rustls::crypto::ring::default_provider().install_default();
        assert!(verifier_for_port(&config(ClientVerify::Off, None), 443)
            .unwrap()
            .is_none());
        assert!(
            verifier_for_port(&config(ClientVerify::On, Some("ca.pem")), 443)
                .unwrap()
                .is_some()
        );
        assert!(
            verifier_for_port(&config(ClientVerify::On, Some("ca.pem")), 8443)
                .unwrap()
                .is_none()
        );
        assert!(verifier_for_port(&config(ClientVerify::On, None), 443).is_err());
    }

    #[test]
    fn certificate_details() {
        let pem = std::fs::read(format!("{DIR}/client.pem")).unwrap();
        let der = rustls_pemfile::certs(&mut pem.as_slice())
            .next()
            .unwrap()
            .unwrap();
        let c = ClientCert::from_der(&der);
        if cfg!(feature = "acme") {
            assert!(c.subject.contains("CN=client"), "{}", c.subject);
            assert!(c.issuer.contains("Ferrix Test CA"), "{}", c.issuer);
        }
        let esc = c.escaped_pem();
        assert!(esc.starts_with("-----BEGIN%20CERTIFICATE-----%0A"));
    }

    #[tokio::test]
    async fn required_certificate_is_enforced_per_vhost() {
        use crate::config::store::ConfigStore;
        use crate::health::HealthChecker;
        use crate::http::ProxyEngine;
        use crate::metrics::MetricsCollector;
        let mut cfg = config(ClientVerify::On, Some("ca.pem"));
        cfg.vhosts[0].routes[0].target_type = crate::config::RouteTargetType::Return;
        cfg.vhosts[0].routes[0].redirect_code = Some(200);
        cfg.vhosts[0].routes[0].return_body = Some("$ssl_client_verify".into());
        let engine = ProxyEngine::new(
            Arc::new(ConfigStore::new(cfg, "t.json")),
            MetricsCollector::new(10),
            Arc::new(HealthChecker::new()),
        );
        let addr = "127.0.0.1:5000".parse().unwrap();
        let req = || {
            axum::http::Request::builder()
                .uri("/")
                .header("host", "x")
                .body(axum::body::Body::empty())
                .unwrap()
        };
        let r = engine.handle_request(443, true, addr, req()).await;
        assert_eq!(r.status(), 400);

        let pem = std::fs::read(format!("{DIR}/client.pem")).unwrap();
        let der = rustls_pemfile::certs(&mut pem.as_slice())
            .next()
            .unwrap()
            .unwrap();
        let mut with = req();
        with.extensions_mut()
            .insert(Arc::new(ClientCert::from_der(&der)));
        let r = engine.handle_request(443, true, addr, with).await;
        assert_eq!(r.status(), 200);
    }
}
