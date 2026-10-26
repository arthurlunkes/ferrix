//! Automatic TLS certificates over ACME (Let's Encrypt by default).
//!
//! Virtual hosts with `tls.acme: true` get a certificate for their exact `server_names`.
//! Ownership is proven with the HTTP-01 challenge, answered by the proxy itself on
//! `/.well-known/acme-challenge/<token>`, so port 80 must reach Ferrix. Certificates are
//! stored under `acme.storage_dir` and renewed in the background; the TLS listener picks
//! new ones up without a restart.

use instant_acme::{
    Account, AccountCredentials, AuthorizationStatus, ChallengeType, Identifier, NewAccount,
    NewOrder, OrderStatus, RetryPolicy,
};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock};
use std::time::Duration;
use tracing::{error, info, warn};

use crate::config::store::ConfigStore;
use crate::config::{AcmeConfig, VirtualHost};

pub const CHALLENGE_PREFIX: &str = "/.well-known/acme-challenge/";

/// Bumped whenever a certificate is written, so TLS listeners reload their tables.
pub static CERT_GENERATION: AtomicU64 = AtomicU64::new(0);

const CHECK_EVERY: Duration = Duration::from_secs(12 * 3600);
const RETRY_AFTER_FAILURE: Duration = Duration::from_secs(3600);

#[derive(Default)]
pub struct AcmeChallenges(RwLock<HashMap<String, String>>);

impl AcmeChallenges {
    pub fn key_authorization(&self, token: &str) -> Option<String> {
        self.0.read().ok()?.get(token).cloned()
    }

    fn insert(&self, token: String, key_auth: String) {
        if let Ok(mut m) = self.0.write() {
            m.insert(token, key_auth);
        }
    }

    fn remove(&self, token: &str) {
        if let Ok(mut m) = self.0.write() {
            m.remove(token);
        }
    }
}

/// Names that can be validated over HTTP-01: exact DNS names only.
pub fn domains_for(vhost: &VirtualHost) -> Vec<String> {
    let mut out: Vec<String> = vhost
        .server_names
        .iter()
        .map(|n| n.trim().trim_end_matches('.').to_ascii_lowercase())
        .filter(|n| {
            n.contains('.')
                && !n.starts_with('.')
                && !n.contains('*')
                && !n.starts_with('~')
                && n.parse::<std::net::IpAddr>().is_err()
                && n.chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '.')
        })
        .collect();
    out.sort();
    out.dedup();
    out
}

pub fn cert_paths(cfg: &AcmeConfig, vhost_id: &str) -> (PathBuf, PathBuf) {
    let safe: String = vhost_id
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    let dir = Path::new(&cfg.storage_dir).join(safe);
    (dir.join("fullchain.pem"), dir.join("privkey.pem"))
}

/// Seconds until the first certificate in `pem` expires, or `None` if unreadable.
fn seconds_left(pem: &[u8]) -> Option<i64> {
    let (_, parsed) = x509_parser::pem::parse_x509_pem(pem).ok()?;
    let cert = parsed.parse_x509().ok()?;
    let not_after = cert.validity().not_after.timestamp();
    Some(not_after - chrono::Utc::now().timestamp())
}

fn needs_certificate(cert: &Path, key: &Path, renew_before_days: u32) -> bool {
    if !key.is_file() {
        return true;
    }
    match std::fs::read(cert).ok().as_deref().and_then(seconds_left) {
        Some(left) => left < renew_before_days as i64 * 86_400,
        None => true,
    }
}

fn write_private(path: &Path, data: &[u8]) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, data)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600))?;
    }
    std::fs::rename(&tmp, path)
}

pub struct AcmeManager {
    store: Arc<ConfigStore>,
    challenges: Arc<AcmeChallenges>,
}

impl AcmeManager {
    pub fn new(store: Arc<ConfigStore>, challenges: Arc<AcmeChallenges>) -> Arc<Self> {
        Arc::new(Self { store, challenges })
    }

    pub fn start(self: Arc<Self>) {
        tokio::spawn(async move {
            // Give the plain-HTTP listeners a moment to bind before the CA calls back.
            tokio::time::sleep(Duration::from_secs(2)).await;
            loop {
                let ok = self.run_once().await;
                tokio::time::sleep(if ok { CHECK_EVERY } else { RETRY_AFTER_FAILURE }).await;
            }
        });
    }

    /// Issues or renews every certificate that needs it. Returns false if any failed.
    pub async fn run_once(&self) -> bool {
        let config = self.store.config();
        let acme = &config.acme;
        let pending: Vec<(&VirtualHost, Vec<String>, PathBuf, PathBuf)> = config
            .vhosts
            .iter()
            .filter(|v| v.tls.as_ref().is_some_and(|t| t.acme))
            .filter_map(|v| {
                let domains = domains_for(v);
                if domains.is_empty() {
                    return None;
                }
                let (cert, key) = cert_paths(acme, &v.id);
                needs_certificate(&cert, &key, acme.renew_before_days)
                    .then_some((v, domains, cert, key))
            })
            .collect();
        if pending.is_empty() {
            return true;
        }

        let account = match self.account(acme).await {
            Ok(a) => a,
            Err(e) => {
                error!("ACME account setup failed: {}", e);
                return false;
            }
        };

        let mut all_ok = true;
        for (vhost, domains, cert, key) in pending {
            info!(vhost = %vhost.name, domains = ?domains, "requesting ACME certificate");
            match self.issue(&account, &domains, &cert, &key).await {
                Ok(()) => {
                    CERT_GENERATION.fetch_add(1, Ordering::Release);
                    info!(vhost = %vhost.name, "ACME certificate installed");
                }
                Err(e) => {
                    all_ok = false;
                    error!(vhost = %vhost.name, "ACME certificate request failed: {}", e);
                }
            }
        }
        all_ok
    }

    async fn account(&self, cfg: &AcmeConfig) -> Result<Account, String> {
        // Credentials are bound to a directory, so keep one file per CA URL.
        let file = Path::new(&cfg.storage_dir).join(format!(
            "account-{:016x}.json",
            crate::http::upstream::hash::fnv1a(cfg.directory_url.as_bytes())
        ));
        if let Ok(data) = std::fs::read(&file) {
            match serde_json::from_slice::<AccountCredentials>(&data) {
                Ok(creds) => {
                    return Account::builder()
                        .map_err(|e| e.to_string())?
                        .from_credentials(creds)
                        .await
                        .map_err(|e| e.to_string());
                }
                Err(e) => warn!(
                    "ignoring unreadable ACME account file {}: {}",
                    file.display(),
                    e
                ),
            }
        }

        let contact = cfg.email.as_ref().map(|e| format!("mailto:{}", e));
        let contacts: Vec<&str> = contact.iter().map(String::as_str).collect();
        let (account, creds) = Account::builder()
            .map_err(|e| e.to_string())?
            .create(
                &NewAccount {
                    contact: &contacts,
                    terms_of_service_agreed: true,
                    only_return_existing: false,
                },
                cfg.directory_url.clone(),
                None,
            )
            .await
            .map_err(|e| e.to_string())?;
        let json = serde_json::to_vec_pretty(&creds).map_err(|e| e.to_string())?;
        write_private(&file, &json).map_err(|e| format!("saving {}: {}", file.display(), e))?;
        Ok(account)
    }

    async fn issue(
        &self,
        account: &Account,
        domains: &[String],
        cert_path: &Path,
        key_path: &Path,
    ) -> Result<(), String> {
        let identifiers: Vec<Identifier> =
            domains.iter().map(|d| Identifier::Dns(d.clone())).collect();
        let mut order = account
            .new_order(&NewOrder::new(&identifiers))
            .await
            .map_err(|e| e.to_string())?;

        let mut tokens = Vec::new();
        let result = async {
            let mut authorizations = order.authorizations();
            while let Some(authz) = authorizations.next().await {
                let mut authz = authz.map_err(|e| e.to_string())?;
                match authz.status {
                    AuthorizationStatus::Valid => continue,
                    AuthorizationStatus::Pending => {}
                    other => return Err(format!("authorization is {:?}", other)),
                }
                let mut challenge = authz
                    .challenge(ChallengeType::Http01)
                    .ok_or("the CA offered no http-01 challenge")?;
                let token = challenge.token.clone();
                self.challenges.insert(
                    token.clone(),
                    challenge.key_authorization().as_str().to_string(),
                );
                tokens.push(token);
                challenge.set_ready().await.map_err(|e| e.to_string())?;
            }

            let status = order
                .poll_ready(&RetryPolicy::default())
                .await
                .map_err(|e| e.to_string())?;
            if status != OrderStatus::Ready {
                return Err(format!("order ended as {:?}", status));
            }
            let key_pem = order.finalize().await.map_err(|e| e.to_string())?;
            let chain_pem = order
                .poll_certificate(&RetryPolicy::default())
                .await
                .map_err(|e| e.to_string())?;
            Ok((key_pem, chain_pem))
        }
        .await;

        for t in &tokens {
            self.challenges.remove(t);
        }
        let (key_pem, chain_pem) = result?;

        write_private(key_path, key_pem.as_bytes())
            .map_err(|e| format!("saving {}: {}", key_path.display(), e))?;
        write_private(cert_path, chain_pem.as_bytes())
            .map_err(|e| format!("saving {}: {}", cert_path.display(), e))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vhost(names: &[&str]) -> VirtualHost {
        VirtualHost {
            id: "site/1".into(),
            name: "site".into(),
            server_names: names.iter().map(|s| s.to_string()).collect(),
            listen_port: 443,
            headers: Default::default(),
            rate_limit: None,
            limit_conn: None,
            allowed_ips: vec![],
            denied_ips: vec![],
            basic_auth: None,
            satisfy: Default::default(),
            tls: None,
            routes: vec![],
            error_pages: Default::default(),
        }
    }

    #[test]
    fn only_exact_dns_names_are_requested() {
        let v = vhost(&[
            "Example.com",
            "www.example.com",
            "*.example.com",
            "_",
            "localhost",
            "10.0.0.1",
            "~^api",
        ]);
        assert_eq!(domains_for(&v), vec!["example.com", "www.example.com"]);
    }

    #[test]
    fn storage_paths_are_sanitized() {
        let (cert, key) = cert_paths(&AcmeConfig::default(), "site/1");
        assert!(cert.ends_with("site_1/fullchain.pem"));
        assert!(key.ends_with("site_1/privkey.pem"));
    }

    #[test]
    fn missing_certificate_needs_issuing() {
        let dir = std::env::temp_dir().join("ferrix-acme-none");
        assert!(needs_certificate(
            &dir.join("c.pem"),
            &dir.join("k.pem"),
            30
        ));
    }

    #[test]
    fn challenges_round_trip() {
        let c = AcmeChallenges::default();
        c.insert("tok".into(), "tok.thumb".into());
        assert_eq!(c.key_authorization("tok").as_deref(), Some("tok.thumb"));
        c.remove("tok");
        assert!(c.key_authorization("tok").is_none());
    }
}
