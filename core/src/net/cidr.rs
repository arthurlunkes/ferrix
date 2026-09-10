//! Matching de endereços IP contra listas de literais e blocos CIDR.
//!
//! Substitui a comparação por string exata que existia no controle de acesso, onde
//! `172.31.0.0/16` nunca casava com nenhum cliente. Aceita as duas formas que aparecem em
//! configuração de nginx: IP literal (`10.0.0.1`) e bloco (`10.0.0.0/8`), em IPv4 e IPv6.

use ipnet::IpNet;
use std::net::IpAddr;
use std::str::FromStr;

/// Uma entrada de lista de acesso já parseada.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IpRule {
    /// `all` ou `*` — casa com qualquer endereço.
    Any,
    /// Um endereço isolado.
    Addr(IpAddr),
    /// Um bloco CIDR.
    Net(IpNet),
}

impl IpRule {
    /// Interpreta uma entrada de configuração.
    ///
    /// Um IP sem máscara vira `Addr` em vez de um `/32`, porque a comparação direta é mais
    /// barata que a de rede e este caminho roda a cada requisição.
    pub fn parse(raw: &str) -> Result<Self, String> {
        let s = raw.trim();
        if s.is_empty() {
            return Err("entrada de IP vazia".to_string());
        }
        if s == "*" || s.eq_ignore_ascii_case("all") {
            return Ok(IpRule::Any);
        }
        if s.contains('/') {
            return IpNet::from_str(s)
                .map(IpRule::Net)
                .map_err(|e| format!("bloco CIDR inválido '{}': {}", s, e));
        }
        IpAddr::from_str(s)
            .map(IpRule::Addr)
            .map_err(|e| format!("endereço IP inválido '{}': {}", s, e))
    }

    pub fn matches(&self, ip: IpAddr) -> bool {
        match self {
            IpRule::Any => true,
            IpRule::Addr(a) => same_addr(*a, ip),
            IpRule::Net(n) => {
                // Testa as duas representações: uma regra escrita em IPv4 deve casar com um
                // cliente que chegou como IPv4-mapeado, e vice-versa.
                n.contains(&ip)
                    || match (n, normalize(ip)) {
                        (IpNet::V4(_), other) if other != ip => n.contains(&other),
                        _ => false,
                    }
            }
        }
    }
}

/// Uma lista de regras compilada uma vez, para não reparsear a cada requisição.
#[derive(Debug, Clone, Default)]
pub struct IpMatcher {
    rules: Vec<IpRule>,
}

impl IpMatcher {
    /// Compila a lista, descartando entradas inválidas e devolvendo-as junto.
    ///
    /// Descartar em vez de falhar mantém o proxy no ar quando uma entrada está torta; quem
    /// precisa recusar a configuração inteira usa os erros devolvidos (é o que a validação
    /// de config faz).
    pub fn compile(entries: &[String]) -> (Self, Vec<String>) {
        let mut rules = Vec::with_capacity(entries.len());
        let mut errors = Vec::new();
        for entry in entries {
            match IpRule::parse(entry) {
                Ok(rule) => rules.push(rule),
                Err(e) => errors.push(e),
            }
        }
        (Self { rules }, errors)
    }

    pub fn is_empty(&self) -> bool {
        self.rules.is_empty()
    }

    pub fn matches(&self, ip: IpAddr) -> bool {
        self.rules.iter().any(|r| r.matches(ip))
    }
}

/// Reduz um IPv6 que carrega um IPv4 (`::ffff:10.0.0.1`) ao IPv4 correspondente.
///
/// Um socket dual-stack entrega clientes IPv4 nessa forma mapeada. Sem isto, uma regra
/// `allow 10.0.0.0/8` deixaria de fora exatamente os clientes que deveria aceitar.
pub fn normalize(ip: IpAddr) -> IpAddr {
    match ip {
        IpAddr::V6(v6) => match v6.to_ipv4_mapped() {
            Some(v4) => IpAddr::V4(v4),
            None => ip,
        },
        v4 => v4,
    }
}

fn same_addr(a: IpAddr, b: IpAddr) -> bool {
    a == b || normalize(a) == normalize(b)
}

/// Verdadeiro para endereços de loopback, considerando a forma IPv4-mapeada.
pub fn is_loopback(ip: IpAddr) -> bool {
    match normalize(ip) {
        IpAddr::V4(v4) => v4.is_loopback(),
        IpAddr::V6(v6) => v6.is_loopback(),
    }
}

/// Verdadeiro se o host de um endereço de bind atende apenas na máquina local.
///
/// Usado para decidir se a API de administração pode subir sem autenticação.
/// Um host que não parseia como IP (um nome, por exemplo) é tratado como não-loopback:
/// na dúvida, o lado seguro é exigir autenticação.
pub fn listen_addr_is_loopback(listen: &str) -> bool {
    let host = match listen.rsplit_once(':') {
        Some((h, _port)) => h,
        None => listen,
    };
    let host = host.trim().trim_start_matches('[').trim_end_matches(']');
    if host.is_empty() {
        return false;
    }
    match IpAddr::from_str(host) {
        Ok(ip) => is_loopback(ip),
        Err(_) => host.eq_ignore_ascii_case("localhost"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ip(s: &str) -> IpAddr {
        IpAddr::from_str(s).unwrap()
    }

    #[test]
    fn literal_ip_matches_only_itself() {
        let rule = IpRule::parse("10.0.0.1").unwrap();
        assert!(rule.matches(ip("10.0.0.1")));
        assert!(!rule.matches(ip("10.0.0.2")));
    }

    #[test]
    fn cidr_block_matches_range() {
        // O caso que motivou o módulo: a comparação por string nunca casava aqui.
        let rule = IpRule::parse("172.31.0.0/16").unwrap();
        assert!(rule.matches(ip("172.31.0.1")));
        assert!(rule.matches(ip("172.31.255.254")));
        assert!(!rule.matches(ip("172.30.0.1")));
        assert!(!rule.matches(ip("172.32.0.1")));
    }

    #[test]
    fn cidr_boundaries_are_exact() {
        let rule = IpRule::parse("192.168.1.0/24").unwrap();
        assert!(rule.matches(ip("192.168.1.0")));
        assert!(rule.matches(ip("192.168.1.255")));
        assert!(!rule.matches(ip("192.168.0.255")));
        assert!(!rule.matches(ip("192.168.2.0")));
    }

    #[test]
    fn slash_zero_matches_everything_in_family() {
        let rule = IpRule::parse("0.0.0.0/0").unwrap();
        assert!(rule.matches(ip("1.2.3.4")));
        assert!(rule.matches(ip("255.255.255.255")));
    }

    #[test]
    fn ipv6_literal_and_block() {
        assert!(IpRule::parse("::1").unwrap().matches(ip("::1")));
        let rule = IpRule::parse("2001:db8::/32").unwrap();
        assert!(rule.matches(ip("2001:db8::1")));
        assert!(!rule.matches(ip("2001:db9::1")));
    }

    #[test]
    fn wildcard_tokens_match_all() {
        for token in ["*", "all", "ALL"] {
            let rule = IpRule::parse(token).unwrap();
            assert!(rule.matches(ip("8.8.8.8")), "token {} deveria casar", token);
            assert!(rule.matches(ip("::1")));
        }
    }

    #[test]
    fn ipv4_mapped_client_matches_ipv4_rule() {
        // Um socket dual-stack entrega o cliente IPv4 nesta forma.
        let mapped = ip("::ffff:10.0.0.7");
        assert!(IpRule::parse("10.0.0.0/8").unwrap().matches(mapped));
        assert!(IpRule::parse("10.0.0.7").unwrap().matches(mapped));
    }

    #[test]
    fn invalid_entries_are_rejected_with_reason() {
        for bad in ["", "não-é-ip", "10.0.0.0/99", "10.0.0.256", "10.0.0.0/"] {
            assert!(IpRule::parse(bad).is_err(), "{:?} deveria falhar", bad);
        }
    }

    #[test]
    fn matcher_collects_errors_and_keeps_valid_rules() {
        let entries = vec![
            "10.0.0.0/8".to_string(),
            "lixo".to_string(),
            "192.168.0.1".to_string(),
        ];
        let (matcher, errors) = IpMatcher::compile(&entries);
        assert_eq!(errors.len(), 1);
        assert!(matcher.matches(ip("10.1.2.3")));
        assert!(matcher.matches(ip("192.168.0.1")));
        assert!(!matcher.matches(ip("8.8.8.8")));
    }

    #[test]
    fn empty_matcher_matches_nothing() {
        let (matcher, errors) = IpMatcher::compile(&[]);
        assert!(errors.is_empty());
        assert!(matcher.is_empty());
        assert!(!matcher.matches(ip("10.0.0.1")));
    }

    #[test]
    fn loopback_detection_covers_mapped_form() {
        assert!(is_loopback(ip("127.0.0.1")));
        assert!(is_loopback(ip("::1")));
        assert!(is_loopback(ip("::ffff:127.0.0.1")));
        assert!(!is_loopback(ip("10.0.0.1")));
    }

    #[test]
    fn listen_addr_loopback_detection() {
        assert!(listen_addr_is_loopback("127.0.0.1:8080"));
        assert!(listen_addr_is_loopback("localhost:8080"));
        assert!(listen_addr_is_loopback("[::1]:8080"));
        assert!(!listen_addr_is_loopback("0.0.0.0:8080"));
        assert!(!listen_addr_is_loopback("192.168.1.10:8080"));
        // Um nome desconhecido conta como exposto: na dúvida, exigir autenticação.
        assert!(!listen_addr_is_loopback("meu-servidor:8080"));
    }
}
