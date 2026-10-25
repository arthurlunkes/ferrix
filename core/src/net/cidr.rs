use ipnet::IpNet;
use std::net::IpAddr;
use std::str::FromStr;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IpRule {
    Any,

    Addr(IpAddr),

    Net(IpNet),
}

impl IpRule {
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
                .map_err(|e| format!("Invalid CIDR block '{}': {}", s, e));
        }
        IpAddr::from_str(s)
            .map(IpRule::Addr)
            .map_err(|e| format!("Invalid IP address '{}': {}", s, e))
    }

    pub fn matches(&self, ip: IpAddr) -> bool {
        match self {
            IpRule::Any => true,
            IpRule::Addr(a) => same_addr(*a, ip),
            IpRule::Net(n) => {
                n.contains(&ip)
                    || match (n, normalize(ip)) {
                        (IpNet::V4(_), other) if other != ip => n.contains(&other),
                        _ => false,
                    }
            }
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct IpMatcher {
    rules: Vec<IpRule>,
}

impl IpMatcher {
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

pub fn is_loopback(ip: IpAddr) -> bool {
    match normalize(ip) {
        IpAddr::V4(v4) => v4.is_loopback(),
        IpAddr::V6(v6) => v6.is_loopback(),
    }
}

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

        assert!(!listen_addr_is_loopback("meu-servidor:8080"));
    }
}
