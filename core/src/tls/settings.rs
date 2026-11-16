use std::sync::Arc;

use rustls::crypto::CryptoProvider;
use rustls::pki_types::PrivateKeyDer;
use rustls::server::ProducesTickets;
use rustls::SupportedProtocolVersion;

use crate::config::TlsSettings;

const OPENSSL_NAMES: [(&str, &str); 9] = [
    ("TLS_AES_256_GCM_SHA384", "TLS13_AES_256_GCM_SHA384"),
    ("TLS_AES_128_GCM_SHA256", "TLS13_AES_128_GCM_SHA256"),
    (
        "TLS_CHACHA20_POLY1305_SHA256",
        "TLS13_CHACHA20_POLY1305_SHA256",
    ),
    (
        "ECDHE-ECDSA-AES256-GCM-SHA384",
        "TLS_ECDHE_ECDSA_WITH_AES_256_GCM_SHA384",
    ),
    (
        "ECDHE-ECDSA-AES128-GCM-SHA256",
        "TLS_ECDHE_ECDSA_WITH_AES_128_GCM_SHA256",
    ),
    (
        "ECDHE-ECDSA-CHACHA20-POLY1305",
        "TLS_ECDHE_ECDSA_WITH_CHACHA20_POLY1305_SHA256",
    ),
    (
        "ECDHE-RSA-AES256-GCM-SHA384",
        "TLS_ECDHE_RSA_WITH_AES_256_GCM_SHA384",
    ),
    (
        "ECDHE-RSA-AES128-GCM-SHA256",
        "TLS_ECDHE_RSA_WITH_AES_128_GCM_SHA256",
    ),
    (
        "ECDHE-RSA-CHACHA20-POLY1305",
        "TLS_ECDHE_RSA_WITH_CHACHA20_POLY1305_SHA256",
    ),
];

pub fn canonical_cipher(name: &str) -> Option<&'static str> {
    let upper = name.trim().to_ascii_uppercase();
    OPENSSL_NAMES
        .iter()
        .find(|(a, b)| *a == upper || *b == upper)
        .map(|(_, b)| *b)
}

pub fn protocol_version(name: &str) -> Option<&'static SupportedProtocolVersion> {
    match name
        .trim()
        .to_ascii_lowercase()
        .replace(['v', '_', ' '], "")
        .as_str()
    {
        "tls1.2" | "tls12" => Some(&rustls::version::TLS12),
        "tls1.3" | "tls13" => Some(&rustls::version::TLS13),
        _ => None,
    }
}

pub fn provider(settings: &TlsSettings) -> Result<Arc<CryptoProvider>, String> {
    let mut provider = rustls::crypto::ring::default_provider();
    if !settings.ciphers.is_empty() {
        let wanted: Vec<&str> = settings
            .ciphers
            .iter()
            .filter_map(|c| canonical_cipher(c))
            .collect();
        let mut suites = Vec::new();
        for name in &wanted {
            if let Some(s) = provider
                .cipher_suites
                .iter()
                .find(|s| format!("{:?}", s.suite()) == *name)
            {
                if !suites.contains(s) {
                    suites.push(*s);
                }
            }
        }
        if suites.is_empty() {
            return Err("http.tls.ciphers: none of the listed suites is supported".into());
        }
        provider.cipher_suites = suites;
    }
    Ok(Arc::new(provider))
}

pub fn versions(settings: &TlsSettings) -> Result<Vec<&'static SupportedProtocolVersion>, String> {
    if settings.protocols.is_empty() {
        return Ok(rustls::DEFAULT_VERSIONS.to_vec());
    }
    let v: Vec<_> = settings
        .protocols
        .iter()
        .filter_map(|p| protocol_version(p))
        .collect();
    if v.is_empty() {
        return Err("http.tls.protocols: use TLSv1.2 and/or TLSv1.3".into());
    }
    Ok(v)
}

pub fn server_builder(
    settings: &TlsSettings,
) -> Result<rustls::ConfigBuilder<rustls::ServerConfig, rustls::WantsVerifier>, String> {
    rustls::ServerConfig::builder_with_provider(provider(settings)?)
        .with_protocol_versions(&versions(settings)?)
        .map_err(|e| format!("http.tls: {e}"))
}

pub fn check(settings: &TlsSettings) -> Result<(), String> {
    server_builder(settings)?;
    if let Some(f) = &settings.session_ticket_key {
        FixedTicketer::from_file(f)?;
    }
    Ok(())
}

pub fn apply_tickets(settings: &TlsSettings, config: &mut rustls::ServerConfig) {
    if !settings.session_tickets {
        config.session_storage = Arc::new(rustls::server::NoServerSessionStorage {});
        config.send_tls13_tickets = 0;
        return;
    }
    if let Some(f) = &settings.session_ticket_key {
        match FixedTicketer::from_file(f) {
            Ok(t) => {
                config.ticketer = Arc::new(t);
                return;
            }
            Err(e) => tracing::error!("session_ticket_key: {}", e),
        }
    }
    if let Ok(ticketer) = rustls::crypto::ring::Ticketer::new() {
        config.ticketer = ticketer;
    }
}

pub struct FixedTicketer {
    key: ring::aead::LessSafeKey,
    rng: ring::rand::SystemRandom,
}

impl std::fmt::Debug for FixedTicketer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("FixedTicketer")
    }
}

impl FixedTicketer {
    pub fn from_file(path: &str) -> Result<Self, String> {
        let raw = std::fs::read(path).map_err(|e| format!("{path}: {e}"))?;
        if raw.len() < 32 {
            return Err(format!(
                "{path}: the ticket key needs at least 32 random bytes (openssl rand 80 > {path})"
            ));
        }
        let digest = ring::digest::digest(&ring::digest::SHA256, &raw);
        let unbound = ring::aead::UnboundKey::new(&ring::aead::CHACHA20_POLY1305, digest.as_ref())
            .map_err(|_| format!("{path}: invalid key"))?;
        Ok(Self {
            key: ring::aead::LessSafeKey::new(unbound),
            rng: ring::rand::SystemRandom::new(),
        })
    }
}

impl ProducesTickets for FixedTicketer {
    fn enabled(&self) -> bool {
        true
    }

    fn lifetime(&self) -> u32 {
        43_200
    }

    fn encrypt(&self, plain: &[u8]) -> Option<Vec<u8>> {
        use ring::rand::SecureRandom;
        let mut nonce = [0u8; 12];
        self.rng.fill(&mut nonce).ok()?;
        let mut out = Vec::with_capacity(12 + plain.len() + 16);
        out.extend_from_slice(&nonce);
        out.extend_from_slice(plain);
        let tag = self
            .key
            .seal_in_place_separate_tag(
                ring::aead::Nonce::assume_unique_for_key(nonce),
                ring::aead::Aad::empty(),
                &mut out[12..],
            )
            .ok()?;
        out.extend_from_slice(tag.as_ref());
        Some(out)
    }

    fn decrypt(&self, cipher: &[u8]) -> Option<Vec<u8>> {
        if cipher.len() < 28 {
            return None;
        }
        let nonce: [u8; 12] = cipher[..12].try_into().ok()?;
        let mut buf = cipher[12..].to_vec();
        let plain = self
            .key
            .open_in_place(
                ring::aead::Nonce::assume_unique_for_key(nonce),
                ring::aead::Aad::empty(),
                &mut buf,
            )
            .ok()?;
        Some(plain.to_vec())
    }
}

pub fn private_key(
    key_path: &str,
    password_file: Option<&str>,
) -> Result<PrivateKeyDer<'static>, String> {
    let pem = std::fs::read(key_path).map_err(|e| format!("cannot read key {key_path}: {e}"))?;
    let text = String::from_utf8_lossy(&pem);
    if text.contains("BEGIN ENCRYPTED PRIVATE KEY") {
        return decrypt(key_path, &text, password_file);
    }
    if text.contains("Proc-Type: 4,ENCRYPTED") {
        return Err(format!(
            "{key_path}: legacy encrypted key; convert it with \
             `openssl pkcs8 -topk8 -v2 aes-256-cbc -in old.key -out new.key`"
        ));
    }
    rustls_pemfile::private_key(&mut pem.as_slice())
        .map_err(|e| format!("invalid key {key_path}: {e}"))?
        .ok_or_else(|| format!("no private key found in {key_path}"))
}

#[cfg(feature = "encrypted-keys")]
fn decrypt(
    key_path: &str,
    pem: &str,
    password_file: Option<&str>,
) -> Result<PrivateKeyDer<'static>, String> {
    use pkcs8::der::Decode;
    let file = password_file
        .ok_or_else(|| format!("{key_path} is encrypted: set tls.key_password_file"))?;
    let passwords = std::fs::read_to_string(file).map_err(|e| format!("{file}: {e}"))?;
    let (label, doc) =
        pkcs8::der::Document::from_pem(pem).map_err(|e| format!("{key_path}: {e}"))?;
    if label != "ENCRYPTED PRIVATE KEY" {
        return Err(format!("{key_path}: unexpected PEM block {label}"));
    }
    let info = pkcs8::EncryptedPrivateKeyInfo::from_der(doc.as_bytes())
        .map_err(|e| format!("{key_path}: {e}"))?;
    for pw in passwords.lines().filter(|l| !l.is_empty()) {
        if let Ok(plain) = info.decrypt(pw) {
            return Ok(PrivateKeyDer::Pkcs8(plain.as_bytes().to_vec().into()));
        }
    }
    Err(format!("{key_path}: no password in {file} opens this key"))
}

#[cfg(not(feature = "encrypted-keys"))]
fn decrypt(
    key_path: &str,
    _pem: &str,
    _password_file: Option<&str>,
) -> Result<PrivateKeyDer<'static>, String> {
    Err(format!(
        "{key_path} is encrypted and this build has no `encrypted-keys` feature"
    ))
}

pub fn crls(
    path: &str,
) -> Result<Vec<rustls::pki_types::CertificateRevocationListDer<'static>>, String> {
    let pem = std::fs::read(path).map_err(|e| format!("{path}: {e}"))?;
    let list = rustls_pemfile::crls(&mut pem.as_slice())
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| format!("{path}: {e}"))?;
    if list.is_empty() {
        return Err(format!("{path}: no CRL found"));
    }
    Ok(list)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ciphers_and_protocols() {
        assert_eq!(
            canonical_cipher("ecdhe-rsa-aes256-gcm-sha384"),
            Some("TLS_ECDHE_RSA_WITH_AES_256_GCM_SHA384")
        );
        assert_eq!(canonical_cipher("DES-CBC3-SHA"), None);
        let s = TlsSettings {
            protocols: vec!["TLSv1.3".into(), "TLSv1".into()],
            ciphers: vec!["TLS_AES_256_GCM_SHA384".into()],
            ..Default::default()
        };
        assert_eq!(versions(&s).unwrap().len(), 1);
        assert_eq!(provider(&s).unwrap().cipher_suites.len(), 1);
        assert!(server_builder(&s).is_ok());
        let bad = TlsSettings {
            ciphers: vec!["RC4".into()],
            ..Default::default()
        };
        assert!(check(&bad).is_err());
    }

    #[test]
    fn fixed_ticket_key_survives_a_restart() {
        let p = std::env::temp_dir().join(format!("ferrix-tk-{}", uuid::Uuid::new_v4()));
        std::fs::write(&p, [7u8; 80]).unwrap();
        let a = FixedTicketer::from_file(p.to_str().unwrap()).unwrap();
        let b = FixedTicketer::from_file(p.to_str().unwrap()).unwrap();
        let ticket = a.encrypt(b"session state").unwrap();
        assert_eq!(b.decrypt(&ticket).unwrap(), b"session state");
        assert!(b.decrypt(&ticket[..20]).is_none());
        std::fs::write(&p, [1u8; 8]).unwrap();
        assert!(FixedTicketer::from_file(p.to_str().unwrap()).is_err());
        let _ = std::fs::remove_file(p);
    }
}
