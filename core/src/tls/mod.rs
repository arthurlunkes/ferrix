//! Decide, a partir da configuração, quantos listeners o Ferrix precisa abrir e quais
//! deles terminam TLS.
//!
//! Antes disto o proxy só ouvia uma porta (`http.listen`) para todo o tráfego, e
//! `vhost.listen_port` era só um rótulo usado no casamento de host — nada de fato ouvia
//! ali. Um domínio real quase sempre precisa de mais de uma porta (o próprio HTTPS já
//! implica `80` para redirecionar e `443` para servir), então cada porta distinta
//! declarada nos vhosts agora recebe seu próprio listener.

use std::collections::BTreeMap;

use crate::config::FerrixConfig;

/// O que um listener de uma porta precisa saber para subir.
#[derive(Debug, Clone)]
pub struct PortPlan {
    pub port: u16,
    /// `Some` quando ao menos um vhost desta porta declara `tls` — o listener sobe em
    /// HTTPS, usando o certificado deste vhost.
    ///
    /// Quando mais de um vhost com `tls` compartilha a porta, hoje só o primeiro
    /// declarado é servido (sem SNI multi-certificado ainda) — `validate` avisa sobre
    /// isso, para não ficar surpresa silenciosa.
    pub tls: Option<TlsSource>,
}

#[derive(Debug, Clone)]
pub struct TlsSource {
    pub cert_path: String,
    pub key_path: String,
    /// Nome do vhost cujo certificado foi escolhido — só para a mensagem de log.
    pub vhost_name: String,
}

/// Monta o plano de listeners: uma entrada por porta distinta entre `http.listen` e o
/// `listen_port` de cada vhost, na ordem das portas (determinística, para os logs de
/// inicialização saírem sempre na mesma ordem).
pub fn plan_ports(config: &FerrixConfig) -> Vec<PortPlan> {
    let mut by_port: BTreeMap<u16, Option<TlsSource>> = BTreeMap::new();

    if let Some((_, p)) = config.http.listen.rsplit_once(':') {
        if let Ok(port) = p.parse::<u16>() {
            by_port.entry(port).or_insert(None);
        }
    }

    for vhost in &config.vhosts {
        let entry = by_port.entry(vhost.listen_port).or_insert(None);
        if entry.is_none() {
            if let Some(tls) = &vhost.tls {
                *entry = Some(TlsSource {
                    cert_path: tls.cert_path.clone(),
                    key_path: tls.key_path.clone(),
                    vhost_name: vhost.name.clone(),
                });
            }
        }
    }

    by_port
        .into_iter()
        .map(|(port, tls)| PortPlan { port, tls })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Satisfy, TlsConfig, VirtualHost};

    fn vhost(port: u16, tls: Option<TlsConfig>) -> VirtualHost {
        VirtualHost {
            id: format!("v{}", port),
            name: format!("vhost-{}", port),
            server_names: vec!["exemplo.com".to_string()],
            listen_port: port,
            headers: Default::default(),
            rate_limit: None,
            limit_conn: None,
            allowed_ips: vec![],
            denied_ips: vec![],
            basic_auth: None,
            satisfy: Satisfy::All,
            tls,
            routes: vec![],
        }
    }

    #[test]
    fn single_vhost_matches_http_listen_yields_one_plain_port() {
        let cfg = FerrixConfig {
            vhosts: vec![vhost(8000, None)],
            ..Default::default()
        };
        let plan = plan_ports(&cfg);
        assert_eq!(plan.len(), 1);
        assert_eq!(plan[0].port, 8000);
        assert!(plan[0].tls.is_none());
    }

    #[test]
    fn distinct_ports_across_vhosts_each_get_an_entry() {
        let mut cfg = FerrixConfig::default();
        cfg.http.listen = "0.0.0.0:8000".to_string();
        cfg.vhosts = vec![vhost(443, None), vhost(4070, None), vhost(80, None)];

        let ports: Vec<u16> = plan_ports(&cfg).iter().map(|p| p.port).collect();
        assert_eq!(ports, vec![80, 443, 4070, 8000]);
    }

    #[test]
    fn a_vhost_with_tls_marks_its_port_for_https() {
        let mut cfg = FerrixConfig::default();
        cfg.vhosts = vec![vhost(
            443,
            Some(TlsConfig {
                cert_path: "/c.pem".into(),
                key_path: "/k.pem".into(),
            }),
        )];

        let plan = plan_ports(&cfg);
        let port_443 = plan.iter().find(|p| p.port == 443).unwrap();
        let tls = port_443.tls.as_ref().expect("deveria ter TLS");
        assert_eq!(tls.cert_path, "/c.pem");
        assert_eq!(tls.key_path, "/k.pem");
    }

    #[test]
    fn first_tls_vhost_on_a_shared_port_wins() {
        let mut cfg = FerrixConfig::default();
        cfg.vhosts = vec![
            vhost(
                443,
                Some(TlsConfig {
                    cert_path: "/primeiro.pem".into(),
                    key_path: "/primeiro-key.pem".into(),
                }),
            ),
            vhost(
                443,
                Some(TlsConfig {
                    cert_path: "/segundo.pem".into(),
                    key_path: "/segundo-key.pem".into(),
                }),
            ),
        ];

        let plan = plan_ports(&cfg);
        let port_443 = plan.iter().find(|p| p.port == 443).unwrap();
        assert_eq!(port_443.tls.as_ref().unwrap().cert_path, "/primeiro.pem");
    }

    #[test]
    fn plain_vhost_sharing_a_tls_port_does_not_downgrade_it() {
        // Se o primeiro vhost visto na porta não tiver `tls` mas outro tiver, a porta
        // ainda deve subir em HTTPS — a ordem de declaração não deveria importar aqui.
        let mut cfg = FerrixConfig::default();
        cfg.vhosts = vec![
            vhost(443, None),
            vhost(
                443,
                Some(TlsConfig {
                    cert_path: "/c.pem".into(),
                    key_path: "/k.pem".into(),
                }),
            ),
        ];

        let plan = plan_ports(&cfg);
        let port_443 = plan.iter().find(|p| p.port == 443).unwrap();
        assert!(port_443.tls.is_some(), "a porta deveria ter sido promovida a TLS");
    }
}
