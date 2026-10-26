use axum::http::{HeaderName, HeaderValue};
use regex::Regex;
use std::collections::HashMap;
use std::sync::Arc;

use crate::config::{
    validate::compile_route_regex, FerrixConfig, LoadBalanceAlgorithm, RouteMatchType, RouteRule,
    VirtualHost,
};
use crate::http::realip::RealIpResolver;
use crate::http::upstream::hash::HashRing;
use crate::net::cidr::IpMatcher;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RouteMatchReason {
    Exact,

    PrefixPriority,

    Regex,

    LongestPrefix,
}

impl RouteMatchReason {
    pub fn as_str(&self) -> &'static str {
        match self {
            RouteMatchReason::Exact => "match exato",
            RouteMatchReason::PrefixPriority => "prefixo prioritário (^~)",
            RouteMatchReason::Regex => "expressão regular",
            RouteMatchReason::LongestPrefix => "prefixo mais longo",
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct RouteMatch {
    pub route_index: usize,
    pub reason: RouteMatchReason,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VHostMatchReason {
    Exact,
    WildcardPrefix,
    WildcardSuffix,
    Regex,
    CatchAll,

    Fallback,
}

impl VHostMatchReason {
    pub fn as_str(&self) -> &'static str {
        match self {
            VHostMatchReason::Exact => "nome exato",
            VHostMatchReason::WildcardPrefix => "curinga *.dominio",
            VHostMatchReason::WildcardSuffix => "curinga nome.*",
            VHostMatchReason::Regex => "expressão regular",
            VHostMatchReason::CatchAll => "curinga total",
            VHostMatchReason::Fallback => "fallback para o primeiro host",
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct VHostMatch {
    pub vhost_index: usize,
    pub reason: VHostMatchReason,
}

#[derive(Debug, Clone)]
enum ServerNamePattern {
    Exact(String),

    WildcardPrefix(String),

    WildcardSuffix(String),
    Regex(Regex),

    CatchAll,
}

impl ServerNamePattern {
    fn parse(raw: &str) -> Option<Self> {
        let s = raw.trim();
        if s.is_empty() {
            return None;
        }
        if s == "*" || s == "_" {
            return Some(ServerNamePattern::CatchAll);
        }
        if let Some(pattern) = s.strip_prefix('~') {
            return Regex::new(pattern).ok().map(ServerNamePattern::Regex);
        }
        if let Some(rest) = s.strip_prefix("*.") {
            return Some(ServerNamePattern::WildcardPrefix(format!(
                ".{}",
                rest.to_ascii_lowercase()
            )));
        }
        if let Some(rest) = s.strip_suffix(".*") {
            return Some(ServerNamePattern::WildcardSuffix(format!(
                "{}.",
                rest.to_ascii_lowercase()
            )));
        }
        Some(ServerNamePattern::Exact(s.to_ascii_lowercase()))
    }
}

fn compile_response_headers(
    vhost: &VirtualHost,
    route: &RouteRule,
) -> Vec<(HeaderName, HeaderValue)> {
    let mut merged: Vec<(HeaderName, HeaderValue)> = Vec::new();
    for (k, v) in vhost.headers.iter().chain(route.add_headers.iter()) {
        let (Ok(name), Ok(value)) = (
            HeaderName::from_bytes(k.as_bytes()),
            HeaderValue::from_str(v),
        ) else {
            continue;
        };
        match merged.iter_mut().find(|(n, _)| *n == name) {
            Some(slot) => slot.1 = value,
            None => merged.push((name, value)),
        }
    }
    merged
}

#[derive(Debug)]
struct CompiledRoute {
    match_type: RouteMatchType,

    pattern: String,
    regex: Option<Regex>,
    allowed_ips: IpMatcher,
    denied_ips: IpMatcher,

    response_headers: Vec<(HeaderName, HeaderValue)>,
}

impl CompiledRoute {
    fn build(vhost: &VirtualHost, route: &RouteRule) -> Self {
        let regex = if route.match_type.is_regex() {
            compile_route_regex(route.match_type, &route.path_prefix).ok()
        } else {
            None
        };
        Self {
            match_type: route.match_type,
            pattern: normalize_prefix(&route.path_prefix),
            regex,
            allowed_ips: IpMatcher::compile(&route.allowed_ips).0,
            denied_ips: IpMatcher::compile(&route.denied_ips).0,
            response_headers: compile_response_headers(vhost, route),
        }
    }

    fn prefix_matches(&self, path: &str) -> bool {
        match self.match_type {
            RouteMatchType::PrefixLiteral | RouteMatchType::PrefixPriority => {
                path.starts_with(&self.pattern)
            }

            RouteMatchType::Prefix => prefix_matches_on_boundary(&self.pattern, path),
            _ => false,
        }
    }
}

#[derive(Debug)]
struct CompiledVHost {
    listen_port: u16,
    server_names: Vec<ServerNamePattern>,
    routes: Vec<CompiledRoute>,
    allowed_ips: IpMatcher,
    denied_ips: IpMatcher,
}

#[derive(Debug)]
pub struct CompiledConfig {
    pub config: Arc<FerrixConfig>,
    pub real_ip: RealIpResolver,
    vhosts: Vec<CompiledVHost>,
    rings: HashMap<String, HashRing>,
    // `X-Ferrix-Upstream` value for each target, by upstream name and target index.
    target_headers: HashMap<String, Vec<Option<HeaderValue>>>,
    /// `Server` response header, per `http.server_tokens`.
    pub server_header: HeaderValue,
}

impl CompiledConfig {
    pub fn build(config: Arc<FerrixConfig>) -> Self {
        let vhosts = config
            .vhosts
            .iter()
            .map(|v: &VirtualHost| CompiledVHost {
                listen_port: v.listen_port,
                server_names: v
                    .server_names
                    .iter()
                    .filter_map(|n| ServerNamePattern::parse(n))
                    .collect(),
                routes: v
                    .routes
                    .iter()
                    .map(|r| CompiledRoute::build(v, r))
                    .collect(),
                allowed_ips: IpMatcher::compile(&v.allowed_ips).0,
                denied_ips: IpMatcher::compile(&v.denied_ips).0,
            })
            .collect();

        let real_ip = RealIpResolver::new(&config.real_ip);
        let rings = config
            .upstreams
            .iter()
            .filter(|(_, u)| u.algorithm == LoadBalanceAlgorithm::ConsistentHash)
            .map(|(name, u)| (name.clone(), HashRing::build(u)))
            .collect();
        let target_headers = config
            .upstreams
            .iter()
            .map(|(name, u)| {
                let values = u
                    .targets
                    .iter()
                    .map(|t| HeaderValue::from_str(&t.url).ok())
                    .collect();
                (name.clone(), values)
            })
            .collect();
        let server_header = if config.http.server_tokens {
            HeaderValue::from_static(concat!("ferrix/", env!("CARGO_PKG_VERSION")))
        } else {
            HeaderValue::from_static("ferrix")
        };
        Self {
            config,
            real_ip,
            vhosts,
            rings,
            target_headers,
            server_header,
        }
    }

    pub fn target_header(&self, upstream: &str, index: usize) -> Option<&HeaderValue> {
        self.target_headers.get(upstream)?.get(index)?.as_ref()
    }

    pub fn hash_ring(&self, upstream: &str) -> Option<&HashRing> {
        self.rings.get(upstream)
    }

    pub fn vhost_acl(&self, vhost_index: usize) -> Option<(&IpMatcher, &IpMatcher)> {
        self.vhosts
            .get(vhost_index)
            .map(|v| (&v.allowed_ips, &v.denied_ips))
    }

    pub fn route_acl(
        &self,
        vhost_index: usize,
        route_index: usize,
    ) -> Option<(&IpMatcher, &IpMatcher)> {
        self.vhosts
            .get(vhost_index)?
            .routes
            .get(route_index)
            .map(|r| (&r.allowed_ips, &r.denied_ips))
    }

    // vhost-level `headers` overlaid with the route's `add_headers`, parsed once per
    // config load instead of once per response.
    pub fn route_response_headers(
        &self,
        vhost_index: usize,
        route_index: usize,
    ) -> &[(HeaderName, HeaderValue)] {
        self.vhosts
            .get(vhost_index)
            .and_then(|v| v.routes.get(route_index))
            .map(|r| r.response_headers.as_slice())
            .unwrap_or(&[])
    }

    pub fn find_vhost(&self, host: &str, port: u16) -> Option<VHostMatch> {
        if self.vhosts.is_empty() {
            return None;
        }
        let host = host.trim().trim_end_matches('.').to_ascii_lowercase();

        let port_is_meaningful = self.vhosts.iter().any(|v| v.listen_port == port);
        let candidates: Vec<usize> = (0..self.vhosts.len())
            .filter(|&i| !port_is_meaningful || self.vhosts[i].listen_port == port)
            .collect();

        let mut best_wildcard_prefix: Option<(usize, usize)> = None;
        let mut best_wildcard_suffix: Option<(usize, usize)> = None;
        let mut first_regex: Option<usize> = None;
        let mut catch_all: Option<usize> = None;

        for &i in &candidates {
            for pattern in &self.vhosts[i].server_names {
                match pattern {
                    ServerNamePattern::Exact(name) => {
                        if name == &host {
                            return Some(VHostMatch {
                                vhost_index: i,
                                reason: VHostMatchReason::Exact,
                            });
                        }
                    }
                    ServerNamePattern::WildcardPrefix(suffix) => {
                        if host.len() > suffix.len() && host.ends_with(suffix.as_str()) {
                            let len = suffix.len();
                            if best_wildcard_prefix.is_none_or(|(best, _)| len > best) {
                                best_wildcard_prefix = Some((len, i));
                            }
                        }
                    }
                    ServerNamePattern::WildcardSuffix(prefix) => {
                        if host.len() > prefix.len() && host.starts_with(prefix.as_str()) {
                            let len = prefix.len();
                            if best_wildcard_suffix.is_none_or(|(best, _)| len > best) {
                                best_wildcard_suffix = Some((len, i));
                            }
                        }
                    }
                    ServerNamePattern::Regex(re) => {
                        if first_regex.is_none() && re.is_match(&host) {
                            first_regex = Some(i);
                        }
                    }
                    ServerNamePattern::CatchAll => {
                        if catch_all.is_none() {
                            catch_all = Some(i);
                        }
                    }
                }
            }
        }

        if let Some((_, i)) = best_wildcard_prefix {
            return Some(VHostMatch {
                vhost_index: i,
                reason: VHostMatchReason::WildcardPrefix,
            });
        }
        if let Some((_, i)) = best_wildcard_suffix {
            return Some(VHostMatch {
                vhost_index: i,
                reason: VHostMatchReason::WildcardSuffix,
            });
        }
        if let Some(i) = first_regex {
            return Some(VHostMatch {
                vhost_index: i,
                reason: VHostMatchReason::Regex,
            });
        }
        if let Some(i) = catch_all {
            return Some(VHostMatch {
                vhost_index: i,
                reason: VHostMatchReason::CatchAll,
            });
        }

        candidates.first().map(|&i| VHostMatch {
            vhost_index: i,
            reason: VHostMatchReason::Fallback,
        })
    }

    pub fn match_route(&self, vhost_index: usize, path: &str) -> Option<RouteMatch> {
        let vhost = self.vhosts.get(vhost_index)?;

        let mut best_prefix: Option<(usize, usize, bool)> = None;

        for (i, route) in vhost.routes.iter().enumerate() {
            match route.match_type {
                RouteMatchType::Exact => {
                    if path == route.pattern {
                        return Some(RouteMatch {
                            route_index: i,
                            reason: RouteMatchReason::Exact,
                        });
                    }
                }
                RouteMatchType::Prefix
                | RouteMatchType::PrefixLiteral
                | RouteMatchType::PrefixPriority => {
                    if route.prefix_matches(path) {
                        let len = route.pattern.len();

                        if best_prefix.is_none_or(|(best, _, _)| len > best) {
                            best_prefix =
                                Some((len, i, route.match_type == RouteMatchType::PrefixPriority));
                        }
                    }
                }
                RouteMatchType::Regex | RouteMatchType::RegexCaseInsensitive => {}
            }
        }

        if let Some((_, i, true)) = best_prefix {
            return Some(RouteMatch {
                route_index: i,
                reason: RouteMatchReason::PrefixPriority,
            });
        }

        for (i, route) in vhost.routes.iter().enumerate() {
            if let Some(re) = &route.regex {
                if re.is_match(path) {
                    return Some(RouteMatch {
                        route_index: i,
                        reason: RouteMatchReason::Regex,
                    });
                }
            }
        }

        best_prefix.map(|(_, i, _)| RouteMatch {
            route_index: i,
            reason: RouteMatchReason::LongestPrefix,
        })
    }
}

fn normalize_prefix(pattern: &str) -> String {
    if pattern == "/" {
        return pattern.to_string();
    }
    let trimmed = pattern.trim_end_matches('/');
    if trimmed.is_empty() {
        "/".to_string()
    } else {
        trimmed.to_string()
    }
}

fn prefix_matches_on_boundary(pattern: &str, path: &str) -> bool {
    if pattern == "/" {
        return path.starts_with('/');
    }
    if !path.starts_with(pattern) {
        return false;
    }

    matches!(path.as_bytes().get(pattern.len()), None | Some(b'/'))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::*;

    fn route(id: &str, pattern: &str, match_type: RouteMatchType) -> RouteRule {
        RouteRule {
            id: id.to_string(),
            path_prefix: pattern.to_string(),
            match_type,
            upstream_name: Some("b".to_string()),
            ..Default::default()
        }
    }

    fn vhost(id: &str, names: &[&str], routes: Vec<RouteRule>) -> VirtualHost {
        VirtualHost {
            id: id.to_string(),
            name: id.to_string(),
            server_names: names.iter().map(|s| s.to_string()).collect(),
            listen_port: 8000,
            headers: Default::default(),
            rate_limit: None,
            limit_conn: None,
            allowed_ips: vec![],
            denied_ips: vec![],
            basic_auth: None,
            satisfy: Satisfy::All,
            tls: None,
            routes,
            error_pages: Default::default(),
        }
    }

    fn compiled(vhosts: Vec<VirtualHost>) -> CompiledConfig {
        let cfg = FerrixConfig {
            vhosts,
            ..Default::default()
        };
        CompiledConfig::build(Arc::new(cfg))
    }

    fn matched_id<'a>(cfg: &'a CompiledConfig, vhost_index: usize, path: &str) -> Option<&'a str> {
        cfg.match_route(vhost_index, path).map(|m| {
            cfg.config.vhosts[vhost_index].routes[m.route_index]
                .id
                .as_str()
        })
    }

    #[test]
    fn equal_length_prefixes_elect_the_first_declared() {
        let cfg = compiled(vec![vhost(
            "v",
            &["*"],
            vec![
                route("primeira", "/aa", RouteMatchType::Prefix),
                route("segunda", "/bb", RouteMatchType::Prefix),
            ],
        )]);

        assert_eq!(matched_id(&cfg, 0, "/aa"), Some("primeira"));
        assert_eq!(matched_id(&cfg, 0, "/bb"), Some("segunda"));
    }

    #[test]
    fn longest_prefix_wins_over_shorter() {
        let cfg = compiled(vec![vhost(
            "v",
            &["*"],
            vec![
                route("raiz", "/", RouteMatchType::Prefix),
                route("api", "/api", RouteMatchType::Prefix),
                route("api-v2", "/api/v2", RouteMatchType::Prefix),
            ],
        )]);

        assert_eq!(matched_id(&cfg, 0, "/"), Some("raiz"));
        assert_eq!(matched_id(&cfg, 0, "/api"), Some("api"));
        assert_eq!(matched_id(&cfg, 0, "/api/v2/x"), Some("api-v2"));
    }

    #[test]
    fn safe_prefix_requires_segment_boundary() {
        let cfg = compiled(vec![vhost(
            "v",
            &["*"],
            vec![
                route("raiz", "/", RouteMatchType::Prefix),
                route("api", "/api", RouteMatchType::Prefix),
            ],
        )]);

        assert_eq!(matched_id(&cfg, 0, "/api"), Some("api"));
        assert_eq!(matched_id(&cfg, 0, "/api/users"), Some("api"));

        assert_eq!(matched_id(&cfg, 0, "/apifoo"), Some("raiz"));
    }

    #[test]
    fn literal_prefix_reproduces_nginx_behaviour() {
        let cfg = compiled(vec![vhost(
            "v",
            &["*"],
            vec![
                route("raiz", "/", RouteMatchType::PrefixLiteral),
                route("api", "/api", RouteMatchType::PrefixLiteral),
            ],
        )]);

        assert_eq!(matched_id(&cfg, 0, "/apifoo"), Some("api"));
    }

    #[test]
    fn trailing_slash_in_pattern_is_normalized() {
        let cfg = compiled(vec![vhost(
            "v",
            &["*"],
            vec![route("api", "/api/", RouteMatchType::Prefix)],
        )]);

        assert_eq!(matched_id(&cfg, 0, "/api"), Some("api"));
        assert_eq!(matched_id(&cfg, 0, "/api/x"), Some("api"));
    }

    #[test]
    fn exact_match_beats_longer_prefix() {
        let cfg = compiled(vec![vhost(
            "v",
            &["*"],
            vec![
                route("prefixo", "/health/check", RouteMatchType::Prefix),
                route("exato", "/health", RouteMatchType::Exact),
            ],
        )]);

        let m = cfg.match_route(0, "/health").unwrap();
        assert_eq!(m.reason, RouteMatchReason::Exact);
        assert_eq!(matched_id(&cfg, 0, "/health"), Some("exato"));

        assert_eq!(matched_id(&cfg, 0, "/health/check"), Some("prefixo"));
    }

    #[test]
    fn regex_beats_plain_prefix() {
        let cfg = compiled(vec![vhost(
            "v",
            &["*"],
            vec![
                route("estatico", "/", RouteMatchType::Prefix),
                route("imagens", r"\.(png|jpg)$", RouteMatchType::Regex),
            ],
        )]);

        let m = cfg.match_route(0, "/fotos/a.png").unwrap();
        assert_eq!(m.reason, RouteMatchReason::Regex);
        assert_eq!(matched_id(&cfg, 0, "/fotos/a.png"), Some("imagens"));
        assert_eq!(matched_id(&cfg, 0, "/fotos/a.txt"), Some("estatico"));
    }

    #[test]
    fn priority_prefix_stops_regex_evaluation() {
        let cfg = compiled(vec![vhost(
            "v",
            &["*"],
            vec![
                route("assets", "/assets", RouteMatchType::PrefixPriority),
                route("imagens", r"\.(png|jpg)$", RouteMatchType::Regex),
            ],
        )]);

        let m = cfg.match_route(0, "/assets/a.png").unwrap();
        assert_eq!(m.reason, RouteMatchReason::PrefixPriority);
        assert_eq!(matched_id(&cfg, 0, "/assets/a.png"), Some("assets"));

        assert_eq!(matched_id(&cfg, 0, "/outro/a.png"), Some("imagens"));
    }

    #[test]
    fn first_declared_regex_wins() {
        let cfg = compiled(vec![vhost(
            "v",
            &["*"],
            vec![
                route("primeiro", r"\.png$", RouteMatchType::Regex),
                route("segundo", r"\.(png|gif)$", RouteMatchType::Regex),
            ],
        )]);

        assert_eq!(matched_id(&cfg, 0, "/a.png"), Some("primeiro"));
        assert_eq!(matched_id(&cfg, 0, "/a.gif"), Some("segundo"));
    }

    #[test]
    fn case_insensitive_regex_matches_upper_case() {
        let cfg = compiled(vec![vhost(
            "v",
            &["*"],
            vec![route(
                "img",
                r"\.png$",
                RouteMatchType::RegexCaseInsensitive,
            )],
        )]);

        assert_eq!(matched_id(&cfg, 0, "/A.PNG"), Some("img"));
    }

    #[test]
    fn no_route_matches_returns_none() {
        let cfg = compiled(vec![vhost(
            "v",
            &["*"],
            vec![route("api", "/api", RouteMatchType::Prefix)],
        )]);

        assert!(cfg.match_route(0, "/outro").is_none());
    }

    #[test]
    fn exact_server_name_wins_over_wildcard() {
        let cfg = compiled(vec![
            vhost("curinga", &["*.exemplo.com"], vec![]),
            vhost("exato", &["www.exemplo.com"], vec![]),
        ]);

        let m = cfg.find_vhost("www.exemplo.com", 8000).unwrap();
        assert_eq!(m.reason, VHostMatchReason::Exact);
        assert_eq!(cfg.config.vhosts[m.vhost_index].id, "exato");
    }

    #[test]
    fn wildcard_prefix_matches_subdomain_only() {
        let cfg = compiled(vec![
            vhost("curinga", &["*.exemplo.com"], vec![]),
            vhost("fallback", &["*"], vec![]),
        ]);

        let m = cfg.find_vhost("api.exemplo.com", 8000).unwrap();
        assert_eq!(cfg.config.vhosts[m.vhost_index].id, "curinga");

        let m = cfg.find_vhost("exemplo.com", 8000).unwrap();
        assert_eq!(cfg.config.vhosts[m.vhost_index].id, "fallback");
    }

    #[test]
    fn longest_wildcard_prefix_wins() {
        let cfg = compiled(vec![
            vhost("curto", &["*.com"], vec![]),
            vhost("longo", &["*.exemplo.com"], vec![]),
        ]);

        let m = cfg.find_vhost("api.exemplo.com", 8000).unwrap();
        assert_eq!(cfg.config.vhosts[m.vhost_index].id, "longo");
    }

    #[test]
    fn wildcard_suffix_is_supported() {
        let cfg = compiled(vec![vhost("mail", &["mail.*"], vec![])]);

        let m = cfg.find_vhost("mail.exemplo.com", 8000).unwrap();
        assert_eq!(m.reason, VHostMatchReason::WildcardSuffix);
    }

    #[test]
    fn wildcard_prefix_beats_wildcard_suffix() {
        let cfg = compiled(vec![
            vhost("sufixo", &["mail.*"], vec![]),
            vhost("prefixo", &["*.exemplo.com"], vec![]),
        ]);

        let m = cfg.find_vhost("mail.exemplo.com", 8000).unwrap();
        assert_eq!(cfg.config.vhosts[m.vhost_index].id, "prefixo");
    }

    #[test]
    fn regex_server_name_is_matched_after_wildcards() {
        let cfg = compiled(vec![
            vhost("regex", &["~^app[0-9]+\\.exemplo\\.com$"], vec![]),
            vhost("catch", &["*"], vec![]),
        ]);

        let m = cfg.find_vhost("app42.exemplo.com", 8000).unwrap();
        assert_eq!(m.reason, VHostMatchReason::Regex);
        assert_eq!(cfg.config.vhosts[m.vhost_index].id, "regex");
    }

    #[test]
    fn host_matching_ignores_case_and_trailing_dot() {
        let cfg = compiled(vec![vhost("v", &["Exemplo.COM"], vec![])]);

        assert_eq!(
            cfg.find_vhost("exemplo.com.", 8000).unwrap().reason,
            VHostMatchReason::Exact
        );
    }

    #[test]
    fn falls_back_to_first_vhost_when_nothing_matches() {
        let cfg = compiled(vec![
            vhost("primeiro", &["a.com"], vec![]),
            vhost("segundo", &["b.com"], vec![]),
        ]);

        let m = cfg.find_vhost("desconhecido.com", 8000).unwrap();
        assert_eq!(m.reason, VHostMatchReason::Fallback);
        assert_eq!(cfg.config.vhosts[m.vhost_index].id, "primeiro");
    }

    #[test]
    fn listen_port_selects_between_hosts() {
        let mut a = vhost("porta-8000", &["exemplo.com"], vec![]);
        a.listen_port = 8000;
        let mut b = vhost("porta-9000", &["exemplo.com"], vec![]);
        b.listen_port = 9000;
        let cfg = compiled(vec![a, b]);

        let m = cfg.find_vhost("exemplo.com", 9000).unwrap();
        assert_eq!(cfg.config.vhosts[m.vhost_index].id, "porta-9000");
    }

    #[test]
    fn unknown_port_does_not_blackhole_traffic() {
        let cfg = compiled(vec![vhost("v", &["exemplo.com"], vec![])]);

        let m = cfg.find_vhost("exemplo.com", 12345).unwrap();
        assert_eq!(cfg.config.vhosts[m.vhost_index].id, "v");
    }

    #[test]
    fn empty_config_matches_nothing() {
        let cfg = compiled(vec![]);
        assert!(cfg.find_vhost("qualquer.com", 8000).is_none());
    }
}
