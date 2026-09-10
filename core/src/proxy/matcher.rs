//! Seleção de virtual host e de rota, com a precedência do nginx.
//!
//! Substitui a lógica que vivia inline no motor de proxy, que tinha três defeitos: o
//! desempate por prefixo usava `>=` e portanto elegia a **última** rota de mesmo tamanho
//! em vez da primeira; não havia match exato nem por expressão regular; e `server_name`
//! só entendia nome exato ou o curinga total `*`.
//!
//! As expressões regulares são compiladas uma vez por versão de configuração — nunca por
//! requisição.

use regex::Regex;
use std::sync::Arc;

use crate::config::{
    validate::compile_route_regex, FerrixConfig, RouteMatchType, RouteRule, VirtualHost,
};
use crate::net::cidr::IpMatcher;
use crate::proxy::realip::RealIpResolver;

/// Por qual regra uma rota foi eleita. Serve ao endpoint de teste de rota, que precisa
/// explicar a decisão, não apenas informá-la.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RouteMatchReason {
    /// Casou um `match_type: exact`, que encerra a busca imediatamente.
    Exact,
    /// Casou um prefixo prioritário, que impede a avaliação das expressões regulares.
    PrefixPriority,
    /// Primeira expressão regular a casar, na ordem de declaração.
    Regex,
    /// Prefixo mais longo, usado quando nenhuma expressão regular casou.
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

/// Por qual regra um virtual host foi eleito.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VHostMatchReason {
    Exact,
    WildcardPrefix,
    WildcardSuffix,
    Regex,
    CatchAll,
    /// Nenhum casou; caiu no primeiro host declarado.
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

/// Um `server_name` já interpretado.
#[derive(Debug, Clone)]
enum ServerNamePattern {
    Exact(String),
    /// `*.exemplo.com`, guardado como `.exemplo.com`.
    WildcardPrefix(String),
    /// `mail.*`, guardado como `mail.`.
    WildcardSuffix(String),
    Regex(Regex),
    /// `*` ou `_`.
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
            // Um regex que não compila não deve derrubar o host inteiro; a validação de
            // configuração é quem reporta o problema.
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

/// Uma rota com o que precisa estar pronto antes da requisição chegar.
#[derive(Debug)]
struct CompiledRoute {
    match_type: RouteMatchType,
    /// Para prefixos, já sem a barra final redundante.
    pattern: String,
    regex: Option<Regex>,
    allowed_ips: IpMatcher,
    denied_ips: IpMatcher,
}

impl CompiledRoute {
    fn build(route: &RouteRule) -> Self {
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
        }
    }

    fn prefix_matches(&self, path: &str) -> bool {
        match self.match_type {
            // `prefix_literal` e `prefix_priority` reproduzem o nginx ao pé da letra: o
            // prefixo é comparado como texto puro, então `/api` casa com `/apifoo`.
            RouteMatchType::PrefixLiteral | RouteMatchType::PrefixPriority => {
                path.starts_with(&self.pattern)
            }
            // `prefix` exige fronteira de segmento: `/api` casa `/api` e `/api/x`, mas não
            // `/apifoo`. É o padrão para configuração escrita à mão; o importador de
            // nginx.conf emite `prefix_literal` para não alterar o comportamento original.
            RouteMatchType::Prefix => prefix_matches_on_boundary(&self.pattern, path),
            _ => false,
        }
    }
}

/// Um virtual host com nomes, rotas e listas de acesso já interpretados.
#[derive(Debug)]
struct CompiledVHost {
    listen_port: u16,
    server_names: Vec<ServerNamePattern>,
    routes: Vec<CompiledRoute>,
    allowed_ips: IpMatcher,
    denied_ips: IpMatcher,
}

/// A configuração ativa junto com tudo que foi pré-compilado a partir dela.
///
/// Trocada atomicamente a cada hot-reload, para que uma requisição nunca veja uma
/// configuração pela metade. Tudo que custa caro para preparar — expressões regulares,
/// blocos CIDR, a lista de proxies confiáveis — é montado aqui uma vez por versão de
/// configuração, nunca por requisição.
#[derive(Debug)]
pub struct CompiledConfig {
    pub config: Arc<FerrixConfig>,
    pub real_ip: RealIpResolver,
    vhosts: Vec<CompiledVHost>,
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
                routes: v.routes.iter().map(CompiledRoute::build).collect(),
                allowed_ips: IpMatcher::compile(&v.allowed_ips).0,
                denied_ips: IpMatcher::compile(&v.denied_ips).0,
            })
            .collect();

        let real_ip = RealIpResolver::new(&config.real_ip);
        Self {
            config,
            real_ip,
            vhosts,
        }
    }

    /// As listas de acesso compiladas de um host, na ordem `(permitidos, negados)`.
    pub fn vhost_acl(&self, vhost_index: usize) -> Option<(&IpMatcher, &IpMatcher)> {
        self.vhosts
            .get(vhost_index)
            .map(|v| (&v.allowed_ips, &v.denied_ips))
    }

    /// As listas de acesso compiladas de uma rota, equivalentes a `allow`/`deny` dentro de
    /// um `location`.
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

    /// Elege o virtual host para um `Host` e uma porta, na precedência do nginx:
    /// nome exato, depois o curinga `*.dominio` mais longo, depois `nome.*`, depois a
    /// primeira expressão regular, depois o curinga total, e por fim o primeiro host.
    pub fn find_vhost(&self, host: &str, port: u16) -> Option<VHostMatch> {
        if self.vhosts.is_empty() {
            return None;
        }
        let host = host.trim().trim_end_matches('.').to_ascii_lowercase();

        // A porta só filtra se algum host de fato a declara. Sem isso, uma configuração
        // cujo `listen_port` divirja do listener deixaria de responder por inteiro — um
        // 404 silencioso é pior que ignorar o campo.
        let port_is_meaningful = self.vhosts.iter().any(|v| v.listen_port == port);
        let candidates: Vec<usize> = (0..self.vhosts.len())
            .filter(|&i| !port_is_meaningful || self.vhosts[i].listen_port == port)
            .collect();

        let mut best_wildcard_prefix: Option<(usize, usize)> = None; // (comprimento, índice)
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
                        // `*.exemplo.com` cobre `www.exemplo.com`, mas não `exemplo.com`.
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

    /// Elege a rota dentro de um host, na precedência do nginx:
    /// match exato encerra na hora; senão vence o prefixo mais longo, e se ele for
    /// prioritário (`^~`) as expressões regulares nem chegam a ser avaliadas; senão a
    /// primeira expressão regular que casar; e por fim o prefixo mais longo.
    pub fn match_route(&self, vhost_index: usize, path: &str) -> Option<RouteMatch> {
        let vhost = self.vhosts.get(vhost_index)?;

        let mut best_prefix: Option<(usize, usize, bool)> = None; // (comprimento, índice, prioritário)

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
                        // Estritamente maior: em empate vence quem foi declarado antes.
                        // O código anterior usava `>=` e elegia o último.
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

/// Remove a barra final redundante para que `/api/` e `/api` se comportem igual.
/// A raiz `/` é preservada.
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

/// Prefixo que só casa em fronteira de segmento.
fn prefix_matches_on_boundary(pattern: &str, path: &str) -> bool {
    if pattern == "/" {
        return path.starts_with('/');
    }
    if !path.starts_with(pattern) {
        return false;
    }
    // O que vem logo depois do prefixo precisa ser o fim do caminho ou uma barra.
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

    // -- Rotas ---------------------------------------------------------------

    #[test]
    fn equal_length_prefixes_elect_the_first_declared() {
        // Este é o bug do `>=`: antes vencia a última rota de mesmo comprimento.
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
        // O ponto do modo seguro: `/apifoo` não pertence a `/api`.
        assert_eq!(matched_id(&cfg, 0, "/apifoo"), Some("raiz"));
    }

    #[test]
    fn literal_prefix_reproduces_nginx_behaviour() {
        // O nginx casa `/apifoo` com `location /api`. O importador emite este modo para
        // não alterar o comportamento de uma configuração migrada.
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
        // O exato não afeta caminhos diferentes.
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
        // Fora do prefixo prioritário, o regex volta a valer.
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

    // -- Virtual hosts -------------------------------------------------------

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

        // O domínio nu não pertence a `*.exemplo.com` — é assim no nginx.
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
        // Precedência do nginx: `*.dominio` vem antes de `nome.*`.
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
        // Se nenhum host declara a porta, ignorá-la é melhor que responder 404 a tudo.
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
