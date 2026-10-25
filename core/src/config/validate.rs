use regex::Regex;
use serde::Serialize;
use std::collections::{HashMap, HashSet};
use std::path::Path;

use super::{AdminAuthMode, FerrixConfig, RouteMatchType, RouteTargetType, VirtualHost};
use crate::net::cidr::{listen_addr_is_loopback, IpRule};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    Error,

    Warning,
}

#[derive(Debug, Clone, Serialize)]
pub struct ValidationIssue {
    pub severity: Severity,

    pub path: String,
    pub message: String,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct ValidationReport {
    pub issues: Vec<ValidationIssue>,
}

impl ValidationReport {
    fn error(&mut self, path: impl Into<String>, message: impl Into<String>) {
        self.issues.push(ValidationIssue {
            severity: Severity::Error,
            path: path.into(),
            message: message.into(),
        });
    }

    fn warn(&mut self, path: impl Into<String>, message: impl Into<String>) {
        self.issues.push(ValidationIssue {
            severity: Severity::Warning,
            path: path.into(),
            message: message.into(),
        });
    }

    pub fn errors(&self) -> impl Iterator<Item = &ValidationIssue> {
        self.issues.iter().filter(|i| i.severity == Severity::Error)
    }

    pub fn warnings(&self) -> impl Iterator<Item = &ValidationIssue> {
        self.issues
            .iter()
            .filter(|i| i.severity == Severity::Warning)
    }

    pub fn is_valid(&self) -> bool {
        self.errors().next().is_none()
    }

    pub fn summary(&self) -> String {
        if self.issues.is_empty() {
            return "configuração válida".to_string();
        }
        self.issues
            .iter()
            .map(|i| {
                let tag = match i.severity {
                    Severity::Error => "erro",
                    Severity::Warning => "aviso",
                };
                format!("[{}] {}: {}", tag, i.path, i.message)
            })
            .collect::<Vec<_>>()
            .join("\n")
    }
}

pub fn validate(cfg: &FerrixConfig) -> ValidationReport {
    let mut report = ValidationReport::default();

    validate_listeners(cfg, &mut report);
    validate_admin_auth(cfg, &mut report);
    validate_real_ip(cfg, &mut report);
    validate_upstreams(cfg, &mut report);

    if cfg.vhosts.is_empty() {
        report.error(
            "vhosts",
            "a configuração precisa de ao menos um virtual host",
        );
    }

    let mut seen_vhost_ids = HashSet::new();
    for (i, vhost) in cfg.vhosts.iter().enumerate() {
        let base = format!("vhosts[{}]", i);

        if vhost.id.trim().is_empty() {
            report.error(format!("{}.id", base), "o id não pode ser vazio");
        } else if !seen_vhost_ids.insert(&vhost.id) {
            report.error(
                format!("{}.id", base),
                format!("id de virtual host duplicado: '{}'", vhost.id),
            );
        }

        if vhost.server_names.is_empty() {
            report.warn(
                format!("{}.server_names", base),
                "sem server_names, este host só é alcançado como fallback",
            );
        }

        validate_ip_list(
            &vhost.allowed_ips,
            &format!("{}.allowed_ips", base),
            &mut report,
        );
        validate_ip_list(
            &vhost.denied_ips,
            &format!("{}.denied_ips", base),
            &mut report,
        );
        validate_vhost_routes(cfg, vhost, &base, &mut report);

        if let Some(tls) = &vhost.tls {
            for (field, path) in [("cert_path", &tls.cert_path), ("key_path", &tls.key_path)] {
                if !Path::new(path).is_file() {
                    report.warn(
                        format!("{}.tls.{}", base, field),
                        format!("o arquivo '{}' não existe no momento", path),
                    );
                }
            }
        }
    }

    validate_shared_tls_ports(cfg, &mut report);

    report
}

fn validate_shared_tls_ports(cfg: &FerrixConfig, report: &mut ValidationReport) {
    let mut first_tls_vhost_by_port: HashMap<u16, &str> = HashMap::new();

    for vhost in &cfg.vhosts {
        if vhost.tls.is_none() {
            continue;
        }
        match first_tls_vhost_by_port.get(&vhost.listen_port) {
            None => {
                first_tls_vhost_by_port.insert(vhost.listen_port, &vhost.id);
            }
            Some(first_id) => {
                report.warn(
                    format!("vhosts[{}].tls", vhost.id),
                    format!(
                        "a porta {} já usa o certificado do host '{}'; o Ferrix ainda não \
                         escolhe certificado por SNI, então este aqui é ignorado",
                        vhost.listen_port, first_id
                    ),
                );
            }
        }
    }
}

fn validate_listeners(cfg: &FerrixConfig, report: &mut ValidationReport) {
    for (label, addr) in [
        ("admin.listen", &cfg.admin.listen),
        ("http.listen", &cfg.http.listen),
    ] {
        if addr.rsplit_once(':').is_none() {
            report.error(
                label,
                format!("endereço '{}' precisa estar no formato host:porta", addr),
            );
        }
    }

    if cfg.http.client_max_body_size < cfg.http.proxy_request_buffer_size {
        report.warn(
            "http.proxy_request_buffer_size",
            "o buffer de retry é maior que o corpo máximo aceito, então nunca será atingido",
        );
    }
}

fn validate_admin_auth(cfg: &FerrixConfig, report: &mut ValidationReport) {
    let auth = &cfg.admin.auth;
    match auth.mode {
        AdminAuthMode::None => {
            if !listen_addr_is_loopback(&cfg.admin.listen) {
                report.error(
                    "admin.auth.mode",
                    format!(
                        "a API de administração escuta em '{}', fora de loopback, sem \
                         autenticação — configure admin.auth ou restrinja o listen a 127.0.0.1",
                        cfg.admin.listen
                    ),
                );
            }
        }
        AdminAuthMode::Token => {
            if auth.token.as_deref().unwrap_or("").trim().is_empty() {
                report.error("admin.auth.token", "o modo token exige um token não-vazio");
            }
        }
        AdminAuthMode::Basic => {
            if auth.username.as_deref().unwrap_or("").trim().is_empty()
                || auth.password.as_deref().unwrap_or("").is_empty()
            {
                report.error(
                    "admin.auth",
                    "o modo basic exige username e password preenchidos",
                );
            }
        }
    }
}

fn validate_real_ip(cfg: &FerrixConfig, report: &mut ValidationReport) {
    validate_ip_list(
        &cfg.real_ip.trusted_proxies,
        "real_ip.trusted_proxies",
        report,
    );

    if cfg.real_ip.header != super::RealIpHeader::None && cfg.real_ip.trusted_proxies.is_empty() {
        report.error(
            "real_ip.trusted_proxies",
            "usar um header de IP real sem trusted_proxies permite que qualquer cliente \
             forje o próprio IP e escape do rate limit e do ACL",
        );
    }
}

fn validate_upstreams(cfg: &FerrixConfig, report: &mut ValidationReport) {
    for (name, upstream) in &cfg.upstreams {
        let base = format!("upstreams.{}", name);

        if name != &upstream.name {
            report.warn(
                format!("{}.name", base),
                format!(
                    "a chave do mapa ('{}') difere do campo name ('{}'); a chave é o que vale",
                    name, upstream.name
                ),
            );
        }

        if upstream.targets.is_empty() {
            report.error(
                format!("{}.targets", base),
                "o pool não tem nenhum servidor",
            );
            continue;
        }

        for (i, target) in upstream.targets.iter().enumerate() {
            let tpath = format!("{}.targets[{}].url", base, i);
            match url::Url::parse(&target.url) {
                Ok(u) if u.scheme() == "http" || u.scheme() == "https" => {}
                Ok(u) => report.error(
                    &tpath,
                    format!("esquema '{}' não suportado; use http ou https", u.scheme()),
                ),
                Err(e) => report.error(&tpath, format!("URL inválida '{}': {}", target.url, e)),
            }
        }

        if upstream.targets.iter().all(|t| t.down) {
            report.error(
                format!("{}.targets", base),
                "todos os servidores do pool estão marcados como down",
            );
        } else if upstream.targets.iter().all(|t| t.down || t.backup) {
            report.warn(
                format!("{}.targets", base),
                "o pool só tem servidores backup ativos; nenhum recebe tráfego normal",
            );
        }

        if let Some(hc) = &upstream.health_check {
            if hc.interval_secs == 0 {
                report.error(
                    format!("{}.health_check.interval_secs", base),
                    "o intervalo precisa ser maior que zero",
                );
            }
            if hc.timeout_secs >= hc.interval_secs && hc.interval_secs > 0 {
                report.warn(
                    format!("{}.health_check.timeout_secs", base),
                    "o timeout é maior ou igual ao intervalo; as verificações vão se sobrepor",
                );
            }
        }
    }
}

fn validate_vhost_routes(
    cfg: &FerrixConfig,
    vhost: &VirtualHost,
    base: &str,
    report: &mut ValidationReport,
) {
    if vhost.routes.is_empty() {
        report.warn(
            format!("{}.routes", base),
            "sem rotas, este host responde 404 a tudo",
        );
    }

    let mut seen_route_ids = HashSet::new();
    let mut seen_patterns: HashSet<(RouteMatchType, &str)> = HashSet::new();

    for (j, route) in vhost.routes.iter().enumerate() {
        let rpath = format!("{}.routes[{}]", base, j);

        if route.id.trim().is_empty() {
            report.error(format!("{}.id", rpath), "o id não pode ser vazio");
        } else if !seen_route_ids.insert(&route.id) {
            report.error(
                format!("{}.id", rpath),
                format!("id de rota duplicado dentro do host: '{}'", route.id),
            );
        }

        if !seen_patterns.insert((route.match_type, route.path_prefix.as_str())) {
            report.warn(
                format!("{}.path_prefix", rpath),
                format!(
                    "'{}' repete um padrão já declarado acima com o mesmo tipo de match; \
                     esta rota nunca será alcançada",
                    route.path_prefix
                ),
            );
        }

        if route.match_type.is_regex() {
            if let Err(e) = compile_route_regex(route.match_type, &route.path_prefix) {
                report.error(format!("{}.path_prefix", rpath), e);
            }
        } else if !route.path_prefix.starts_with('/') {
            report.error(
                format!("{}.path_prefix", rpath),
                format!("'{}' precisa começar com '/'", route.path_prefix),
            );
        }

        match route.target_type {
            RouteTargetType::ProxyUpstream => match &route.upstream_name {
                None => report.error(
                    format!("{}.upstream_name", rpath),
                    "rota de proxy sem upstream definido",
                ),
                Some(name) if !cfg.upstreams.contains_key(name) => report.error(
                    format!("{}.upstream_name", rpath),
                    format!("o upstream '{}' não existe na configuração", name),
                ),
                Some(_) => {}
            },
            RouteTargetType::StaticFiles => match &route.static_root {
                None => report.error(
                    format!("{}.static_root", rpath),
                    "rota de arquivos estáticos sem diretório definido",
                ),
                Some(root) => {
                    let p = Path::new(root);

                    if !p.exists() {
                        report.warn(
                            format!("{}.static_root", rpath),
                            format!("o diretório '{}' não existe no momento", root),
                        );
                    } else if !p.is_dir() {
                        report.error(
                            format!("{}.static_root", rpath),
                            format!("'{}' existe mas não é um diretório", root),
                        );
                    }
                }
            },
            RouteTargetType::Redirect => {
                if route
                    .redirect_url
                    .as_deref()
                    .unwrap_or("")
                    .trim()
                    .is_empty()
                {
                    report.error(
                        format!("{}.redirect_url", rpath),
                        "rota de redirecionamento sem URL de destino",
                    );
                }
                if let Some(code) = route.redirect_code {
                    if !(300..400).contains(&code) {
                        report.error(
                            format!("{}.redirect_code", rpath),
                            format!("{} não é um código de redirecionamento (300–399)", code),
                        );
                    }
                }
            }
        }

        if let Some(rl) = &route.rate_limit {
            if rl.requests_per_second == 0 {
                report.error(
                    format!("{}.rate_limit.requests_per_second", rpath),
                    "a taxa precisa ser maior que zero; para bloquear tudo use um ACL",
                );
            }
        }
    }

    if let Some(rl) = &vhost.rate_limit {
        if rl.requests_per_second == 0 {
            report.error(
                format!("{}.rate_limit.requests_per_second", base),
                "a taxa precisa ser maior que zero; para bloquear tudo use um ACL",
            );
        }
    }
}

fn validate_ip_list(entries: &[String], path: &str, report: &mut ValidationReport) {
    for (i, entry) in entries.iter().enumerate() {
        if let Err(e) = IpRule::parse(entry) {
            report.error(format!("{}[{}]", path, i), e);
        }
    }
}

pub fn compile_route_regex(match_type: RouteMatchType, pattern: &str) -> Result<Regex, String> {
    let source = if match_type == RouteMatchType::RegexCaseInsensitive {
        format!("(?i){}", pattern)
    } else {
        pattern.to_string()
    };
    Regex::new(&source).map_err(|e| format!("expressão regular inválida '{}': {}", pattern, e))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::*;

    fn base_config() -> FerrixConfig {
        let mut cfg = FerrixConfig::default();

        cfg.admin.listen = "127.0.0.1:8080".to_string();
        cfg
    }

    fn proxy_route(id: &str, path: &str, upstream: &str) -> RouteRule {
        RouteRule {
            id: id.to_string(),
            path_prefix: path.to_string(),
            upstream_name: Some(upstream.to_string()),
            ..Default::default()
        }
    }

    #[test]
    fn default_config_has_no_errors() {
        let report = validate(&base_config());
        assert!(report.is_valid(), "{}", report.summary());
    }

    #[test]
    fn missing_upstream_reference_is_an_error() {
        let mut cfg = base_config();
        cfg.vhosts[0].routes = vec![proxy_route("r", "/api", "nao_existe")];

        let report = validate(&cfg);
        assert!(!report.is_valid());
        assert!(report.summary().contains("nao_existe"));
    }

    #[test]
    fn invalid_regex_is_an_error() {
        let mut cfg = base_config();
        let mut route = proxy_route("r", "\\.(php$", "default_backend");
        route.match_type = RouteMatchType::Regex;
        cfg.vhosts[0].routes = vec![route];

        let report = validate(&cfg);
        assert!(!report.is_valid());
        assert!(report.summary().contains("expressão regular inválida"));
    }

    #[test]
    fn duplicate_route_ids_are_rejected() {
        let mut cfg = base_config();
        cfg.vhosts[0].routes = vec![
            proxy_route("mesmo", "/a", "default_backend"),
            proxy_route("mesmo", "/b", "default_backend"),
        ];

        let report = validate(&cfg);
        assert!(!report.is_valid());
        assert!(report.summary().contains("id de rota duplicado"));
    }

    #[test]
    fn duplicate_pattern_warns_about_unreachable_route() {
        let mut cfg = base_config();
        cfg.vhosts[0].routes = vec![
            proxy_route("a", "/api", "default_backend"),
            proxy_route("b", "/api", "default_backend"),
        ];

        let report = validate(&cfg);

        assert!(report.is_valid());
        assert!(report.summary().contains("nunca será alcançada"));
    }

    #[test]
    fn invalid_cidr_in_acl_is_an_error() {
        let mut cfg = base_config();
        cfg.vhosts[0].denied_ips = vec!["10.0.0.0/99".to_string()];

        let report = validate(&cfg);
        assert!(!report.is_valid());
    }

    #[test]
    fn admin_api_exposed_without_auth_is_an_error() {
        let mut cfg = base_config();
        cfg.admin.listen = "0.0.0.0:8080".to_string();
        cfg.admin.auth.mode = AdminAuthMode::None;

        let report = validate(&cfg);
        assert!(!report.is_valid(), "expor a API admin sem auth deve falhar");
    }

    #[test]
    fn admin_api_on_loopback_without_auth_is_allowed() {
        let mut cfg = base_config();
        cfg.admin.listen = "127.0.0.1:8080".to_string();
        cfg.admin.auth.mode = AdminAuthMode::None;

        assert!(validate(&cfg).is_valid());
    }

    #[test]
    fn token_mode_requires_a_token() {
        let mut cfg = base_config();
        cfg.admin.auth.mode = AdminAuthMode::Token;
        cfg.admin.auth.token = Some("   ".to_string());

        assert!(!validate(&cfg).is_valid());
    }

    #[test]
    fn real_ip_header_without_trusted_proxies_is_an_error() {
        let mut cfg = base_config();
        cfg.real_ip.header = RealIpHeader::XForwardedFor;
        cfg.real_ip.trusted_proxies = vec![];

        let report = validate(&cfg);
        assert!(!report.is_valid());
        assert!(report.summary().contains("forje o próprio IP"));
    }

    #[test]
    fn real_ip_header_with_trusted_proxies_is_fine() {
        let mut cfg = base_config();
        cfg.real_ip.header = RealIpHeader::XForwardedFor;
        cfg.real_ip.trusted_proxies = vec!["10.0.0.0/8".to_string()];

        assert!(validate(&cfg).is_valid());
    }

    #[test]
    fn all_targets_down_is_an_error() {
        let mut cfg = base_config();
        if let Some(u) = cfg.upstreams.get_mut("default_backend") {
            for t in &mut u.targets {
                t.down = true;
            }
        }
        assert!(!validate(&cfg).is_valid());
    }

    #[test]
    fn redirect_without_url_is_an_error() {
        let mut cfg = base_config();
        let mut route = proxy_route("r", "/ir", "default_backend");
        route.target_type = RouteTargetType::Redirect;
        route.upstream_name = None;
        route.redirect_url = None;
        cfg.vhosts[0].routes = vec![route];

        assert!(!validate(&cfg).is_valid());
    }

    #[test]
    fn non_redirect_status_code_is_rejected() {
        let mut cfg = base_config();
        let mut route = proxy_route("r", "/ir", "default_backend");
        route.target_type = RouteTargetType::Redirect;
        route.upstream_name = None;
        route.redirect_url = Some("https://exemplo.com".to_string());
        route.redirect_code = Some(200);
        cfg.vhosts[0].routes = vec![route];

        assert!(!validate(&cfg).is_valid());
    }

    #[test]
    fn case_insensitive_regex_compiles_with_flag() {
        let re = compile_route_regex(RouteMatchType::RegexCaseInsensitive, r"\.PNG$").unwrap();
        assert!(re.is_match("/foto.png"));
    }
}
