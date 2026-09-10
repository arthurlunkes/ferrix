//! Descoberta do IP real do cliente quando o Ferrix está atrás de outro proxy.
//!
//! Antes disto o IP vinha sempre do socket, então atrás de um load balancer o rate limit e
//! o ACL agiam sobre o endereço do balanceador — o mesmo para todo mundo.
//!
//! A regra que sustenta a segurança do módulo: um header de encaminhamento só é levado em
//! conta se a conexão veio de um proxy declarado como confiável. Sem essa checagem
//! qualquer cliente mandaria `X-Forwarded-For: 1.2.3.4` e escaparia de qualquer limite.

use axum::http::HeaderMap;
use std::net::IpAddr;
use std::str::FromStr;

use crate::config::{RealIpConfig, RealIpHeader};
use crate::net::cidr::{normalize, IpMatcher};

#[derive(Debug, Clone, Default)]
pub struct RealIpResolver {
    trusted: IpMatcher,
    header: RealIpHeader,
    recursive: bool,
}

impl RealIpResolver {
    pub fn new(cfg: &RealIpConfig) -> Self {
        let (trusted, _errors) = IpMatcher::compile(&cfg.trusted_proxies);
        Self {
            trusted,
            header: cfg.header,
            recursive: cfg.recursive,
        }
    }

    /// Decide qual endereço representa o cliente.
    ///
    /// Devolve o endereço do socket sempre que não houver base para confiar em outra coisa.
    pub fn resolve(&self, peer: IpAddr, headers: &HeaderMap) -> IpAddr {
        let peer = normalize(peer);

        if self.header == RealIpHeader::None {
            return peer;
        }
        // O peer não é um proxy conhecido: qualquer header que ele mande é palpite dele.
        if !self.trusted.matches(peer) {
            return peer;
        }

        match self.header {
            RealIpHeader::None => peer,
            RealIpHeader::XRealIp => headers
                .get("x-real-ip")
                .and_then(|v| v.to_str().ok())
                .and_then(|s| IpAddr::from_str(s.trim()).ok())
                .map(normalize)
                .unwrap_or(peer),
            RealIpHeader::XForwardedFor => {
                let chain = forwarded_chain(headers);
                if chain.is_empty() {
                    return peer;
                }
                if self.recursive {
                    // Anda da direita para a esquerda até sair da malha de proxies
                    // confiáveis. O primeiro endereço não-confiável é o cliente.
                    chain
                        .iter()
                        .rev()
                        .find(|ip| !self.trusted.matches(**ip))
                        .copied()
                        // Cadeia inteira confiável: o cliente é quem a iniciou.
                        .unwrap_or_else(|| chain[0])
                } else {
                    // Sem busca recursiva vale o último endereço da cadeia, que é o que o
                    // proxy imediatamente anterior anexou.
                    chain[chain.len() - 1]
                }
            }
        }
    }

    /// Monta o valor de `$proxy_add_x_forwarded_for`: a cadeia que chegou, com o endereço
    /// do cliente anexado ao fim.
    ///
    /// O código anterior devolvia apenas o IP do cliente, descartando a cadeia — o que
    /// apagava a origem real a cada salto.
    pub fn proxy_add_x_forwarded_for(headers: &HeaderMap, client_ip: &str) -> String {
        let existing: Vec<String> = headers
            .get_all("x-forwarded-for")
            .iter()
            .filter_map(|v| v.to_str().ok())
            .flat_map(|v| v.split(','))
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string())
            .collect();

        if existing.is_empty() {
            client_ip.to_string()
        } else {
            format!("{}, {}", existing.join(", "), client_ip)
        }
    }
}

/// Lê `X-Forwarded-For` como uma cadeia ordenada do cliente original ao proxy mais recente.
/// Entradas que não são endereços válidos são descartadas.
fn forwarded_chain(headers: &HeaderMap) -> Vec<IpAddr> {
    headers
        .get_all("x-forwarded-for")
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(','))
        .filter_map(|s| IpAddr::from_str(s.trim()).ok())
        .map(normalize)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ip(s: &str) -> IpAddr {
        IpAddr::from_str(s).unwrap()
    }

    fn headers(pairs: &[(&str, &str)]) -> HeaderMap {
        let mut h = HeaderMap::new();
        for (k, v) in pairs {
            h.append(
                axum::http::HeaderName::from_bytes(k.as_bytes()).unwrap(),
                v.parse().unwrap(),
            );
        }
        h
    }

    fn resolver(trusted: &[&str], header: RealIpHeader, recursive: bool) -> RealIpResolver {
        RealIpResolver::new(&RealIpConfig {
            trusted_proxies: trusted.iter().map(|s| s.to_string()).collect(),
            header,
            recursive,
        })
    }

    #[test]
    fn default_config_always_uses_socket_address() {
        let r = RealIpResolver::new(&RealIpConfig::default());
        let h = headers(&[("x-forwarded-for", "1.2.3.4")]);
        assert_eq!(r.resolve(ip("10.0.0.1"), &h), ip("10.0.0.1"));
    }

    #[test]
    fn header_from_untrusted_peer_is_ignored() {
        // O ponto central do módulo: sem isto, qualquer cliente forja o próprio IP.
        let r = resolver(&["10.0.0.0/8"], RealIpHeader::XForwardedFor, false);
        let h = headers(&[("x-forwarded-for", "1.2.3.4")]);
        assert_eq!(r.resolve(ip("203.0.113.9"), &h), ip("203.0.113.9"));
    }

    #[test]
    fn header_from_trusted_peer_is_honoured() {
        let r = resolver(&["10.0.0.0/8"], RealIpHeader::XForwardedFor, false);
        let h = headers(&[("x-forwarded-for", "1.2.3.4")]);
        assert_eq!(r.resolve(ip("10.0.0.1"), &h), ip("1.2.3.4"));
    }

    #[test]
    fn non_recursive_takes_last_entry_of_chain() {
        let r = resolver(&["10.0.0.0/8"], RealIpHeader::XForwardedFor, false);
        let h = headers(&[("x-forwarded-for", "1.2.3.4, 10.0.0.5")]);
        assert_eq!(r.resolve(ip("10.0.0.1"), &h), ip("10.0.0.5"));
    }

    #[test]
    fn recursive_skips_trusted_proxies_from_the_right() {
        let r = resolver(&["10.0.0.0/8"], RealIpHeader::XForwardedFor, true);
        let h = headers(&[("x-forwarded-for", "1.2.3.4, 10.0.0.5, 10.0.0.6")]);
        assert_eq!(r.resolve(ip("10.0.0.1"), &h), ip("1.2.3.4"));
    }

    #[test]
    fn recursive_with_fully_trusted_chain_uses_the_first_entry() {
        let r = resolver(&["10.0.0.0/8"], RealIpHeader::XForwardedFor, true);
        let h = headers(&[("x-forwarded-for", "10.0.0.4, 10.0.0.5")]);
        assert_eq!(r.resolve(ip("10.0.0.1"), &h), ip("10.0.0.4"));
    }

    #[test]
    fn chain_split_across_repeated_headers_is_joined() {
        let r = resolver(&["10.0.0.0/8"], RealIpHeader::XForwardedFor, true);
        let h = headers(&[
            ("x-forwarded-for", "1.2.3.4"),
            ("x-forwarded-for", "10.0.0.5"),
        ]);
        assert_eq!(r.resolve(ip("10.0.0.1"), &h), ip("1.2.3.4"));
    }

    #[test]
    fn garbage_in_chain_is_discarded() {
        let r = resolver(&["10.0.0.0/8"], RealIpHeader::XForwardedFor, false);
        let h = headers(&[("x-forwarded-for", "unknown, 1.2.3.4")]);
        assert_eq!(r.resolve(ip("10.0.0.1"), &h), ip("1.2.3.4"));
    }

    /// Um header com bytes fora do ASCII visível é descartado por inteiro, e o endereço do
    /// socket prevalece. Recusar o header suspeito é o lado seguro.
    #[test]
    fn non_ascii_header_is_dropped_entirely() {
        let r = resolver(&["10.0.0.0/8"], RealIpHeader::XForwardedFor, false);
        let h = headers(&[("x-forwarded-for", "não-é-ip, 1.2.3.4")]);
        assert_eq!(r.resolve(ip("10.0.0.1"), &h), ip("10.0.0.1"));
    }

    #[test]
    fn empty_chain_falls_back_to_peer() {
        let r = resolver(&["10.0.0.0/8"], RealIpHeader::XForwardedFor, false);
        let h = headers(&[("x-forwarded-for", "  ")]);
        assert_eq!(r.resolve(ip("10.0.0.1"), &h), ip("10.0.0.1"));
    }

    #[test]
    fn x_real_ip_mode_reads_that_header() {
        let r = resolver(&["10.0.0.0/8"], RealIpHeader::XRealIp, false);
        let h = headers(&[("x-real-ip", "1.2.3.4"), ("x-forwarded-for", "9.9.9.9")]);
        assert_eq!(r.resolve(ip("10.0.0.1"), &h), ip("1.2.3.4"));
    }

    #[test]
    fn x_real_ip_falls_back_when_unparseable() {
        let r = resolver(&["10.0.0.0/8"], RealIpHeader::XRealIp, false);
        let h = headers(&[("x-real-ip", "lixo")]);
        assert_eq!(r.resolve(ip("10.0.0.1"), &h), ip("10.0.0.1"));
    }

    #[test]
    fn ipv4_mapped_peer_matches_ipv4_trusted_block() {
        let r = resolver(&["10.0.0.0/8"], RealIpHeader::XForwardedFor, false);
        let h = headers(&[("x-forwarded-for", "1.2.3.4")]);
        assert_eq!(r.resolve(ip("::ffff:10.0.0.1"), &h), ip("1.2.3.4"));
    }

    #[test]
    fn proxy_add_xff_appends_to_existing_chain() {
        // O comportamento anterior descartava a cadeia e mandava só o IP do cliente.
        let h = headers(&[("x-forwarded-for", "1.2.3.4, 10.0.0.5")]);
        assert_eq!(
            RealIpResolver::proxy_add_x_forwarded_for(&h, "10.0.0.1"),
            "1.2.3.4, 10.0.0.5, 10.0.0.1"
        );
    }

    #[test]
    fn proxy_add_xff_without_existing_header_is_just_the_client() {
        let h = HeaderMap::new();
        assert_eq!(
            RealIpResolver::proxy_add_x_forwarded_for(&h, "10.0.0.1"),
            "10.0.0.1"
        );
    }
}
