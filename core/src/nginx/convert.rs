//! Converte a árvore de um `nginx.conf` em uma `FerrixConfig`.
//!
//! Duas regras guiam tudo aqui:
//!
//! 1. **Não inventar equivalências.** O que o Ferrix não sabe fazer vira linha no
//!    relatório, com arquivo e linha, em vez de virar uma aproximação silenciosa.
//! 2. **Nunca afrouxar segurança na migração.** Uma diretiva de proteção que não pode ser
//!    convertida fielmente deixa a rota fechada, não aberta.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use super::parser::{parse, Directive, ParseError};
use super::report::{MigrationReport, NoteLevel};
use crate::config::{
    BasicAuthConfig, CacheConfig, FerrixConfig, LimitConnConfig, LoadBalanceAlgorithm,
    NextUpstreamCondition, ProxyNextUpstreamConfig, RateLimitConfig, RateLimitMode, RealIpHeader,
    RouteMatchType, RouteRule, RouteTargetType, Satisfy, TlsConfig, UpstreamConfig,
    UpstreamTarget, VirtualHost,
};

pub struct ImportResult {
    pub config: FerrixConfig,
    pub report: MigrationReport,
}

/// Converte o conteúdo de um `nginx.conf` já lido.
pub fn import_str(source: &str, file_name: &str) -> Result<ImportResult, ParseError> {
    let directives = parse(source)?;
    let mut converter = Converter::new(file_name, PathBuf::new());
    converter.report.files.push(file_name.to_string());
    converter.run(&directives);
    Ok(ImportResult {
        config: converter.config,
        report: converter.report,
    })
}

/// Lê um `nginx.conf` do disco, resolvendo `include`, e converte.
pub fn import_file(path: &Path) -> Result<ImportResult, String> {
    let source = std::fs::read_to_string(path)
        .map_err(|e| format!("não foi possível ler {}: {}", path.display(), e))?;
    let file_name = path.display().to_string();

    let mut directives =
        parse(&source).map_err(|e| format!("erro de sintaxe em {}: {}", file_name, e))?;

    let base_dir = path.parent().unwrap_or(Path::new(".")).to_path_buf();
    // A raiz da busca por nome é a mesma pasta: é onde `include`s de caminho absoluto
    // (que não existem nesta máquina) têm chance de ser encontrados por nome de arquivo.
    let mut converter = Converter::new(&file_name, base_dir.clone());
    converter.report.files.push(file_name.clone());
    converter.resolve_includes(&mut directives, &base_dir, 0);
    converter.run(&directives);

    Ok(ImportResult {
        config: converter.config,
        report: converter.report,
    })
}

struct Converter {
    config: FerrixConfig,
    report: MigrationReport,
    file: String,
    /// Pasta onde a busca por nome de arquivo procura um `include` de caminho absoluto que
    /// não existe nesta máquina. Vazia quando a conversão partiu de uma string em memória.
    search_root: PathBuf,
    /// Taxas declaradas em `limit_req_zone`, para resolver o `limit_req zone=NOME`.
    limit_req_zones: HashMap<String, u32>,
    limit_conn_zones: HashSet<String>,
    /// Contador para nomear upstreams criados a partir de um `proxy_pass` direto.
    synthetic_upstreams: usize,
    route_seq: usize,
    /// `add_header` visto no nível `http`, aplicado como ponto de partida de todo vhost
    /// criado depois — nginx também propaga assim, na ausência do override quirk (se o
    /// server redefine `add_header`, o Ferrix soma em vez de substituir a lista inteira,
    /// mais previsível do que replicar esse detalhe).
    http_headers: HashMap<String, String>,

    // Estado herdado de `proxy_cache`/`proxy_cache_valid`/`proxy_cache_bypass` — desce de
    // `http` para `server` para `location` como no nginx real: uma location que só tem
    // `proxy_cache_bypass` (sem repetir `proxy_cache`) ainda está cacheada se um nível
    // acima ligou o cache. `cache_enabled` é `false` até algum nível ligar uma zona que
    // não seja "off"; os outros três só importam quando `cache_enabled` é `true`.
    cache_enabled: bool,
    cache_valid_secs: u64,
    cache_bypass_headers: Vec<String>,
    cache_bypass_cookies: Vec<String>,
    cache_bypass_query: Vec<String>,
    /// Snapshot do estado logo após o pré-passe do nível `http` — todo `server` reparte
    /// daqui, não do que o `server` anterior deixou para trás.
    http_cache_baseline: (bool, u64, Vec<String>, Vec<String>, Vec<String>),
}

impl Converter {
    fn new(file: &str, search_root: PathBuf) -> Self {
        // Parte de uma configuração vazia: tudo vem do arquivo de origem.
        let config = FerrixConfig {
            admin: Default::default(),
            http: Default::default(),
            real_ip: Default::default(),
            upstreams: HashMap::new(),
            vhosts: Vec::new(),
        };
        Self {
            config,
            report: MigrationReport::default(),
            file: file.to_string(),
            search_root,
            limit_req_zones: HashMap::new(),
            limit_conn_zones: HashSet::new(),
            synthetic_upstreams: 0,
            route_seq: 0,
            http_headers: HashMap::new(),
            cache_enabled: false,
            cache_valid_secs: 60,
            cache_bypass_headers: Vec::new(),
            cache_bypass_cookies: Vec::new(),
            cache_bypass_query: Vec::new(),
            http_cache_baseline: (false, 60, Vec::new(), Vec::new(), Vec::new()),
        }
    }

    /// Substitui cada `include` pelo conteúdo dos arquivos que ele aponta.
    fn resolve_includes(&mut self, directives: &mut Vec<Directive>, base: &Path, depth: usize) {
        if depth > 16 {
            return;
        }
        let mut index = 0;
        while index < directives.len() {
            if directives[index].name == "include" {
                let d = directives[index].clone();
                let pattern = d.arg(0).unwrap_or("").to_string();
                let expanded = self.expand_include(&pattern, base, &d, depth);
                directives.splice(index..index + 1, expanded.clone());
                index += expanded.len();
                continue;
            }
            if let Some(block) = directives[index].block.as_mut() {
                let mut inner = std::mem::take(block);
                self.resolve_includes(&mut inner, base, depth + 1);
                *block = inner;
            }
            index += 1;
        }
    }

    fn expand_include(
        &mut self,
        pattern: &str,
        base: &Path,
        d: &Directive,
        depth: usize,
    ) -> Vec<Directive> {
        // Snippets sabidamente sem conteúdo para migrar (ver `safe_include_note`) — não
        // precisa nem tentar ler o arquivo.
        if let Some(reason) = safe_include_note(pattern) {
            self.report
                .note(NoteLevel::Converted, &self.file, d.line, "include", &d.source(), reason);
            return Vec::new();
        }

        let mut paths = resolve_include_paths(pattern, base);

        // Um `include` de caminho absoluto (`/etc/nginx/snippets/proxy.conf`) é comum e
        // legítimo em nginx — mas ao migrar a partir de uma cópia da configuração (e não
        // do próprio servidor), esse caminho não existe na máquina onde a importação
        // corre. Antes de desistir, procura pelo nome do arquivo dentro da árvore de onde
        // o `nginx.conf` principal foi lido: é o que uma pessoa faria à mão.
        let mut resolved_by_name = false;
        if paths.is_empty() && !pattern.contains('*') && Path::new(pattern).is_absolute() {
            if let Some(name) = Path::new(pattern).file_name() {
                let matches = find_files_by_name(&self.search_root, name, 0);
                if matches.len() == 1 {
                    paths = matches;
                    resolved_by_name = true;
                }
            }
        }

        if paths.is_empty() {
            self.report.unsupported(
                &self.file,
                d.line,
                "include",
                &d.source(),
                format!(
                    "nenhum arquivo encontrado para '{}'; o conteúdo dele não foi migrado",
                    pattern
                ),
            );
            return Vec::new();
        }

        if resolved_by_name {
            self.report.partial(
                &self.file,
                d.line,
                "include",
                &d.source(),
                format!(
                    "o caminho '{}' não existe nesta máquina; usado '{}', encontrado pelo nome \
                     dentro da pasta importada — confirme se é o arquivo certo",
                    pattern,
                    paths[0].display()
                ),
            );
        }

        let mut result = Vec::new();
        for p in paths {
            let display = p.display().to_string();
            match std::fs::read_to_string(&p) {
                Ok(text) => match parse(&text) {
                    Ok(mut inner) => {
                        self.report.files.push(display);
                        let inner_base = p.parent().unwrap_or(base).to_path_buf();
                        self.resolve_includes(&mut inner, &inner_base, depth + 1);
                        result.extend(inner);
                    }
                    Err(e) => self.report.unsupported(
                        &self.file,
                        d.line,
                        "include",
                        &d.source(),
                        format!("erro de sintaxe em {}: {}", display, e),
                    ),
                },
                Err(e) => self.report.unsupported(
                    &self.file,
                    d.line,
                    "include",
                    &d.source(),
                    format!("não foi possível ler {}: {}", display, e),
                ),
            }
        }
        result
    }

    fn run(&mut self, directives: &[Directive]) {
        // Um arquivo de site do nginx (o que fica em sites-enabled/, o que o Certbot
        // gerencia) é feito de `server { }` e `upstream { }` soltos, sem o `http { }` que
        // só existe no nginx.conf principal — é o `include` que costura os dois. Colar só
        // o arquivo do site, sem esse wrapper, é o caso comum de migração de um site só.
        // Por isso `upstream`/`server`/zonas de limite/timeouts são tratados igual estando
        // soltos no nível superior ou dentro de um bloco `http`.
        self.collect_upstreams_and_zones(directives);
        for d in directives {
            if d.name == "http" {
                if let Some(block) = &d.block {
                    self.collect_upstreams_and_zones(block);
                }
            }
        }

        // Mesma ideia: `proxy_cache`/`proxy_cache_valid`/`proxy_cache_bypass` no nível
        // `http` viram a base herdada por todo `server` — vistos antes de qualquer server
        // ser processado, não importa se vêm antes ou depois deles no arquivo.
        self.collect_http_cache_scope(directives);
        for d in directives {
            if d.name == "http" {
                if let Some(block) = &d.block {
                    self.collect_http_cache_scope(block);
                }
            }
        }
        self.http_cache_baseline = (
            self.cache_enabled,
            self.cache_valid_secs,
            self.cache_bypass_headers.clone(),
            self.cache_bypass_cookies.clone(),
            self.cache_bypass_query.clone(),
        );

        for d in directives {
            match d.name.as_str() {
                "http" => {
                    if let Some(block) = &d.block {
                        for item in block {
                            self.dispatch_top_level(item);
                        }
                    }
                }
                "events" => {
                    self.report.note(
                        NoteLevel::Converted,
                        &self.file,
                        d.line,
                        "events",
                        "events { ... }",
                        "o runtime assíncrono do Ferrix dimensiona as conexões sozinho",
                    );
                }
                "stream" => self.report.unsupported(
                    &self.file,
                    d.line,
                    "stream",
                    "stream { ... }",
                    "proxy TCP/UDP não existe no Ferrix; o bloco inteiro ficou de fora",
                ),
                "load_module" | "worker_processes" | "pid" | "user" | "daemon" => {
                    self.report.note(
                        NoteLevel::Converted,
                        &self.file,
                        d.line,
                        &d.name,
                        &d.source(),
                        "detalhe de processo do nginx, sem equivalente necessário",
                    );
                }
                _ => self.dispatch_top_level(d),
            }
        }

        // Sem nenhum `server`, o resultado não subiria: a validação exige um host.
        if self.config.vhosts.is_empty() {
            self.config.vhosts.push(VirtualHost {
                id: "vhost-importado".to_string(),
                name: "Host padrão (nenhum server encontrado)".to_string(),
                server_names: vec!["*".to_string()],
                listen_port: 8000,
                headers: HashMap::new(),
                rate_limit: None,
                limit_conn: None,
                allowed_ips: Vec::new(),
                denied_ips: Vec::new(),
                basic_auth: None,
                satisfy: Satisfy::All,
                tls: None,
                routes: Vec::new(),
            });
        }

        self.report.vhosts_created = self.config.vhosts.len();
        self.report.upstreams_created = self.config.upstreams.len();
        self.report.routes_created = self.config.vhosts.iter().map(|v| v.routes.len()).sum();

        // Um snippet incluído por várias locations (comum: `proxy.conf` num monte de
        // `location`) entraria repetido uma vez por include. A ordem original não importa
        // aqui — é só a lista de "o que foi lido" — então ordenar e deduplicar deixa o
        // relatório legível sem perder nenhum arquivo.
        self.report.files.sort();
        self.report.files.dedup();
    }

    /// Primeira passada: zonas de `limit_req`/`limit_conn` e pools de `upstream`,
    /// registrados antes de qualquer `server` ser processado.
    ///
    /// A ordem no arquivo não deveria importar — o próprio nginx não exige `upstream`
    /// declarado antes do `server` que o usa — mas o conversor processa em ordem, então
    /// sem esta passada um `server` que aparecesse primeiro no arquivo criaria um
    /// upstream sintético de um servidor só, em vez de apontar para o pool de verdade
    /// declarado mais abaixo.
    fn collect_upstreams_and_zones(&mut self, block: &[Directive]) {
        for d in block {
            match d.name.as_str() {
                "limit_req_zone" => self.collect_limit_req_zone(d),
                "limit_conn_zone" => self.collect_limit_conn_zone(d),
                "upstream" => self.convert_upstream(d),
                _ => {}
            }
        }
    }

    /// Junta todo `proxy_cache`/`proxy_cache_valid`/`proxy_cache_bypass` direto num nível
    /// `http` (ou solto no topo, sem `http{}`) e fixa o resultado em `self.cache_*` — a
    /// base que todo `server` vai herdar antes de processar o que é seu.
    fn collect_http_cache_scope(&mut self, block: &[Directive]) {
        let mut zone: Option<&Directive> = None;
        let mut valid: Vec<&Directive> = Vec::new();
        let mut bypass: Vec<&Directive> = Vec::new();
        for d in block {
            match d.name.as_str() {
                "proxy_cache" => zone = Some(d),
                "proxy_cache_valid" => valid.push(d),
                "proxy_cache_bypass" => bypass.push(d),
                _ => {}
            }
        }
        self.finalize_cache_scope(zone, &valid, &bypass);
    }

    /// Diretivas que valem tanto soltas no nível superior (arquivo de site sem `http{}`)
    /// quanto dentro de um bloco `http{}` de verdade.
    fn dispatch_top_level(&mut self, d: &Directive) {
        match d.name.as_str() {
            "limit_req_zone" | "limit_conn_zone" | "upstream" => {} // já coletados
            "proxy_cache" | "proxy_cache_valid" | "proxy_cache_bypass" => {} // já coletados
            "server" => self.convert_server(d),

            "client_max_body_size" => {
                if let Some(v) = d.arg(0).and_then(parse_size) {
                    self.config.http.client_max_body_size = v;
                }
            }
            "client_header_timeout" => {
                if let Some(v) = d.arg(0).and_then(parse_time_ms) {
                    self.config.http.client_header_timeout_ms = v;
                }
            }
            "client_body_timeout" => {
                if let Some(v) = d.arg(0).and_then(parse_time_ms) {
                    self.config.http.client_body_timeout_ms = v;
                }
            }
            "keepalive_timeout" => {
                if let Some(v) = d.arg(0).and_then(parse_time_ms) {
                    self.config.http.keepalive_timeout_ms = v;
                }
            }
            "proxy_connect_timeout" => {
                if let Some(v) = d.arg(0).and_then(parse_time_ms) {
                    self.config.http.proxy_connect_timeout_ms = v;
                }
            }
            "proxy_read_timeout" => {
                if let Some(v) = d.arg(0).and_then(parse_time_ms) {
                    self.config.http.proxy_read_timeout_ms = v;
                }
            }
            "proxy_send_timeout" => {
                if let Some(v) = d.arg(0).and_then(parse_time_ms) {
                    self.config.http.proxy_send_timeout_ms = v;
                }
            }

            "set_real_ip_from" => {
                if let Some(v) = d.arg(0) {
                    self.config.real_ip.trusted_proxies.push(v.to_string());
                }
            }
            "real_ip_header" => {
                self.config.real_ip.header =
                    match d.arg(0).unwrap_or("").to_ascii_lowercase().as_str() {
                        "x-forwarded-for" => RealIpHeader::XForwardedFor,
                        "x-real-ip" => RealIpHeader::XRealIp,
                        other => {
                            self.report.unsupported(
                                &self.file,
                                d.line,
                                "real_ip_header",
                                &d.source(),
                                format!(
                                    "o Ferrix só entende X-Forwarded-For e X-Real-IP, não '{}'",
                                    other
                                ),
                            );
                            RealIpHeader::None
                        }
                    };
            }
            "real_ip_recursive" => {
                self.config.real_ip.recursive = d.arg(0) == Some("on");
            }

            "add_header" => {
                let mut headers = std::mem::take(&mut self.http_headers);
                self.apply_add_header(d, &mut headers);
                self.http_headers = headers;
            }

            "proxy_cache_path" => self.report.note(
                NoteLevel::Converted,
                &self.file,
                d.line,
                "proxy_cache_path",
                &d.source(),
                "o Ferrix não separa por zona nem grava em disco — toda rota com \
                 'proxy_cache' divide uma única tabela em memória; veja a nota em cada \
                 location que usa esta zona",
            ),

            _ => self.note_unsupported(d),
        }
    }

    fn collect_limit_req_zone(&mut self, d: &Directive) {
        // `limit_req_zone $chave zone=nome:10m rate=5r/s;`
        let mut zone_name = None;
        let mut rate = None;
        for arg in &d.args {
            if let Some(v) = arg.strip_prefix("zone=") {
                zone_name = Some(v.split(':').next().unwrap_or(v).to_string());
            } else if let Some(v) = arg.strip_prefix("rate=") {
                rate = parse_rate_per_second(v);
            }
        }
        match (zone_name, rate) {
            (Some(name), Some(r)) => {
                self.limit_req_zones.insert(name, r);
            }
            _ => self.report.unsupported(
                &self.file,
                d.line,
                "limit_req_zone",
                &d.source(),
                "não foi possível ler o nome da zona ou a taxa",
            ),
        }

        // A chave da zona é sempre o IP do cliente no Ferrix.
        if let Some(key) = d.args.first() {
            if key != "$binary_remote_addr" && key != "$remote_addr" {
                self.report.partial(
                    &self.file,
                    d.line,
                    "limit_req_zone",
                    &d.source(),
                    format!(
                        "a chave '{}' foi trocada pelo IP do cliente; o Ferrix não limita por variável arbitrária",
                        key
                    ),
                );
            }
        }
    }

    fn collect_limit_conn_zone(&mut self, d: &Directive) {
        for arg in &d.args {
            if let Some(v) = arg.strip_prefix("zone=") {
                self.limit_conn_zones
                    .insert(v.split(':').next().unwrap_or(v).to_string());
            }
        }
    }

    fn convert_upstream(&mut self, d: &Directive) {
        let Some(name) = d.arg(0) else {
            self.report.unsupported(
                &self.file,
                d.line,
                "upstream",
                &d.source(),
                "bloco sem nome",
            );
            return;
        };
        let Some(block) = &d.block else { return };

        let mut targets = Vec::new();
        let mut algorithm = None;
        let mut has_weights = false;

        for item in block {
            match item.name.as_str() {
                "server" => {
                    if let Some(t) = self.parse_upstream_server(item) {
                        if t.weight > 1 {
                            has_weights = true;
                        }
                        targets.push(t);
                    }
                }
                "least_conn" => algorithm = Some(LoadBalanceAlgorithm::LeastConn),
                "ip_hash" => algorithm = Some(LoadBalanceAlgorithm::IpHash),
                "random" => algorithm = Some(LoadBalanceAlgorithm::Random),
                "hash" => {
                    // `hash $request_uri consistent` distribui por uma variável qualquer;
                    // o Ferrix só sabe distribuir por IP de origem.
                    algorithm = Some(LoadBalanceAlgorithm::IpHash);
                    self.report.partial(
                        &self.file,
                        item.line,
                        "hash",
                        &item.source(),
                        format!(
                            "trocado por ip_hash; o Ferrix não distribui por '{}'",
                            item.arg(0).unwrap_or("variável")
                        ),
                    );
                }
                "keepalive" => self.report.note(
                    NoteLevel::Converted,
                    &self.file,
                    item.line,
                    "keepalive",
                    &item.source(),
                    "o Ferrix reaproveita conexões com o backend automaticamente",
                ),
                _ => self.note_unsupported(item),
            }
        }

        if targets.is_empty() {
            self.report.unsupported(
                &self.file,
                d.line,
                "upstream",
                &d.source(),
                format!("o pool '{}' não tem nenhum server utilizável", name),
            );
            return;
        }

        let algorithm = algorithm.unwrap_or(if has_weights {
            // Pesos sem algoritmo declarado significam rodízio ponderado no nginx.
            LoadBalanceAlgorithm::WeightedRoundRobin
        } else {
            LoadBalanceAlgorithm::RoundRobin
        });

        self.config.upstreams.insert(
            name.to_string(),
            UpstreamConfig {
                name: name.to_string(),
                algorithm,
                targets,
                health_check: None,
                verify_tls: true,
            },
        );
    }

    fn parse_upstream_server(&mut self, d: &Directive) -> Option<UpstreamTarget> {
        let addr = d.arg(0)?;
        let mut target = UpstreamTarget {
            url: normalize_upstream_url(addr),
            weight: 1,
            max_fails: 3,
            fail_timeout_secs: 10,
            backup: false,
            down: false,
            max_conns: None,
        };

        for arg in d.args.iter().skip(1) {
            if let Some(v) = arg.strip_prefix("weight=") {
                target.weight = v.parse().unwrap_or(1);
            } else if let Some(v) = arg.strip_prefix("max_fails=") {
                target.max_fails = v.parse().unwrap_or(3);
            } else if let Some(v) = arg.strip_prefix("fail_timeout=") {
                target.fail_timeout_secs = parse_time_ms(v).map(|ms| ms / 1000).unwrap_or(10);
            } else if let Some(v) = arg.strip_prefix("max_conns=") {
                target.max_conns = v.parse().ok();
            } else if arg == "backup" {
                target.backup = true;
            } else if arg == "down" {
                target.down = true;
            } else if arg.starts_with("slow_start=") {
                self.report.unsupported(
                    &self.file,
                    d.line,
                    "slow_start",
                    &d.source(),
                    "recuperação gradual de um servidor não existe no Ferrix",
                );
            } else if arg == "resolve" {
                self.report.note(
                    NoteLevel::Converted,
                    &self.file,
                    d.line,
                    "resolve",
                    &d.source(),
                    "o Ferrix resolve o nome a cada conexão",
                );
            }
        }

        // `max_fails=0` desliga a contagem de falhas no nginx. No Ferrix o zero marcaria o
        // servidor como fora já na primeira falha, então vira um teto bem alto.
        if target.max_fails == 0 {
            target.max_fails = u32::MAX;
        }

        Some(target)
    }

    fn convert_server(&mut self, d: &Directive) {
        let Some(block) = &d.block else { return };

        // Cada server reparte do que `http` deixou herdado, não do que o server anterior
        // deixou para trás — sem isto, o segundo `server{}` de um arquivo herdaria o
        // cache ligado (ou a lista de bypass) do primeiro.
        (
            self.cache_enabled,
            self.cache_valid_secs,
            self.cache_bypass_headers,
            self.cache_bypass_cookies,
            self.cache_bypass_query,
        ) = self.http_cache_baseline.clone();

        let index = self.config.vhosts.len();
        let mut vhost = VirtualHost {
            id: format!("vhost-{}", index + 1),
            name: String::new(),
            server_names: Vec::new(),
            listen_port: 80,
            headers: self.http_headers.clone(),
            rate_limit: None,
            limit_conn: None,
            allowed_ips: Vec::new(),
            denied_ips: Vec::new(),
            basic_auth: None,
            satisfy: Satisfy::All,
            tls: None,
            routes: Vec::new(),
        };

        let mut server_root: Option<String> = None;
        let mut auth_realm: Option<String> = None;
        let mut server_return: Option<&Directive> = None;
        let mut saw_ssl_listen = false;
        let mut ssl_cert: Option<String> = None;
        let mut ssl_key: Option<String> = None;
        // Preenchido quando o server é o clássico redirecionador HTTP→HTTPS do Certbot —
        // ver `try_convert_certbot_https_redirect`. Substitui inteiramente o `if`/`return
        // 404` que o gerava, então o `return 404` de fallback não é reportado.
        let mut certbot_redirect: Option<RouteRule> = None;
        let mut cache_zone: Option<&Directive> = None;
        let mut cache_valid: Vec<&Directive> = Vec::new();
        let mut cache_bypass: Vec<&Directive> = Vec::new();

        for item in block {
            match item.name.as_str() {
                "listen" => saw_ssl_listen |= self.apply_listen(item, &mut vhost),
                "ssl_certificate" => ssl_cert = item.arg(0).map(|s| s.to_string()),
                "ssl_certificate_key" => ssl_key = item.arg(0).map(|s| s.to_string()),
                "if" => {
                    certbot_redirect = self.try_convert_certbot_https_redirect(item);
                    if certbot_redirect.is_none() {
                        self.note_unsupported(item);
                    }
                }
                "server_name" => {
                    vhost.server_names.extend(item.args.iter().cloned());
                }
                "root" => server_root = item.arg(0).map(|s| s.to_string()),
                "location" => {} // segunda passada, depois de `root` ser conhecido
                "allow" => {
                    if let Some(v) = item.arg(0) {
                        vhost.allowed_ips.push(v.to_string());
                    }
                }
                "deny" => {
                    if let Some(v) = item.arg(0) {
                        vhost.denied_ips.push(v.to_string());
                    }
                }
                "satisfy" => {
                    vhost.satisfy = if item.arg(0) == Some("any") {
                        Satisfy::Any
                    } else {
                        Satisfy::All
                    };
                }
                "auth_basic" => {
                    if item.arg(0) != Some("off") {
                        auth_realm = Some(item.arg(0).unwrap_or("Restrito").to_string());
                    }
                }
                "auth_basic_user_file" => {
                    vhost.basic_auth =
                        Some(self.placeholder_basic_auth(item, auth_realm.as_deref()));
                }
                "add_header" => self.apply_add_header(item, &mut vhost.headers),
                "limit_req" => {
                    vhost.rate_limit = self.parse_limit_req(item);
                }
                "limit_conn" => {
                    vhost.limit_conn = self.parse_limit_conn(item);
                }
                "return" => server_return = Some(item),
                "http2" => {
                    if item.arg(0) != Some("off") {
                        self.report.note(
                            NoteLevel::Converted,
                            &self.file,
                            item.line,
                            "http2",
                            &item.source(),
                            "HTTP/2 já liga sozinho por ALPN sempre que TLS está configurado \
                             neste vhost — não existe (nem precisa) uma flag separada",
                        );
                    }
                }
                "index" => self.report.note(
                    NoteLevel::Converted,
                    &self.file,
                    item.line,
                    "index",
                    &item.source(),
                    "o Ferrix serve index.html automaticamente em diretórios",
                ),
                "proxy_cache" => cache_zone = Some(item),
                "proxy_cache_valid" => cache_valid.push(item),
                "proxy_cache_bypass" => cache_bypass.push(item),
                _ => self.note_unsupported(item),
            }
        }

        self.finalize_cache_scope(cache_zone, &cache_valid, &cache_bypass);
        let server_cache_state = (
            self.cache_enabled,
            self.cache_valid_secs,
            self.cache_bypass_headers.clone(),
            self.cache_bypass_cookies.clone(),
            self.cache_bypass_query.clone(),
        );

        if vhost.server_names.is_empty() {
            vhost.server_names.push("*".to_string());
        }
        vhost.name = format!(
            "{} (porta {})",
            vhost.server_names.join(", "),
            vhost.listen_port
        );

        match (ssl_cert, ssl_key) {
            (Some(cert_path), Some(key_path)) => {
                vhost.tls = Some(TlsConfig { cert_path, key_path });
                if saw_ssl_listen {
                    self.report.note(
                        NoteLevel::Converted,
                        &self.file,
                        d.line,
                        "listen ssl",
                        &d.source(),
                        "TLS habilitado com o certificado encontrado — HTTP/2 já liga sozinho por ALPN",
                    );
                }
            }
            (cert, _key) if saw_ssl_listen => {
                // Tinha `listen ... ssl` mas faltou um dos dois arquivos — a porta migra,
                // mas sem certificado o Ferrix não tem como servir HTTPS nela.
                let missing = if cert.is_none() { "ssl_certificate" } else { "ssl_certificate_key" };
                self.report.partial(
                    &self.file,
                    d.line,
                    "listen ssl",
                    &d.source(),
                    format!(
                        "faltou '{}' — a porta foi migrada mas sem TLS o tráfego será HTTP",
                        missing
                    ),
                );
            }
            _ => {}
        }

        // Agora as locations, que podem herdar o `root` do server.
        for item in block {
            if item.name == "location" {
                // Cada location reparte do que este server deixou herdado — não do que a
                // location anterior sobrescreveu, senão um `proxy_cache_bypass` numa
                // location vazaria para a próxima que nunca pediu isso.
                (
                    self.cache_enabled,
                    self.cache_valid_secs,
                    self.cache_bypass_headers,
                    self.cache_bypass_cookies,
                    self.cache_bypass_query,
                ) = server_cache_state.clone();

                if let Some(route) = self.convert_location(item, server_root.as_deref()) {
                    vhost.routes.push(route);
                }
            }
        }

        // O redirect do Certbot substitui o `if`/`return 404` inteiro; um `return` comum
        // no nível do server vira uma rota que pega tudo, como antes.
        if let Some(route) = certbot_redirect {
            vhost.routes.push(route);
        } else if let Some(item) = server_return {
            if let Some(mut route) = self.convert_return(item) {
                route.path_prefix = "/".to_string();
                route.match_type = RouteMatchType::PrefixLiteral;
                vhost.routes.push(route);
            }
        }

        if vhost.routes.is_empty() {
            if let Some(root) = &server_root {
                // Um server só com `root` serve arquivos a partir da raiz.
                vhost.routes.push(RouteRule {
                    id: self.next_route_id(),
                    path_prefix: "/".to_string(),
                    match_type: RouteMatchType::PrefixLiteral,
                    target_type: RouteTargetType::StaticFiles,
                    static_root: Some(root.clone()),
                    websocket_enabled: false,
                    ..Default::default()
                });
            }
        }

        self.config.vhosts.push(vhost);
    }

    /// Devolve `true` quando este `listen` pediu `ssl` — quem chama decide a nota certa
    /// depois de saber se um certificado foi encontrado no resto do server.
    fn apply_listen(&mut self, d: &Directive, vhost: &mut VirtualHost) -> bool {
        if let Some(first) = d.arg(0) {
            if let Some(port) = parse_listen_port(first) {
                vhost.listen_port = port;
            }
        }
        let mut saw_ssl = false;
        for arg in &d.args {
            match arg.as_str() {
                "ssl" => saw_ssl = true,
                "quic" | "http3" => self.report.unsupported(
                    &self.file,
                    d.line,
                    "listen quic",
                    &d.source(),
                    "HTTP/3 não existe no Ferrix",
                ),
                "udp" => self.report.unsupported(
                    &self.file,
                    d.line,
                    "listen udp",
                    &d.source(),
                    "UDP não existe no Ferrix",
                ),
                "http2" => self.report.note(
                    NoteLevel::Converted,
                    &self.file,
                    d.line,
                    "listen http2",
                    &d.source(),
                    "HTTP/2 já liga sozinho por ALPN sempre que TLS está configurado — não \
                     precisa desta flag",
                ),
                _ => {}
            }
        }
        saw_ssl
    }

    /// Reconhece `if ($host = X) { return 301 https://$host$request_uri; }` — o jeito que
    /// o Certbot redireciona HTTP para HTTPS. A condição é sempre verdadeira na prática:
    /// o server já só é alcançado quando o `Host` bate com um `server_name` seu, então
    /// checar `$host` de novo aqui não filtra nada — só um redirect incondicional.
    fn try_convert_certbot_https_redirect(&mut self, if_directive: &Directive) -> Option<RouteRule> {
        // A forma exata que o Certbot gera: sem espaço depois do "(", o lexer devolve
        // "($host" como uma palavra só.
        if if_directive.args.len() != 3
            || if_directive.args[0] != "($host"
            || if_directive.args[1] != "="
        {
            return None;
        }

        let block = if_directive.block.as_ref()?;
        let [inner] = block.as_slice() else { return None };
        if inner.name != "return" {
            return None;
        }

        let mut route = self.convert_return(inner)?;
        self.report.note(
            NoteLevel::Converted,
            &self.file,
            if_directive.line,
            "if",
            &if_directive.source(),
            "condição simplificada: este server já só responde por este host, então o \
             redirect vira incondicional — o 'return 404' de reserva deixa de ser necessário",
        );
        route.path_prefix = "/".to_string();
        route.match_type = RouteMatchType::PrefixLiteral;
        Some(route)
    }

    /// `access_log off;` — a única forma que o Ferrix consegue migrar de verdade, porque
    /// ele só grava em memória. Um caminho de arquivo customizado não tem para onde ir.
    fn apply_access_log(&mut self, d: &Directive, route: &mut RouteRule) {
        if d.arg(0) == Some("off") {
            route.access_log = false;
            self.report.note(
                NoteLevel::Converted,
                &self.file,
                d.line,
                "access_log",
                &d.source(),
                "esta rota não aparece mais no buffer de logs nem no stream ao vivo — as \
                 métricas agregadas (RPS, contagem de status) continuam contando ela",
            );
        } else {
            self.report.unsupported(
                &self.file,
                d.line,
                "access_log",
                &d.source(),
                "o Ferrix só grava em memória; um arquivo ou formato customizado não tem para onde ir",
            );
        }
    }

    /// `expires` vira `add_header Cache-Control` — o efeito prático que interessa. O
    /// header `Expires` em si (uma data calculada a cada resposta) o Ferrix não gera,
    /// mas todo navegador atual já prioriza `Cache-Control` quando os dois aparecem.
    ///
    /// Chamada só depois do resto da location já ter sido processada, para poder ver se
    /// já existe um `add_header Cache-Control` explícito — não importa se ele veio antes
    /// ou depois do `expires` no arquivo original.
    fn apply_expires(&mut self, d: &Directive, route: &mut RouteRule) {
        let Some(value) = d.arg(0) else { return };

        if value == "off" {
            // É o padrão do próprio Ferrix também — nada a converter.
            return;
        }

        let computed = if value == "-1" || value == "epoch" {
            "no-cache".to_string()
        } else if let Some(ms) = parse_time_ms(value) {
            format!("max-age={}", ms / 1000)
        } else {
            self.report.unsupported(
                &self.file,
                d.line,
                "expires",
                &d.source(),
                format!(
                    "não entendi o valor '{}' (entendo 'off', '-1', 'epoch' e durações como '7d')",
                    value
                ),
            );
            return;
        };

        match route.add_headers.get("Cache-Control").cloned() {
            None => {
                route.add_headers.insert("Cache-Control".to_string(), computed);
                self.report.note(
                    NoteLevel::Converted,
                    &self.file,
                    d.line,
                    "expires",
                    &d.source(),
                    "convertido para add_header Cache-Control",
                );
            }
            Some(existing) => {
                let lower = existing.to_ascii_lowercase();
                if lower.contains("max-age") || lower.contains("no-cache") || lower.contains("no-store") {
                    // Já tem duração própria — é mais específico que o que expires pediria.
                    // Nginx mandaria os dois headers (um bug documentado: o cliente escolhe
                    // qual obedecer); aqui fica só o explícito, que é a escolha mais segura.
                    self.report.partial(
                        &self.file,
                        d.line,
                        "expires",
                        &d.source(),
                        format!(
                            "esta rota já tem 'add_header Cache-Control: {}'; o expires foi \
                             ignorado para não mandar dois Cache-Control (o nginx original \
                             mandaria os dois, e o cliente escolhe qual obedecer)",
                            existing
                        ),
                    );
                } else {
                    // O add_header explícito não define duração (ex.: só "public") — soma a
                    // duração do expires, preservando as duas intenções.
                    let merged = format!("{}, {}", existing, computed);
                    route.add_headers.insert("Cache-Control".to_string(), merged);
                    self.report.note(
                        NoteLevel::Converted,
                        &self.file,
                        d.line,
                        "expires",
                        &d.source(),
                        "somado ao add_header Cache-Control já existente, que não tinha duração própria",
                    );
                }
            }
        }
    }

    /// `add_header X-Cache-Status $upstream_cache_status;` é como o nginx expõe se a
    /// resposta veio do cache. O Ferrix já expõe exatamente isso sozinho — todo `HIT`/
    /// `MISS` de uma rota com `proxy_cache` sai no header `X-Cache-Status` — então guardar
    /// o valor literal aqui sobrescreveria esse header de verdade com o texto cru
    /// "$upstream_cache_status", que não significa nada fora do nginx.
    fn apply_add_header(&mut self, item: &Directive, target: &mut HashMap<String, String>) {
        let (Some(k), Some(v)) = (item.arg(0), item.arg(1)) else { return };
        if k.eq_ignore_ascii_case("x-cache-status") && v == "$upstream_cache_status" {
            self.report.note(
                NoteLevel::Converted,
                &self.file,
                item.line,
                "add_header",
                &item.source(),
                "o Ferrix já expõe X-Cache-Status (HIT/MISS) sozinho em toda rota com \
                 cache; este add_header não precisa ser repetido",
            );
            return;
        }
        target.insert(k.to_string(), v.to_string());
    }

    /// Processa `proxy_cache`/`proxy_cache_valid`/`proxy_cache_bypass` vistos num único
    /// nível (http, server ou location) e atualiza o estado herdado (`self.cache_*`), que
    /// quem chamou já deixou pré-carregado com o valor do nível pai — exatamente como o
    /// nginx real: uma location só com `proxy_cache_bypass` continua cacheada se `http`
    /// ou `server` já ligou `proxy_cache`, sem precisar repetir a diretiva.
    ///
    /// O Ferrix não separa por zona nem grava em disco — uma tabela só, compartilhada,
    /// diferenciada pela própria chave (método+host+caminho) — então o nome da zona só
    /// serve para saber que o cache foi pedido; `proxy_cache_path` (nível `http`) é só
    /// anotado, sem efeito.
    fn finalize_cache_scope(
        &mut self,
        zone_item: Option<&Directive>,
        valid_items: &[&Directive],
        bypass_items: &[&Directive],
    ) {
        if let Some(item) = zone_item {
            self.cache_enabled = item.arg(0).is_some_and(|z| z != "off");
            if self.cache_enabled {
                self.report.note(
                    NoteLevel::Converted,
                    &self.file,
                    item.line,
                    "proxy_cache",
                    &item.source(),
                    "cache em memória ligado a partir daqui — o Ferrix usa uma tabela única \
                     compartilhada entre rotas, sem zona nem disco",
                );
            }
        }

        // Pode haver um `proxy_cache_valid` por grupo de status ("200 30s", "404 10s",
        // "any 5s"); o Ferrix só cacheia 200 e só tem uma duração, então usa a entrada
        // que fala de "200" (ou "any", se não houver uma específica) e avisa quando
        // sobra alguma diferença não capturada.
        if let Some(chosen) = valid_items
            .iter()
            .find(|it| it.args.iter().any(|a| a == "200"))
            .or_else(|| valid_items.iter().find(|it| it.args.iter().any(|a| a == "any")))
            .or_else(|| valid_items.first())
        {
            match chosen.args.last().and_then(|s| parse_time_ms(s)) {
                Some(ms) => self.cache_valid_secs = (ms / 1000).max(1),
                None => self.report.partial(
                    &self.file,
                    chosen.line,
                    "proxy_cache_valid",
                    &chosen.source(),
                    format!(
                        "não entendi a duração; usando o padrão do Ferrix de {}s",
                        self.cache_valid_secs
                    ),
                ),
            }
            let codes = &chosen.args[..chosen.args.len().saturating_sub(1)];
            if !codes.is_empty() && !codes.iter().any(|c| c == "200" || c == "any") {
                self.report.partial(
                    &self.file,
                    chosen.line,
                    "proxy_cache_valid",
                    &chosen.source(),
                    "o Ferrix só guarda respostas 200; os demais códigos desta diretiva não têm efeito",
                );
            }
            if valid_items.len() > 1 {
                self.report.partial(
                    &self.file,
                    chosen.line,
                    "proxy_cache_valid",
                    &chosen.source(),
                    format!(
                        "havia {} diretivas 'proxy_cache_valid' neste nível, uma por código \
                         de status; o Ferrix usa uma única duração para tudo (aqui, {}s), \
                         sem diferenciar por status",
                        valid_items.len(),
                        self.cache_valid_secs
                    ),
                );
            }
        }

        if !bypass_items.is_empty() {
            // Redefinir substitui a lista herdada inteira, não soma — mesmo critério do
            // nginx para esta diretiva.
            let mut headers = Vec::new();
            let mut cookies = Vec::new();
            let mut query = Vec::new();
            for item in bypass_items {
                let mut all_recognized = true;
                for arg in &item.args {
                    if let Some(name) = arg.strip_prefix("$http_") {
                        headers.push(name.replace('_', "-"));
                    } else if let Some(name) = arg.strip_prefix("$cookie_") {
                        cookies.push(name.to_string());
                    } else if let Some(name) = arg.strip_prefix("$arg_") {
                        query.push(name.to_string());
                    } else {
                        all_recognized = false;
                    }
                }
                if !all_recognized {
                    self.report.partial(
                        &self.file,
                        item.line,
                        "proxy_cache_bypass",
                        &item.source(),
                        "só variáveis '$http_*', '$cookie_*' e '$arg_*' são reconhecidas; as \
                         demais desta diretiva foram ignoradas",
                    );
                }
            }
            self.cache_bypass_headers = headers;
            self.cache_bypass_cookies = cookies;
            self.cache_bypass_query = query;
        }

        if !self.cache_enabled {
            for item in valid_items {
                self.report.unsupported(
                    &self.file,
                    item.line,
                    "proxy_cache_valid",
                    &item.source(),
                    "sem um 'proxy_cache' ativo (nem aqui, nem em http/server), não há cache para validar",
                );
            }
            for item in bypass_items {
                self.report.unsupported(
                    &self.file,
                    item.line,
                    "proxy_cache_bypass",
                    &item.source(),
                    "sem um 'proxy_cache' ativo (nem aqui, nem em http/server), não há cache para pular",
                );
            }
        }
    }

    fn effective_cache_config(&self) -> Option<CacheConfig> {
        self.cache_enabled.then(|| CacheConfig {
            valid_secs: self.cache_valid_secs,
            bypass_headers: self.cache_bypass_headers.clone(),
            bypass_cookies: self.cache_bypass_cookies.clone(),
            bypass_query_params: self.cache_bypass_query.clone(),
        })
    }

    fn convert_location(&mut self, d: &Directive, server_root: Option<&str>) -> Option<RouteRule> {
        let (match_type, pattern) = parse_location_args(&d.args)?;

        if pattern.starts_with('@') {
            self.report.unsupported(
                &self.file,
                d.line,
                "location @nomeada",
                &d.source(),
                "locations nomeadas, usadas por error_page e try_files, não existem no Ferrix",
            );
            return None;
        }

        let mut route = RouteRule {
            id: self.next_route_id(),
            path_prefix: pattern,
            match_type,
            websocket_enabled: false,
            ..Default::default()
        };

        let Some(block) = &d.block else {
            // `location /x { }` vazia herda o root do server.
            route.target_type = RouteTargetType::StaticFiles;
            route.static_root = server_root.map(|s| s.to_string());
            return route.static_root.is_some().then_some(route);
        };

        let mut has_target = false;
        let mut auth_realm: Option<String> = None;
        // `expires` só é aplicado depois do loop (ver abaixo), porque a ordem no arquivo
        // não é garantida: um `add_header Cache-Control` explícito pode vir antes OU
        // depois dele, e só dá para decidir mesclar ou não depois de ver os dois.
        let mut expires: Option<&Directive> = None;
        // Mesma lógica do `expires`: a ordem entre `proxy_cache`, `proxy_cache_valid` e
        // `proxy_cache_bypass` não é garantida, e só dá para montar a `CacheConfig`
        // depois de ver todo mundo.
        let mut cache_zone: Option<&Directive> = None;
        // `proxy_cache_valid` pode aparecer várias vezes, uma por grupo de status (ex.:
        // "200 30s", "404 10s", "any 5s") — o Ferrix usa uma duração só, então todas são
        // coletadas para `apply_cache` escolher a mais representativa e avisar da perda.
        let mut cache_valid: Vec<&Directive> = Vec::new();
        let mut cache_bypass: Vec<&Directive> = Vec::new();

        for item in block {
            match item.name.as_str() {
                "proxy_pass" => {
                    if self.apply_proxy_pass(item, &mut route) {
                        has_target = true;
                    }
                }
                "root" => {
                    route.target_type = RouteTargetType::StaticFiles;
                    route.static_root = item.arg(0).map(|s| s.to_string());
                    has_target = true;
                }
                "alias" => {
                    // `alias` substitui o prefixo casado pelo caminho, e não o acrescenta.
                    route.target_type = RouteTargetType::StaticFiles;
                    route.static_root = item.arg(0).map(|s| s.to_string());
                    route.strip_prefix = true;
                    has_target = true;
                }
                "try_files" => {
                    route.try_files = Some(item.args.join(" "));
                    if route.target_type != RouteTargetType::ProxyUpstream {
                        route.target_type = RouteTargetType::StaticFiles;
                    }
                }
                "return" => {
                    if let Some(r) = self.convert_return(item) {
                        route.target_type = RouteTargetType::Redirect;
                        route.redirect_url = r.redirect_url;
                        route.redirect_code = r.redirect_code;
                        has_target = true;
                    }
                }
                "proxy_set_header" => {
                    if let (Some(k), Some(v)) = (item.arg(0), item.arg(1)) {
                        // `proxy_set_header Upgrade $http_upgrade;` é o que de fato pede
                        // upgrade de protocolo. Já `Connection` só conta quando o valor diz
                        // "upgrade" — `proxy_set_header Connection "";` é um idioma comum
                        // para forçar keep-alive numa API HTTP normal, e não deve acender
                        // o túnel de WebSocket à toa.
                        let asks_upgrade = k.eq_ignore_ascii_case("upgrade") && !v.is_empty();
                        let connection_upgrades = k.eq_ignore_ascii_case("connection")
                            && v.to_ascii_lowercase().contains("upgrade");

                        if asks_upgrade || connection_upgrades {
                            // O túnel do Ferrix já cuida dos headers de upgrade sozinho;
                            // só liga a flag, não repassa o header em si.
                            route.websocket_enabled = true;
                        } else {
                            route.proxy_set_headers.insert(k.to_string(), v.to_string());
                        }
                    }
                }
                "add_header" => self.apply_add_header(item, &mut route.add_headers),
                "allow" => {
                    if let Some(v) = item.arg(0) {
                        route.allowed_ips.push(v.to_string());
                    }
                }
                "deny" => {
                    if let Some(v) = item.arg(0) {
                        route.denied_ips.push(v.to_string());
                    }
                }
                "satisfy" => {
                    route.satisfy = if item.arg(0) == Some("any") {
                        Satisfy::Any
                    } else {
                        Satisfy::All
                    };
                }
                "auth_basic" => {
                    if item.arg(0) != Some("off") {
                        auth_realm = Some(item.arg(0).unwrap_or("Restrito").to_string());
                    }
                }
                "auth_basic_user_file" => {
                    route.basic_auth =
                        Some(self.placeholder_basic_auth(item, auth_realm.as_deref()));
                }
                "limit_req" => route.rate_limit = self.parse_limit_req(item),
                "limit_conn" => route.limit_conn = self.parse_limit_conn(item),
                "proxy_connect_timeout" => {
                    route.proxy_connect_timeout_ms = item.arg(0).and_then(parse_time_ms);
                }
                "proxy_read_timeout" => {
                    route.proxy_read_timeout_ms = item.arg(0).and_then(parse_time_ms);
                }
                "proxy_send_timeout" => {
                    route.proxy_send_timeout_ms = item.arg(0).and_then(parse_time_ms);
                }
                "proxy_next_upstream" => {
                    route.proxy_next_upstream = Some(self.parse_next_upstream(item));
                }
                "proxy_next_upstream_tries" => {
                    let cfg = route
                        .proxy_next_upstream
                        .get_or_insert_with(Default::default);
                    cfg.tries = item.arg(0).and_then(|v| v.parse().ok());
                }
                "proxy_next_upstream_timeout" => {
                    let cfg = route
                        .proxy_next_upstream
                        .get_or_insert_with(Default::default);
                    cfg.timeout_ms = item.arg(0).and_then(parse_time_ms);
                }
                "proxy_http_version" | "proxy_buffering" | "proxy_request_buffering" => {
                    self.report.note(
                        NoteLevel::Converted,
                        &self.file,
                        item.line,
                        &item.name,
                        &item.source(),
                        "o Ferrix já usa HTTP/1.1 e repassa a resposta em streaming",
                    );
                }
                "access_log" => self.apply_access_log(item, &mut route),
                "expires" => expires = Some(item),
                "proxy_cache" => cache_zone = Some(item),
                "proxy_cache_valid" => cache_valid.push(item),
                "proxy_cache_bypass" => cache_bypass.push(item),
                _ => self.note_unsupported(item),
            }
        }

        if !has_target {
            if let Some(root) = server_root {
                route.target_type = RouteTargetType::StaticFiles;
                route.static_root = Some(root.to_string());
            } else if route.try_files.is_some() {
                self.report.unsupported(
                    &self.file,
                    d.line,
                    "location",
                    &d.source(),
                    "a location tem try_files mas nenhum root; defina o diretório no painel",
                );
                return None;
            } else {
                self.report.unsupported(
                    &self.file,
                    d.line,
                    "location",
                    &d.source(),
                    "a location não tem proxy_pass, root, alias nem return; nada para migrar",
                );
                return None;
            }
        }

        if let Some(item) = expires {
            self.apply_expires(item, &mut route);
        }

        self.finalize_cache_scope(cache_zone, &cache_valid, &cache_bypass);
        route.cache = self.effective_cache_config();

        Some(route)
    }

    /// Converte `proxy_pass`. Devolve `false` se não deu para migrar.
    fn apply_proxy_pass(&mut self, d: &Directive, route: &mut RouteRule) -> bool {
        let Some(raw) = d.arg(0) else { return false };

        if raw.contains('$') {
            // No nginx isso exige `resolver` e resolve em runtime; o destino pode ser
            // qualquer coisa, então não há como escolher um upstream na conversão.
            self.report.unsupported(
                &self.file,
                d.line,
                "proxy_pass",
                &d.source(),
                "o destino é definido por variável em runtime; escolha o upstream no painel",
            );
            return false;
        }

        let without_scheme = raw
            .strip_prefix("http://")
            .or_else(|| raw.strip_prefix("https://"));
        let Some(rest) = without_scheme else {
            self.report.unsupported(
                &self.file,
                d.line,
                "proxy_pass",
                &d.source(),
                format!("esquema não suportado em '{}'; use http ou https", raw),
            );
            return false;
        };

        // Barra ao final substitui o prefixo casado — é o `strip_prefix` do Ferrix.
        route.strip_prefix = rest.contains('/');
        let host_part = rest.split('/').next().unwrap_or(rest);

        if let Some(path) = rest.strip_prefix(host_part) {
            if path.len() > 1 {
                self.report.partial(
                    &self.file,
                    d.line,
                    "proxy_pass",
                    &d.source(),
                    format!(
                        "o caminho '{}' do destino foi descartado; o Ferrix só troca o prefixo",
                        path
                    ),
                );
            }
        }

        route.target_type = RouteTargetType::ProxyUpstream;

        if self.config.upstreams.contains_key(host_part) {
            route.upstream_name = Some(host_part.to_string());
            return true;
        }

        // `proxy_pass` direto para um host: cria um pool de um servidor só.
        let scheme = if raw.starts_with("https://") {
            "https"
        } else {
            "http"
        };
        let name = self.synthesize_upstream(host_part, scheme);
        route.upstream_name = Some(name);
        true
    }

    fn synthesize_upstream(&mut self, host: &str, scheme: &str) -> String {
        let base: String = host
            .chars()
            .map(|c| if c.is_alphanumeric() { c } else { '_' })
            .collect();

        let mut name = format!("upstream_{}", base);
        if self.config.upstreams.contains_key(&name) {
            self.synthetic_upstreams += 1;
            name = format!("upstream_{}_{}", base, self.synthetic_upstreams);
        }

        self.config.upstreams.insert(
            name.clone(),
            UpstreamConfig {
                name: name.clone(),
                algorithm: LoadBalanceAlgorithm::RoundRobin,
                targets: vec![UpstreamTarget {
                    url: format!("{}://{}", scheme, host),
                    weight: 1,
                    max_fails: 3,
                    fail_timeout_secs: 10,
                    backup: false,
                    down: false,
                    max_conns: None,
                }],
                health_check: None,
                verify_tls: true,
            },
        );
        name
    }

    fn convert_return(&mut self, d: &Directive) -> Option<RouteRule> {
        let first = d.arg(0)?;

        // `return https://destino` sem código é um 302 implícito.
        if first.starts_with("http://") || first.starts_with("https://") {
            return Some(RouteRule {
                id: self.next_route_id(),
                target_type: RouteTargetType::Redirect,
                redirect_url: Some(first.to_string()),
                redirect_code: Some(302),
                ..Default::default()
            });
        }

        let code: u16 = first.parse().ok()?;
        let target = d.arg(1);

        if (300..400).contains(&code) {
            let Some(url) = target else {
                self.report.unsupported(
                    &self.file,
                    d.line,
                    "return",
                    &d.source(),
                    "redirecionamento sem URL de destino",
                );
                return None;
            };
            if let Some(unknown) = first_unsupported_variable(url) {
                // `$scheme`, `$host` e `$request_uri` — o essencial do padrão HTTP→HTTPS
                // — o Ferrix já interpola no redirect. O resto (`$arg_x`, `$cookie_x`...)
                // não tem onde guardar valor nenhum num redirect estático.
                self.report.unsupported(
                    &self.file,
                    d.line,
                    "return",
                    &d.source(),
                    format!(
                        "o destino usa a variável '{}', que o Ferrix não interpola num redirect",
                        unknown
                    ),
                );
                return None;
            }
            return Some(RouteRule {
                id: self.next_route_id(),
                target_type: RouteTargetType::Redirect,
                redirect_url: Some(url.to_string()),
                redirect_code: Some(code),
                ..Default::default()
            });
        }

        self.report.unsupported(
            &self.file,
            d.line,
            "return",
            &d.source(),
            format!(
                "responder {} com corpo fixo não existe no Ferrix; só redirecionamentos 3xx",
                code
            ),
        );
        None
    }

    fn parse_limit_req(&mut self, d: &Directive) -> Option<RateLimitConfig> {
        let mut zone = None;
        let mut burst = 0;
        let mut mode = RateLimitMode::Queue; // o padrão do nginx enfileira
        let mut delay = None;

        for arg in &d.args {
            if let Some(v) = arg.strip_prefix("zone=") {
                zone = Some(v.split(':').next().unwrap_or(v).to_string());
            } else if let Some(v) = arg.strip_prefix("burst=") {
                burst = v.parse().unwrap_or(0);
            } else if arg == "nodelay" {
                mode = RateLimitMode::NoDelay;
            } else if let Some(v) = arg.strip_prefix("delay=") {
                mode = RateLimitMode::Delay;
                delay = v.parse().ok();
            }
        }

        let zone = zone?;
        let Some(&rate) = self.limit_req_zones.get(&zone) else {
            self.report.unsupported(
                &self.file,
                d.line,
                "limit_req",
                &d.source(),
                format!(
                    "a zona '{}' não foi declarada em nenhum limit_req_zone",
                    zone
                ),
            );
            return None;
        };

        Some(RateLimitConfig {
            requests_per_second: rate,
            burst,
            mode,
            delay,
            status: 429,
            retry_after: None,
        })
    }

    fn parse_limit_conn(&mut self, d: &Directive) -> Option<LimitConnConfig> {
        let zone = d.arg(0)?;
        let max: u32 = d.arg(1)?.parse().ok()?;

        if !self.limit_conn_zones.contains(zone) {
            self.report.partial(
                &self.file,
                d.line,
                "limit_conn",
                &d.source(),
                format!(
                    "a zona '{}' não foi declarada; o limite vale por IP de cliente",
                    zone
                ),
            );
        }

        Some(LimitConnConfig {
            max_connections: max,
            status: 429,
        })
    }

    fn parse_next_upstream(&mut self, d: &Directive) -> ProxyNextUpstreamConfig {
        let mut conditions = Vec::new();

        for arg in &d.args {
            match arg.as_str() {
                "off" => {
                    return ProxyNextUpstreamConfig {
                        conditions: Vec::new(),
                        tries: None,
                        timeout_ms: None,
                    }
                }
                "error" => conditions.push(NextUpstreamCondition::Error),
                "timeout" => conditions.push(NextUpstreamCondition::Timeout),
                "http_500" => conditions.push(NextUpstreamCondition::Http500),
                "http_502" => conditions.push(NextUpstreamCondition::Http502),
                "http_503" => conditions.push(NextUpstreamCondition::Http503),
                "http_504" => conditions.push(NextUpstreamCondition::Http504),
                "http_403" => conditions.push(NextUpstreamCondition::Http403),
                "http_404" => conditions.push(NextUpstreamCondition::Http404),
                other => self.report.partial(
                    &self.file,
                    d.line,
                    "proxy_next_upstream",
                    &d.source(),
                    format!("a condição '{}' não existe no Ferrix e foi ignorada", other),
                ),
            }
        }

        ProxyNextUpstreamConfig {
            conditions,
            tries: None,
            timeout_ms: None,
        }
    }

    /// Fecha a rota com uma senha aleatória quando o `htpasswd` não pode ser convertido.
    ///
    /// O arquivo guarda hashes, então não há como recuperar a senha original. Deixar a
    /// rota sem proteção seria transformar uma migração numa brecha, então ela fica
    /// fechada e o relatório manda redefinir as credenciais no painel.
    fn placeholder_basic_auth(&mut self, d: &Directive, realm: Option<&str>) -> BasicAuthConfig {
        self.report.unsupported(
            &self.file,
            d.line,
            "auth_basic_user_file",
            &d.source(),
            format!(
                "o arquivo htpasswd guarda hashes e não pode ser convertido{}. \
                 A rota ficou protegida com uma senha aleatória — defina usuário e senha no painel \
                 antes de colocar no ar.",
                realm
                    .map(|r| format!(" (área '{}')", r))
                    .unwrap_or_default()
            ),
        );

        BasicAuthConfig {
            username: "admin".to_string(),
            password: uuid::Uuid::new_v4().simple().to_string(),
        }
    }

    fn next_route_id(&mut self) -> String {
        self.route_seq += 1;
        format!("route-{}", self.route_seq)
    }

    fn note_unsupported(&mut self, d: &Directive) {
        // Um `include` de snippet reconhecido (ver `safe_include_note`) é Converted mesmo
        // ao colar texto na tela, sem acesso ao arquivo — `expand_include` cobre o mesmo
        // caso quando o arquivo existe de verdade (`ferrix import --from`), mas aqui é a
        // única chance: `import_str` nunca resolve includes.
        if d.name == "include" {
            if let Some(reason) = d.arg(0).and_then(safe_include_note) {
                self.report
                    .note(NoteLevel::Converted, &self.file, d.line, "include", &d.source(), reason);
                return;
            }
        }
        // `ssl_ciphers`, `ssl_protocols`, `ssl_dhparam` e companhia não são um gap — o
        // rustls decide essas escolhas sozinho, de propósito, e não existe (nem vai
        // existir) um jeito de sobrescrever. `Ignored` sinaliza isso na UI como uma
        // diferença de design permanente, não como uma pendência de fase futura.
        //
        // Nem todo `ssl_*` se encaixa aqui: `ssl_client_certificate`/`ssl_verify_client`
        // (mTLS) e `ssl_stapling*` (OCSP) são recursos que o Ferrix de fato ainda não tem
        // — um gap real, não uma escolha — então continuam `Unsupported`.
        const RUSTLS_PERMANENT_DEFAULTS: [&str; 8] = [
            "ssl_protocols",
            "ssl_ciphers",
            "ssl_prefer_server_ciphers",
            "ssl_ecdh_curve",
            "ssl_dhparam",
            "ssl_session_cache",
            "ssl_session_timeout",
            "ssl_session_tickets",
        ];
        if RUSTLS_PERMANENT_DEFAULTS.contains(&d.name.as_str()) {
            self.report
                .ignored(&self.file, d.line, &d.name, &d.source(), reason_for(&d.name));
            return;
        }
        self.report.unsupported(
            &self.file,
            d.line,
            &d.name,
            &d.source(),
            reason_for(&d.name),
        );
    }
}

/// Padrões de `include` sem conteúdo para migrar, reconhecidos só pelo caminho — não
/// precisa ler o arquivo (por isso funciona tanto colando texto quanto lendo do disco).
fn safe_include_note(pattern: &str) -> Option<&'static str> {
    if pattern.ends_with("mime.types") {
        Some("tipos MIME são detectados automaticamente pelo Ferrix")
    } else if pattern.ends_with("options-ssl-nginx.conf") {
        Some(
            "é o snippet de TLS do Certbot — o rustls já aplica protocolos e cifras seguros \
             por padrão, sem precisar deste ajuste",
        )
    } else {
        None
    }
}

/// Explicação específica para as diretivas que sabemos não suportar.
fn reason_for(name: &str) -> String {
    let reason = match name {
        n if n.starts_with("gzip") || n == "brotli" => {
            "compressão está planejada para a próxima fase do Ferrix"
        }
        "ssl_protocols" | "ssl_ciphers" | "ssl_prefer_server_ciphers" | "ssl_ecdh_curve"
        | "ssl_dhparam" | "ssl_session_cache" | "ssl_session_timeout" | "ssl_session_tickets" => {
            "o rustls (biblioteca de TLS do Ferrix) já escolhe protocolos, cifras e parâmetros \
             seguros automaticamente; não existe — e não está planejado — ajuste manual \
             disso: é uma limitação definitiva da biblioteca, não um item de fase futura"
        }
        n if n.starts_with("ssl_client_certificate")
            || n.starts_with("ssl_verify_client")
            || n.starts_with("ssl_verify_depth")
            || n.starts_with("ssl_trusted_certificate")
            || n.starts_with("ssl_stapling")
            || n.starts_with("ssl_crl") =>
        {
            "autenticação por certificado de cliente (mTLS) e OCSP stapling ainda não existem \
             no Ferrix — diferente de ssl_ciphers/ssl_protocols, isto é um recurso que falta \
             construir, não uma escolha definitiva"
        }
        n if n.starts_with("ssl_") => "ajuste de TLS sem equivalente no Ferrix",
        "http3" => "HTTP/3 não existe no Ferrix",
        n if n.starts_with("otel_") => "tracing com OpenTelemetry não existe no Ferrix",
        n if n.starts_with("js_") => "njs (JavaScript dentro do servidor) não existe no Ferrix",
        n if n.starts_with("modsecurity") => "WAF não existe no Ferrix",
        n if n.starts_with("open_file_cache") => {
            "o Ferrix não mantém cache de descritores de arquivo"
        }
        n if n.starts_with("fastcgi") || n.starts_with("uwsgi") || n.starts_with("scgi") => {
            "o Ferrix só fala HTTP com o backend"
        }
        "map" => "escolher destino por variável não existe no Ferrix; crie rotas explícitas",
        // Só chega até aqui colando texto na tela: `import_str` não segue `include`, por
        // não ter acesso ao resto dos arquivos. Vindo de `ferrix import --from arquivo`,
        // um include já foi substituído pelo conteúdo antes desta função ser chamada — e
        // se não foi encontrado, a nota vem de `expand_include`, não daqui.
        "include" => {
            "não é seguido ao colar texto na tela; use 'ferrix import --from arquivo.conf' na \
             máquina onde os arquivos estão, que resolve os includes automaticamente"
        }
        "split_clients" => {
            "sorteio de canário por variável não existe; distribua com pesos no upstream"
        }
        "geo" => "classificação de IP em variável não existe; use as listas de allow/deny",
        "rewrite" => "reescrita por expressão regular não existe; use uma rota de redirect",
        "if" => "blocos condicionais não existem no Ferrix",
        "set" => "variáveis definidas na configuração não existem no Ferrix",
        "auth_request" => "autenticação delegada a um subrequest não existe no Ferrix",
        "error_page" => "páginas de erro customizadas ainda não existem",
        "stub_status" => "use /api/metrics e o painel do Ferrix",
        "access_log" | "error_log" | "log_format" => {
            "o Ferrix registra as requisições em memória; veja /api/logs e a tela Live Requests"
        }
        "resolver" => "o Ferrix resolve nomes de backend a cada conexão",
        "internal" => "não existe o conceito de location interna",
        "expires" => "use add_header Cache-Control para o mesmo efeito",
        "sendfile" | "tcp_nopush" | "tcp_nodelay" | "send_timeout" => {
            "detalhe de entrega que o Ferrix trata internamente"
        }
        "default_type" => "o Ferrix detecta o tipo do arquivo automaticamente",
        "proxy_hide_header" | "proxy_ignore_headers" | "proxy_pass_request_body" => {
            "manipulação fina de cabeçalhos do backend ainda não existe"
        }
        "limit_req_status" | "limit_conn_status" | "limit_req_log_level" => {
            "o Ferrix responde 429; o código ainda não é configurável pela importação"
        }
        "keepalive_requests" | "client_body_buffer_size" | "large_client_header_buffers" => {
            "ajuste fino de buffer que o Ferrix administra sozinho"
        }
        _ => "não há equivalente no Ferrix",
    };
    reason.to_string()
}

/// `location [= | ~ | ~* | ^~] padrão`
fn parse_location_args(args: &[String]) -> Option<(RouteMatchType, String)> {
    match args.len() {
        0 => None,
        1 => Some((
            // Sem modificador o nginx compara o prefixo como texto puro.
            RouteMatchType::PrefixLiteral,
            args[0].clone(),
        )),
        _ => {
            let modifier = args[0].as_str();
            let pattern = args[1].clone();
            let match_type = match modifier {
                "=" => RouteMatchType::Exact,
                "~" => RouteMatchType::Regex,
                "~*" => RouteMatchType::RegexCaseInsensitive,
                "^~" => RouteMatchType::PrefixPriority,
                _ => return Some((RouteMatchType::PrefixLiteral, args[0].clone())),
            };
            Some((match_type, pattern))
        }
    }
}

/// `listen 80`, `listen 443 ssl`, `listen [::]:8080`, `listen 127.0.0.1:8000`
fn parse_listen_port(arg: &str) -> Option<u16> {
    if let Ok(port) = arg.parse::<u16>() {
        return Some(port);
    }
    arg.rsplit_once(':').and_then(|(_, p)| p.parse().ok())
}

/// As únicas variáveis que um `redirect_url` sabe interpolar em runtime — ver
/// `proxy::interpolate_redirect_url`. Qualquer outra (`$arg_x`, `$cookie_x`, ...) não tem
/// como ser resolvida num redirect estático.
const SUPPORTED_REDIRECT_VARIABLES: [&str; 3] = ["scheme", "host", "request_uri"];

/// Procura a primeira variável não suportada num destino de redirect. `None` significa
/// que todas as variáveis usadas (se houver alguma) são interpoladas pelo Ferrix.
fn first_unsupported_variable(target: &str) -> Option<String> {
    let mut rest = target;
    while let Some(dollar_at) = rest.find('$') {
        rest = &rest[dollar_at + 1..];
        let name_len = rest
            .find(|c: char| !c.is_alphanumeric() && c != '_')
            .unwrap_or(rest.len());
        let name = &rest[..name_len];
        if !SUPPORTED_REDIRECT_VARIABLES.contains(&name) {
            return Some(format!("${}", name));
        }
        rest = &rest[name_len..];
    }
    None
}

/// Um servidor de upstream do nginx não tem esquema; assume-se HTTP.
fn normalize_upstream_url(addr: &str) -> String {
    if addr.starts_with("http://") || addr.starts_with("https://") {
        addr.to_string()
    } else {
        format!("http://{}", addr)
    }
}

/// `10s`, `100ms`, `1m`, `1h`, ou um número solto (segundos).
pub fn parse_time_ms(value: &str) -> Option<u64> {
    let v = value.trim();
    if v.is_empty() {
        return None;
    }
    let (number, unit) = split_number_unit(v);
    let n: f64 = number.parse().ok()?;

    let ms = match unit {
        "ms" => n,
        "s" | "" => n * 1000.0,
        "m" => n * 60_000.0,
        "h" => n * 3_600_000.0,
        "d" => n * 86_400_000.0,
        _ => return None,
    };
    Some(ms.round() as u64)
}

/// `10m`, `512k`, `1g`, ou um número solto (bytes).
pub fn parse_size(value: &str) -> Option<u64> {
    let v = value.trim();
    if v.is_empty() {
        return None;
    }
    let (number, unit) = split_number_unit(v);
    let n: f64 = number.parse().ok()?;

    let bytes = match unit.to_ascii_lowercase().as_str() {
        "" => n,
        "k" => n * 1024.0,
        "m" => n * 1024.0 * 1024.0,
        "g" => n * 1024.0 * 1024.0 * 1024.0,
        _ => return None,
    };
    Some(bytes.round() as u64)
}

/// `5r/s` ou `30r/m`, sempre devolvido em requisições por segundo.
pub fn parse_rate_per_second(value: &str) -> Option<u32> {
    let v = value.trim();
    let (number, rest) = split_number_unit(v);
    let n: f64 = number.parse().ok()?;

    let per_second = match rest {
        "r/s" => n,
        // Arredonda para cima: um limite de 1 r/s é mais permissivo que zero, e zero
        // seria interpretado como "bloqueie tudo".
        "r/m" => (n / 60.0).max(1.0),
        _ => return None,
    };
    Some(per_second.round().max(1.0) as u32)
}

fn split_number_unit(v: &str) -> (&str, &str) {
    let idx = v
        .find(|c: char| !c.is_ascii_digit() && c != '.')
        .unwrap_or(v.len());
    (&v[..idx], &v[idx..])
}

/// Expande o padrão de um `include`, com suporte a `*` no nome do arquivo.
/// Procura, recursivamente, arquivos cujo nome bate exatamente com `name` dentro de `root`.
///
/// Usada só como último recurso quando um `include` de caminho absoluto não existe na
/// máquina onde a importação corre — o caso comum de migrar a partir de uma cópia da
/// configuração, e não do servidor original. Devolve todos os achados: mais de um é
/// ambíguo, e quem chama decide não adivinhar nesse caso.
fn find_files_by_name(root: &Path, name: &std::ffi::OsStr, depth: usize) -> Vec<PathBuf> {
    if depth > 12 {
        return Vec::new();
    }
    let Ok(entries) = std::fs::read_dir(root) else {
        return Vec::new();
    };

    let mut found = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            found.extend(find_files_by_name(&path, name, depth + 1));
        } else if path.file_name() == Some(name) {
            found.push(path);
        }
    }
    found
}

fn resolve_include_paths(pattern: &str, base: &Path) -> Vec<PathBuf> {
    let joined = if Path::new(pattern).is_absolute() {
        PathBuf::from(pattern)
    } else {
        base.join(pattern)
    };

    let text = joined.to_string_lossy().to_string();
    if !text.contains('*') {
        return if joined.is_file() {
            vec![joined]
        } else {
            Vec::new()
        };
    }

    // Só o caso comum: curinga no nome do arquivo, como `conf.d/*.conf`.
    let Some(parent) = joined.parent() else {
        return Vec::new();
    };
    let Some(file_pattern) = joined.file_name().map(|f| f.to_string_lossy().to_string()) else {
        return Vec::new();
    };
    let Ok(entries) = std::fs::read_dir(parent) else {
        return Vec::new();
    };

    let (prefix, suffix) = match file_pattern.split_once('*') {
        Some((p, s)) => (p.to_string(), s.to_string()),
        None => return Vec::new(),
    };

    let mut found: Vec<PathBuf> = entries
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.is_file())
        .filter(|p| {
            let name = p
                .file_name()
                .map(|f| f.to_string_lossy().to_string())
                .unwrap_or_default();
            name.starts_with(&prefix)
                && name.ends_with(&suffix)
                && name.len() >= prefix.len() + suffix.len()
        })
        .collect();
    // Ordem estável, para que a importação seja reproduzível.
    found.sort();
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    fn convert(src: &str) -> ImportResult {
        import_str(src, "teste.conf").expect("deve parsear")
    }

    fn first_route(r: &ImportResult) -> &RouteRule {
        &r.config.vhosts[0].routes[0]
    }

    // -- Auxiliares de unidade ----------------------------------------------

    #[test]
    fn time_units_are_parsed() {
        assert_eq!(parse_time_ms("10s"), Some(10_000));
        assert_eq!(parse_time_ms("100ms"), Some(100));
        assert_eq!(parse_time_ms("1m"), Some(60_000));
        assert_eq!(parse_time_ms("1h"), Some(3_600_000));
        // Número solto no nginx é segundos.
        assert_eq!(parse_time_ms("30"), Some(30_000));
        assert_eq!(parse_time_ms("lixo"), None);
    }

    #[test]
    fn size_units_are_parsed() {
        assert_eq!(parse_size("1k"), Some(1024));
        assert_eq!(parse_size("10m"), Some(10 * 1024 * 1024));
        assert_eq!(parse_size("1g"), Some(1024 * 1024 * 1024));
        assert_eq!(parse_size("512"), Some(512));
        assert_eq!(parse_size("1M"), Some(1024 * 1024));
    }

    #[test]
    fn rates_are_normalised_to_per_second() {
        assert_eq!(parse_rate_per_second("5r/s"), Some(5));
        assert_eq!(parse_rate_per_second("120r/m"), Some(2));
        // Abaixo de 1 r/s arredonda para 1: zero significaria bloquear tudo.
        assert_eq!(parse_rate_per_second("30r/m"), Some(1));
        assert_eq!(parse_rate_per_second("5"), None);
    }

    #[test]
    fn listen_ports_are_extracted() {
        assert_eq!(parse_listen_port("80"), Some(80));
        assert_eq!(parse_listen_port("127.0.0.1:8000"), Some(8000));
        assert_eq!(parse_listen_port("[::]:8080"), Some(8080));
    }

    #[test]
    fn location_modifiers_map_to_match_types() {
        let f = |args: Vec<&str>| {
            parse_location_args(&args.iter().map(|s| s.to_string()).collect::<Vec<_>>()).unwrap()
        };
        assert_eq!(f(vec!["/api"]).0, RouteMatchType::PrefixLiteral);
        assert_eq!(f(vec!["=", "/exato"]).0, RouteMatchType::Exact);
        assert_eq!(f(vec!["~", r"\.png$"]).0, RouteMatchType::Regex);
        assert_eq!(
            f(vec!["~*", r"\.png$"]).0,
            RouteMatchType::RegexCaseInsensitive
        );
        assert_eq!(f(vec!["^~", "/assets"]).0, RouteMatchType::PrefixPriority);
    }

    // -- Upstreams -----------------------------------------------------------

    #[test]
    fn upstream_with_options_is_converted() {
        let r = convert(
            r#"http {
                upstream backend {
                    least_conn;
                    server app1:3000 weight=3 max_fails=2 fail_timeout=15s;
                    server app2:3000 backup;
                    server app3:3000 down;
                }
                server { listen 80; location / { proxy_pass http://backend; } }
            }"#,
        );

        let u = &r.config.upstreams["backend"];
        assert_eq!(u.algorithm, LoadBalanceAlgorithm::LeastConn);
        assert_eq!(u.targets.len(), 3);
        assert_eq!(u.targets[0].url, "http://app1:3000");
        assert_eq!(u.targets[0].weight, 3);
        assert_eq!(u.targets[0].max_fails, 2);
        assert_eq!(u.targets[0].fail_timeout_secs, 15);
        assert!(u.targets[1].backup);
        assert!(u.targets[2].down);
    }

    #[test]
    fn weights_without_an_algorithm_imply_weighted_round_robin() {
        let r = convert(
            r#"http {
                upstream p { server a:3000 weight=3; server b:3000; }
                server { listen 80; location / { proxy_pass http://p; } }
            }"#,
        );
        assert_eq!(
            r.config.upstreams["p"].algorithm,
            LoadBalanceAlgorithm::WeightedRoundRobin
        );
    }

    #[test]
    fn ip_hash_is_recognised() {
        let r = convert(
            r#"http {
                upstream p { ip_hash; server a:3000; }
                server { listen 80; location / { proxy_pass http://p; } }
            }"#,
        );
        assert_eq!(
            r.config.upstreams["p"].algorithm,
            LoadBalanceAlgorithm::IpHash
        );
    }

    #[test]
    fn hash_by_variable_degrades_to_ip_hash_with_a_note() {
        let r = convert(
            r#"http {
                upstream p { hash $request_uri consistent; server a:3000; }
                server { listen 80; location / { proxy_pass http://p; } }
            }"#,
        );
        assert_eq!(
            r.config.upstreams["p"].algorithm,
            LoadBalanceAlgorithm::IpHash
        );
        assert!(r.report.notes.iter().any(|n| n.directive == "hash"));
    }

    #[test]
    fn max_fails_zero_becomes_effectively_unlimited() {
        // No nginx `max_fails=0` desliga a contagem; zero literal marcaria o servidor
        // como fora logo na primeira falha.
        let r = convert(
            r#"http {
                upstream p { server a:3000 max_fails=0; }
                server { listen 80; location / { proxy_pass http://p; } }
            }"#,
        );
        assert_eq!(r.config.upstreams["p"].targets[0].max_fails, u32::MAX);
    }

    #[test]
    fn proxy_pass_to_a_bare_host_creates_an_upstream() {
        let r = convert(
            r#"http { server { listen 80; location / { proxy_pass http://127.0.0.1:3000; } } }"#,
        );
        assert_eq!(r.config.upstreams.len(), 1);
        let (_name, u) = r.config.upstreams.iter().next().unwrap();
        assert_eq!(u.targets[0].url, "http://127.0.0.1:3000");
        assert!(first_route(&r).upstream_name.is_some());
    }

    // -- Servers e locations -------------------------------------------------

    #[test]
    fn server_names_and_port_are_migrated() {
        let r = convert(
            r#"http { server {
                listen 8080;
                server_name a.com www.a.com;
                location / { proxy_pass http://b:3000; }
            } }"#,
        );
        let v = &r.config.vhosts[0];
        assert_eq!(v.listen_port, 8080);
        assert_eq!(v.server_names, vec!["a.com", "www.a.com"]);
    }

    #[test]
    fn trailing_slash_in_proxy_pass_enables_strip_prefix() {
        let r = convert(
            r#"http {
                upstream b { server app:3000; }
                server { listen 80; location /api/ { proxy_pass http://b/; } }
            }"#,
        );
        assert!(first_route(&r).strip_prefix);
    }

    #[test]
    fn proxy_pass_without_trailing_slash_keeps_the_path() {
        let r = convert(
            r#"http {
                upstream b { server app:3000; }
                server { listen 80; location /api { proxy_pass http://b; } }
            }"#,
        );
        assert!(!first_route(&r).strip_prefix);
    }

    #[test]
    fn root_becomes_a_static_route() {
        let r = convert(
            r#"http { server {
                listen 80;
                location / { root /var/www; try_files $uri $uri/ /index.html; }
            } }"#,
        );
        let route = first_route(&r);
        assert_eq!(route.target_type, RouteTargetType::StaticFiles);
        assert_eq!(route.static_root.as_deref(), Some("/var/www"));
        assert_eq!(route.try_files.as_deref(), Some("$uri $uri/ /index.html"));
    }

    #[test]
    fn alias_strips_the_matched_prefix() {
        let r = convert(r#"http { server { listen 80; location /x/ { alias /srv/x/; } } }"#);
        let route = first_route(&r);
        assert_eq!(route.static_root.as_deref(), Some("/srv/x/"));
        assert!(route.strip_prefix);
    }

    #[test]
    fn server_level_root_is_inherited_by_bare_locations() {
        let r = convert(
            r#"http { server {
                listen 80;
                root /usr/share/nginx/html;
                location /assets/ { }
            } }"#,
        );
        assert_eq!(
            first_route(&r).static_root.as_deref(),
            Some("/usr/share/nginx/html")
        );
    }

    #[test]
    fn return_with_a_url_becomes_a_redirect() {
        let r = convert(
            r#"http { server { listen 80; location /docs { return 301 https://exemplo.com; } } }"#,
        );
        let route = first_route(&r);
        assert_eq!(route.target_type, RouteTargetType::Redirect);
        assert_eq!(route.redirect_code, Some(301));
        assert_eq!(route.redirect_url.as_deref(), Some("https://exemplo.com"));
    }

    /// O clássico redirect HTTP→HTTPS usa exatamente as três variáveis que o motor do
    /// Ferrix interpola num redirect — ver `proxy::interpolate_redirect_url`.
    #[test]
    fn return_with_supported_variables_is_converted() {
        let r = convert(r#"http { server { listen 80; return 301 https://$host$request_uri; } }"#);
        let route = first_route(&r);
        assert_eq!(route.target_type, RouteTargetType::Redirect);
        assert_eq!(route.redirect_url.as_deref(), Some("https://$host$request_uri"));
        assert!(!r.report.notes.iter().any(|n| n.directive == "return"));
    }

    #[test]
    fn return_with_an_unsupported_variable_is_reported_not_guessed() {
        // $arg_* lê a query string — sem equivalente num redirect estático.
        let r = convert(r#"http { server { listen 80; return 301 https://$host/$arg_next; } }"#);
        assert!(r.report.notes.iter().any(|n| n.directive == "return"
            && n.reason.contains("$arg_next")));
    }

    #[test]
    fn return_with_a_body_is_reported() {
        let r =
            convert(r#"http { server { listen 80; location /ok { return 200 "tudo certo"; } } }"#);
        assert!(r
            .report
            .notes
            .iter()
            .any(|n| n.directive == "return" && n.reason.contains("corpo fixo")));
    }

    #[test]
    fn websocket_headers_enable_the_tunnel() {
        let r = convert(
            r#"http {
                upstream b { server app:3000; }
                server { listen 80; location /ws {
                    proxy_pass http://b;
                    proxy_set_header Upgrade $http_upgrade;
                    proxy_set_header Connection "upgrade";
                } }
            }"#,
        );
        assert!(first_route(&r).websocket_enabled);
        // Os headers de upgrade não são repassados literalmente: o túnel cuida disso.
        assert!(!first_route(&r).proxy_set_headers.contains_key("Upgrade"));
    }

    /// `proxy_set_header Connection "";` é o idioma comum para forçar keep-alive numa API
    /// HTTP normal — nada a ver com WebSocket. Antes desta correção, qualquer
    /// `proxy_set_header Connection ...` acendia `websocket_enabled`, mesmo vazio.
    #[test]
    fn clearing_connection_header_does_not_enable_websocket() {
        let r = convert(
            r#"http {
                upstream b { server app:3000; }
                server { listen 80; location /api {
                    proxy_pass http://b;
                    proxy_set_header Connection '';
                } }
            }"#,
        );
        let route = first_route(&r);
        assert!(!route.websocket_enabled);
        // E o header em si precisa continuar indo para o backend — é o que a diretiva pede.
        assert_eq!(route.proxy_set_headers.get("Connection").map(|s| s.as_str()), Some(""));
    }

    #[test]
    fn proxy_set_header_is_carried_over() {
        let r = convert(
            r#"http {
                upstream b { server app:3000; }
                server { listen 80; location / {
                    proxy_pass http://b;
                    proxy_set_header Host $host;
                    proxy_set_header X-Real-IP $remote_addr;
                } }
            }"#,
        );
        let h = &first_route(&r).proxy_set_headers;
        assert_eq!(h.get("Host").map(|s| s.as_str()), Some("$host"));
        assert_eq!(h.get("X-Real-IP").map(|s| s.as_str()), Some("$remote_addr"));
    }

    #[test]
    fn timeouts_are_migrated_per_location() {
        let r = convert(
            r#"http {
                upstream b { server app:3000; }
                server { listen 80; location / {
                    proxy_pass http://b;
                    proxy_read_timeout 3s;
                    proxy_connect_timeout 2s;
                } }
            }"#,
        );
        let route = first_route(&r);
        assert_eq!(route.proxy_read_timeout_ms, Some(3000));
        assert_eq!(route.proxy_connect_timeout_ms, Some(2000));
    }

    #[test]
    fn http_level_settings_are_migrated() {
        let r = convert(
            r#"http {
                client_max_body_size 20m;
                client_header_timeout 10s;
                keepalive_timeout 15s;
                server { listen 80; location / { proxy_pass http://a:1; } }
            }"#,
        );
        assert_eq!(r.config.http.client_max_body_size, 20 * 1024 * 1024);
        assert_eq!(r.config.http.client_header_timeout_ms, 10_000);
        assert_eq!(r.config.http.keepalive_timeout_ms, 15_000);
    }

    #[test]
    fn real_ip_directives_are_migrated() {
        let r = convert(
            r#"http {
                set_real_ip_from 10.0.0.0/8;
                set_real_ip_from 172.16.0.0/12;
                real_ip_header X-Forwarded-For;
                real_ip_recursive on;
                server { listen 80; location / { proxy_pass http://a:1; } }
            }"#,
        );
        assert_eq!(r.config.real_ip.trusted_proxies.len(), 2);
        assert_eq!(r.config.real_ip.header, RealIpHeader::XForwardedFor);
        assert!(r.config.real_ip.recursive);
    }

    // -- Limites -------------------------------------------------------------

    #[test]
    fn limit_req_resolves_its_zone_rate() {
        let r = convert(
            r#"http {
                limit_req_zone $binary_remote_addr zone=basico:10m rate=5r/s;
                upstream b { server app:3000; }
                server { listen 80; location /x {
                    limit_req zone=basico burst=10 nodelay;
                    proxy_pass http://b;
                } }
            }"#,
        );
        let rl = first_route(&r).rate_limit.as_ref().unwrap();
        assert_eq!(rl.requests_per_second, 5);
        assert_eq!(rl.burst, 10);
        assert_eq!(rl.mode, RateLimitMode::NoDelay);
    }

    #[test]
    fn limit_req_without_nodelay_queues_like_nginx() {
        let r = convert(
            r#"http {
                limit_req_zone $binary_remote_addr zone=z:10m rate=5r/s;
                upstream b { server app:3000; }
                server { listen 80; location /x { limit_req zone=z burst=10; proxy_pass http://b; } }
            }"#,
        );
        assert_eq!(
            first_route(&r).rate_limit.as_ref().unwrap().mode,
            RateLimitMode::Queue
        );
    }

    #[test]
    fn limit_req_with_delay_keeps_the_free_allowance() {
        let r = convert(
            r#"http {
                limit_req_zone $binary_remote_addr zone=z:10m rate=5r/s;
                upstream b { server app:3000; }
                server { listen 80; location /x { limit_req zone=z burst=10 delay=5; proxy_pass http://b; } }
            }"#,
        );
        let rl = first_route(&r).rate_limit.as_ref().unwrap();
        assert_eq!(rl.mode, RateLimitMode::Delay);
        assert_eq!(rl.delay, Some(5));
    }

    #[test]
    fn limit_req_with_an_unknown_zone_is_reported() {
        let r = convert(
            r#"http {
                upstream b { server app:3000; }
                server { listen 80; location /x { limit_req zone=fantasma; proxy_pass http://b; } }
            }"#,
        );
        assert!(first_route(&r).rate_limit.is_none());
        assert!(r.report.notes.iter().any(|n| n.directive == "limit_req"));
    }

    #[test]
    fn limit_conn_is_migrated() {
        let r = convert(
            r#"http {
                limit_conn_zone $binary_remote_addr zone=c:10m;
                upstream b { server app:3000; }
                server { listen 80; location /x { limit_conn c 2; proxy_pass http://b; } }
            }"#,
        );
        assert_eq!(
            first_route(&r).limit_conn.as_ref().unwrap().max_connections,
            2
        );
    }

    // -- Controle de acesso --------------------------------------------------

    #[test]
    fn allow_and_deny_are_migrated_per_location() {
        let r = convert(
            r#"http { server {
                listen 80;
                root /www;
                location /interno/ {
                    deny 172.31.0.1;
                    allow 172.31.0.0/16;
                    deny all;
                }
            } }"#,
        );
        let route = first_route(&r);
        assert_eq!(route.allowed_ips, vec!["172.31.0.0/16"]);
        assert_eq!(route.denied_ips, vec!["172.31.0.1", "all"]);
    }

    #[test]
    fn satisfy_any_is_migrated() {
        let r = convert(
            r#"http { server {
                listen 80;
                root /www;
                location /ou/ { satisfy any; allow 10.0.0.0/8; deny all; }
            } }"#,
        );
        assert_eq!(first_route(&r).satisfy, Satisfy::Any);
    }

    #[test]
    fn htpasswd_leaves_the_route_closed_not_open() {
        // O ponto de segurança: uma migração não pode transformar uma rota protegida
        // numa rota pública.
        let r = convert(
            r#"http { server {
                listen 80;
                root /www;
                location /admin/ {
                    auth_basic "Area administrativa";
                    auth_basic_user_file /etc/nginx/htpasswd;
                }
            } }"#,
        );
        let auth = first_route(&r).basic_auth.as_ref();
        assert!(auth.is_some(), "a rota deve continuar protegida");
        assert!(!auth.unwrap().password.is_empty());
        assert!(
            r.report
                .notes
                .iter()
                .any(|n| n.directive == "auth_basic_user_file"
                    && n.reason.contains("senha aleatória"))
        );
    }

    // -- Relatório -----------------------------------------------------------

    #[test]
    fn unsupported_directives_are_reported_with_their_line() {
        let r = convert(
            r#"http {
                gzip on;
                server {
                    listen 80;
                    location / { proxy_pass http://a:1; proxy_no_cache $cookie_nocache; }
                }
            }"#,
        );
        let names = r.report.unsupported_directives();
        assert!(names.contains(&"gzip".to_string()));
        assert!(names.contains(&"proxy_no_cache".to_string()));

        let gzip = r
            .report
            .notes
            .iter()
            .find(|n| n.directive == "gzip")
            .unwrap();
        assert_eq!(gzip.line, 2);
        assert!(gzip.reason.contains("compressão"));
    }

    #[test]
    fn map_and_split_clients_get_specific_explanations() {
        let r = convert(
            r#"http {
                map $a $b { default x; }
                split_clients "$request_id" $s { 10% v2; * v1; }
                server { listen 80; location / { proxy_pass http://a:1; } }
            }"#,
        );
        let map_note = r
            .report
            .notes
            .iter()
            .find(|n| n.directive == "map")
            .unwrap();
        assert!(map_note.reason.contains("rotas explícitas"));
        let split = r
            .report
            .notes
            .iter()
            .find(|n| n.directive == "split_clients")
            .unwrap();
        assert!(split.reason.contains("pesos"));
    }

    #[test]
    fn stream_block_is_reported_as_a_whole() {
        let r = convert(r#"stream { server { listen 5432; proxy_pass db:5432; } }"#);
        assert!(r.report.notes.iter().any(|n| n.directive == "stream"));
    }

    #[test]
    fn counts_are_reported() {
        let r = convert(
            r#"http {
                upstream b { server app:3000; }
                server { listen 80; server_name a.com;
                    location / { proxy_pass http://b; }
                    location /x { proxy_pass http://b; }
                }
            }"#,
        );
        assert_eq!(r.report.vhosts_created, 1);
        assert_eq!(r.report.routes_created, 2);
        assert_eq!(r.report.upstreams_created, 1);
    }

    #[test]
    fn included_snippet_used_by_many_locations_is_listed_once() {
        // O exemplo 13 do learning-NGINX inclui snippets/proxy.conf em nove locations
        // diferentes. Sem a deduplicação, a lista de arquivos lidos repetiria o mesmo
        // caminho nove vezes.
        let path = Path::new("tests/fixtures/nginx/13-loadbalance_avancado/nginx.conf");
        let result = import_file(path).expect("fixture deveria importar");

        let snippet_occurrences = result
            .report
            .files
            .iter()
            .filter(|f| f.ends_with("snippets/proxy.conf"))
            .count();
        assert_eq!(
            snippet_occurrences, 1,
            "arquivos lidos: {:?}",
            result.report.files
        );
    }

    /// Um `include` de caminho absoluto que não existe nesta máquina, mas cujo nome de
    /// arquivo existe em algum lugar dentro da pasta importada, deve ser resolvido — é
    /// exatamente o caso de migrar uma cópia de `/etc/nginx` para outra máquina.
    #[test]
    fn absolute_include_not_found_locally_resolves_by_filename_within_the_import_tree() {
        let path = Path::new("tests/fixtures/nginx/3-timeouts/nginx.conf");
        let result = import_file(path).expect("fixture deveria importar");

        // O snippet declara `proxy_set_header Host $host;` — se ele não tivesse sido
        // encontrado, nenhuma rota teria esse header.
        let found_host_header = result
            .config
            .vhosts
            .iter()
            .flat_map(|v| &v.routes)
            .any(|r| r.proxy_set_headers.get("Host").map(|v| v.as_str()) == Some("$host"));
        assert!(
            found_host_header,
            "o snippet incluído por caminho absoluto não foi encontrado"
        );

        assert!(result
            .report
            .notes
            .iter()
            .any(|n| n.directive == "include" && n.reason.contains("encontrado pelo nome")));
    }

    #[test]
    fn a_config_with_no_server_still_produces_a_valid_shape() {
        let r = convert(r#"http { gzip on; }"#);
        assert_eq!(r.config.vhosts.len(), 1);
    }

    #[test]
    fn multiple_servers_become_multiple_vhosts() {
        let r = convert(
            r#"http {
                server { listen 80; server_name a.com; location / { proxy_pass http://x:1; } }
                server { listen 80; server_name b.com; location / { proxy_pass http://y:1; } }
            }"#,
        );
        assert_eq!(r.config.vhosts.len(), 2);
        assert_eq!(r.config.vhosts[0].server_names, vec!["a.com"]);
        assert_eq!(r.config.vhosts[1].server_names, vec!["b.com"]);
    }

    /// O caso real que motivou este teste: um arquivo de site (o que fica em
    /// `sites-enabled/`, o que o Certbot gerencia) traz `server`/`upstream` soltos, sem o
    /// `http { }` que só existe no nginx.conf principal — quem cola só esse arquivo não
    /// devia ver "server: não há equivalente no Ferrix", porque o Ferrix converte isto sim.
    #[test]
    fn bare_server_and_upstream_without_an_http_wrapper_are_converted() {
        let r = convert(
            r#"
            upstream backend_x {
                server 127.0.0.1:5070;
            }

            server {
                listen 443 ssl;
                server_name x.exemplo.com;
                location / {
                    proxy_pass http://backend_x;
                }
            }
            "#,
        );

        assert_eq!(r.report.vhosts_created, 1);
        assert_eq!(r.report.upstreams_created, 1);
        assert!(r.config.upstreams.contains_key("backend_x"));
        assert_eq!(first_route(&r).upstream_name.as_deref(), Some("backend_x"));

        // Nem "server" nem "upstream" devem aparecer como não migrados.
        let unsupported = r.report.unsupported_directives();
        assert!(!unsupported.contains(&"server".to_string()));
        assert!(!unsupported.contains(&"upstream".to_string()));
    }

    /// Mesmo caso, mas com o `server` aparecendo no arquivo *antes* do `upstream` que ele
    /// referencia — ordem válida em nginx de verdade, que não deve fazer o Ferrix criar um
    /// upstream sintético de um servidor só em vez de apontar para o pool declarado.
    #[test]
    fn a_server_before_its_upstream_still_links_to_the_real_pool() {
        let r = convert(
            r#"
            server {
                listen 80;
                location / { proxy_pass http://backend_y; }
            }

            upstream backend_y {
                server 10.0.0.1:3000;
                server 10.0.0.2:3000;
            }
            "#,
        );

        assert_eq!(r.config.upstreams["backend_y"].targets.len(), 2);
        assert_eq!(first_route(&r).upstream_name.as_deref(), Some("backend_y"));
        // Um upstream sintético "upstream_backend_y" apareceria se a ordem tivesse
        // importado; só deve existir o pool de verdade.
        assert_eq!(r.config.upstreams.len(), 1);
    }

    /// Vários `server` soltos no mesmo arquivo, sem `http{}` — o formato de um vhost do
    /// Certbot com blocos de app, API e WebSocket separados por porta.
    #[test]
    fn multiple_bare_servers_produce_multiple_vhosts() {
        let r = convert(
            r#"
            upstream api_pool { server 127.0.0.1:4000; }

            server {
                listen 443 ssl;
                server_name app.exemplo.com;
                root /var/www/app;
            }

            server {
                listen 4070 ssl;
                server_name app.exemplo.com;
                location / { proxy_pass http://api_pool; }
            }
            "#,
        );

        assert_eq!(r.config.vhosts.len(), 2);
        assert_eq!(r.report.upstreams_created, 1);
    }

    #[test]
    fn route_ids_are_unique_across_vhosts() {
        let r = convert(
            r#"http {
                server { listen 80; server_name a.com; location / { proxy_pass http://x:1; } }
                server { listen 80; server_name b.com; location / { proxy_pass http://y:1; } }
            }"#,
        );
        let a = &r.config.vhosts[0].routes[0].id;
        let b = &r.config.vhosts[1].routes[0].id;
        assert_ne!(a, b);
    }

    /// Só `ssl_certificate`, sem a chave — falta a metade que o Ferrix precisa para
    /// terminar TLS de verdade nesta porta.
    #[test]
    fn listen_ssl_without_a_key_stays_partial() {
        let r = convert(
            r#"http { server {
                listen 443 ssl;
                server_name a.com;
                ssl_certificate /c.pem;
                location / { proxy_pass http://x:1; }
            } }"#,
        );
        assert_eq!(r.config.vhosts[0].listen_port, 443);
        assert!(r.config.vhosts[0].tls.is_none());
        let note = r
            .report
            .notes
            .iter()
            .find(|n| n.directive == "listen ssl")
            .expect("deveria haver uma nota sobre listen ssl");
        assert_eq!(note.level, NoteLevel::Partial);
        assert!(note.reason.contains("ssl_certificate_key"));
    }

    /// O caso que interessa de verdade: certificado e chave presentes — TLS sai
    /// habilitado no vhost, pronto para o Core terminar HTTPS nesta porta.
    #[test]
    fn listen_ssl_with_cert_and_key_enables_tls() {
        let r = convert(
            r#"http { server {
                listen 443 ssl;
                server_name a.com;
                ssl_certificate /etc/certs/fullchain.pem;
                ssl_certificate_key /etc/certs/privkey.pem;
                location / { proxy_pass http://x:1; }
            } }"#,
        );

        let tls = r.config.vhosts[0].tls.as_ref().expect("TLS deveria estar habilitado");
        assert_eq!(tls.cert_path, "/etc/certs/fullchain.pem");
        assert_eq!(tls.key_path, "/etc/certs/privkey.pem");

        let note = r
            .report
            .notes
            .iter()
            .find(|n| n.directive == "listen ssl")
            .expect("deveria haver uma nota sobre listen ssl");
        assert_eq!(note.level, NoteLevel::Converted);
    }

    /// O redirecionador HTTP→HTTPS que o Certbot gera para todo domínio — deve virar um
    /// redirect incondicional, não um "if" e um "return 404" não migrados.
    #[test]
    fn certbot_https_redirect_idiom_becomes_an_unconditional_redirect() {
        let r = convert(
            r#"http { server {
                listen 80;
                server_name urussanga.certasistemas.com.br;

                if ($host = urussanga.certasistemas.com.br) {
                    return 301 https://$host$request_uri;
                }

                return 404;
            } }"#,
        );

        let route = first_route(&r);
        assert_eq!(route.target_type, RouteTargetType::Redirect);
        assert_eq!(route.redirect_code, Some(301));
        assert_eq!(route.redirect_url.as_deref(), Some("https://$host$request_uri"));

        // O "if" ganha uma nota explicando a simplificação (não é "não migrado" — foi
        // convertido, só que de um jeito diferente do texto original). O "return 404" de
        // reserva nem chega a ser reportado: foi absorvido pelo redirect incondicional.
        assert!(r
            .report
            .notes
            .iter()
            .any(|n| n.directive == "if" && n.level == NoteLevel::Converted));
        assert!(!r
            .report
            .notes
            .iter()
            .any(|n| n.directive == "return" && n.level == NoteLevel::Unsupported));
    }

    /// Um `if` que não é exatamente o idioma do Certbot continua reportado como não
    /// migrado — nada de tentar adivinhar condições genéricas.
    #[test]
    fn an_if_that_does_not_match_the_certbot_idiom_stays_unsupported() {
        let r = convert(
            r#"http { server {
                listen 80;
                if ($request_method = POST) {
                    return 405;
                }
                location / { proxy_pass http://x:1; }
            } }"#,
        );
        assert!(r.report.notes.iter().any(|n| n.directive == "if"));
    }

    // -- access_log / expires -------------------------------------------------

    #[test]
    fn access_log_off_in_a_location_disables_the_route_log() {
        let r = convert(
            r#"http { server {
                listen 80;
                location /health {
                    access_log off;
                    proxy_pass http://x:1;
                }
            } }"#,
        );
        let route = first_route(&r);
        assert!(!route.access_log);
        assert!(r
            .report
            .notes
            .iter()
            .any(|n| n.directive == "access_log" && n.level == NoteLevel::Converted));
    }

    #[test]
    fn access_log_with_a_custom_path_stays_unsupported() {
        let r = convert(
            r#"http { server {
                listen 80;
                location / {
                    access_log /var/log/custom.log;
                    proxy_pass http://x:1;
                }
            } }"#,
        );
        let route = first_route(&r);
        assert!(route.access_log);
        assert!(r
            .report
            .notes
            .iter()
            .any(|n| n.directive == "access_log" && n.level == NoteLevel::Unsupported));
    }

    #[test]
    fn expires_duration_becomes_cache_control_max_age() {
        let r = convert(
            r#"http { server {
                listen 80;
                location / {
                    expires 30d;
                    proxy_pass http://x:1;
                }
            } }"#,
        );
        let route = first_route(&r);
        assert_eq!(
            route.add_headers.get("Cache-Control").map(String::as_str),
            Some("max-age=2592000")
        );
    }

    #[test]
    fn expires_minus_one_becomes_no_cache() {
        let r = convert(
            r#"http { server {
                listen 80;
                location / {
                    expires -1;
                    proxy_pass http://x:1;
                }
            } }"#,
        );
        let route = first_route(&r);
        assert_eq!(
            route.add_headers.get("Cache-Control").map(String::as_str),
            Some("no-cache")
        );
    }

    #[test]
    fn expires_off_leaves_no_cache_control_header() {
        let r = convert(
            r#"http { server {
                listen 80;
                location / {
                    expires off;
                    proxy_pass http://x:1;
                }
            } }"#,
        );
        let route = first_route(&r);
        assert!(route.add_headers.get("Cache-Control").is_none());
    }

    #[test]
    fn expires_merges_into_an_add_header_with_no_duration_of_its_own() {
        // Ordem no arquivo original não deve importar: expires antes ou depois do
        // add_header chega ao mesmo resultado.
        let r = convert(
            r#"http { server {
                listen 80;
                location / {
                    add_header Cache-Control "public";
                    expires 7d;
                    proxy_pass http://x:1;
                }
            } }"#,
        );
        let route = first_route(&r);
        assert_eq!(
            route.add_headers.get("Cache-Control").map(String::as_str),
            Some("public, max-age=604800")
        );
    }

    #[test]
    fn expires_defers_to_an_add_header_that_already_has_a_duration() {
        let r = convert(
            r#"http { server {
                listen 80;
                location / {
                    expires -1;
                    add_header Cache-Control "no-cache, no-store, must-revalidate";
                    proxy_pass http://x:1;
                }
            } }"#,
        );
        let route = first_route(&r);
        assert_eq!(
            route.add_headers.get("Cache-Control").map(String::as_str),
            Some("no-cache, no-store, must-revalidate")
        );
        assert!(r
            .report
            .notes
            .iter()
            .any(|n| n.directive == "expires" && n.level == NoteLevel::Partial));
    }

    // -- http2 / TLS ------------------------------------------------------------

    #[test]
    fn bare_http2_directive_is_converted_not_unsupported() {
        let r = convert(
            r#"http { server {
                listen 443 ssl;
                ssl_certificate /a.pem;
                ssl_certificate_key /a.key;
                http2 on;
                location / { proxy_pass http://x:1; }
            } }"#,
        );
        assert!(!r.report.notes.iter().any(|n| n.directive == "http2" && n.level == NoteLevel::Unsupported));
        assert!(r
            .report
            .notes
            .iter()
            .any(|n| n.directive == "http2" && n.level == NoteLevel::Converted));
    }

    #[test]
    fn listen_http2_flag_is_converted_not_silently_dropped() {
        let r = convert(
            r#"http { server {
                listen 443 ssl http2;
                ssl_certificate /a.pem;
                ssl_certificate_key /a.key;
                location / { proxy_pass http://x:1; }
            } }"#,
        );
        assert!(r
            .report
            .notes
            .iter()
            .any(|n| n.directive == "listen http2" && n.level == NoteLevel::Converted));
    }

    #[test]
    fn options_ssl_nginx_conf_include_is_converted_without_needing_the_file() {
        let r = convert(
            r#"http { server {
                listen 443 ssl;
                include /etc/letsencrypt/options-ssl-nginx.conf;
                ssl_certificate /a.pem;
                ssl_certificate_key /a.key;
                location / { proxy_pass http://x:1; }
            } }"#,
        );
        assert!(r
            .report
            .notes
            .iter()
            .any(|n| n.directive == "include" && n.level == NoteLevel::Converted));
    }

    #[test]
    fn ssl_ciphers_reports_a_permanent_limitation_not_a_future_phase() {
        let r = convert(
            r#"http { server {
                listen 443 ssl;
                ssl_certificate /a.pem;
                ssl_certificate_key /a.key;
                ssl_ciphers HIGH:!aNULL;
                location / { proxy_pass http://x:1; }
            } }"#,
        );
        let note = r
            .report
            .notes
            .iter()
            .find(|n| n.directive == "ssl_ciphers")
            .expect("nota esperada");
        assert_eq!(note.level, NoteLevel::Ignored);
        assert!(!note.reason.contains("próxima fase"));
        assert!(note.reason.contains("definitiva"));
    }

    // -- proxy_cache -------------------------------------------------------------

    #[test]
    fn proxy_cache_enables_route_cache_with_the_declared_duration() {
        let r = convert(
            r#"http { server {
                listen 80;
                location / {
                    proxy_cache conteudo;
                    proxy_cache_valid 200 30s;
                    proxy_pass http://x:1;
                }
            } }"#,
        );
        let route = first_route(&r);
        let cfg = route.cache.as_ref().expect("cache deveria estar ligado");
        assert_eq!(cfg.valid_secs, 30);
        assert!(r
            .report
            .notes
            .iter()
            .any(|n| n.directive == "proxy_cache" && n.level == NoteLevel::Converted));
    }

    #[test]
    fn proxy_cache_valid_without_proxy_cache_stays_unsupported() {
        let r = convert(
            r#"http { server {
                listen 80;
                location / {
                    proxy_cache_valid 200 30s;
                    proxy_pass http://x:1;
                }
            } }"#,
        );
        let route = first_route(&r);
        assert!(route.cache.is_none());
        assert!(r
            .report
            .notes
            .iter()
            .any(|n| n.directive == "proxy_cache_valid" && n.level == NoteLevel::Unsupported));
    }

    #[test]
    fn multiple_proxy_cache_valid_picks_the_200_entry_and_reports_the_loss() {
        let r = convert(
            r#"http { server {
                listen 80;
                location / {
                    proxy_cache conteudo;
                    proxy_cache_valid 200 30s;
                    proxy_cache_valid 404 10s;
                    proxy_cache_valid any 5s;
                    proxy_pass http://x:1;
                }
            } }"#,
        );
        let route = first_route(&r);
        assert_eq!(route.cache.as_ref().unwrap().valid_secs, 30);
        assert!(r.report.notes.iter().any(|n| n.directive == "proxy_cache_valid"
            && n.level == NoteLevel::Partial
            && n.reason.contains("3 diretivas")));
    }

    #[test]
    fn proxy_cache_bypass_maps_http_cookie_and_arg_variables() {
        let r = convert(
            r#"http { server {
                listen 80;
                location / {
                    proxy_cache conteudo;
                    proxy_cache_bypass $http_x_purge_cache $cookie_sessao $arg_nocache;
                    proxy_pass http://x:1;
                }
            } }"#,
        );
        let route = first_route(&r);
        let cfg = route.cache.as_ref().unwrap();
        assert_eq!(cfg.bypass_headers, vec!["x-purge-cache".to_string()]);
        assert_eq!(cfg.bypass_cookies, vec!["sessao".to_string()]);
        assert_eq!(cfg.bypass_query_params, vec!["nocache".to_string()]);
    }

    #[test]
    fn proxy_cache_path_at_http_level_is_converted() {
        let r = convert(
            r#"http {
                proxy_cache_path /var/cache/nginx keys_zone=conteudo:10m;
                server {
                    listen 80;
                    location / { proxy_pass http://x:1; }
                }
            }"#,
        );
        assert!(r
            .report
            .notes
            .iter()
            .any(|n| n.directive == "proxy_cache_path" && n.level == NoteLevel::Converted));
    }

    /// O bug real reportado: `proxy_cache_bypass` numa location que não repete
    /// `proxy_cache` (porque a zona já foi ligada lá no `server`) precisa continuar
    /// cacheando, herdando do nível acima — como o nginx real faz.
    #[test]
    fn proxy_cache_at_server_level_is_inherited_by_a_location_with_only_bypass() {
        let r = convert(
            r#"http { server {
                listen 80;
                proxy_cache conteudo;
                proxy_cache_valid 200 30s;
                location /api/ {
                    proxy_cache_bypass $http_upgrade;
                    proxy_pass http://x:1;
                }
            } }"#,
        );
        let route = first_route(&r);
        let cfg = route.cache.as_ref().expect("deveria herdar o cache do server");
        assert_eq!(cfg.valid_secs, 30);
        assert_eq!(cfg.bypass_headers, vec!["upgrade".to_string()]);
        assert!(!r
            .report
            .notes
            .iter()
            .any(|n| n.directive == "proxy_cache_bypass" && n.level == NoteLevel::Unsupported));
    }

    #[test]
    fn proxy_cache_at_http_level_is_inherited_by_every_server() {
        let r = convert(
            r#"http {
                proxy_cache conteudo;
                proxy_cache_valid 200 45s;
                server {
                    listen 80;
                    location / { proxy_pass http://x:1; }
                }
                server {
                    listen 81;
                    location / { proxy_pass http://y:1; }
                }
            }"#,
        );
        assert_eq!(r.config.vhosts.len(), 2);
        for vhost in &r.config.vhosts {
            let cfg = vhost.routes[0]
                .cache
                .as_ref()
                .expect("os dois vhosts deveriam herdar o cache do http");
            assert_eq!(cfg.valid_secs, 45);
        }
    }

    /// Uma location que redefine o bypass não pode vazar essa lista para a próxima
    /// location irmã, que nunca pediu isso — cada location reparte do que o server
    /// deixou, não do que a location anterior deixou.
    #[test]
    fn one_location_bypass_override_does_not_leak_into_the_next_sibling() {
        let r = convert(
            r#"http { server {
                listen 80;
                proxy_cache conteudo;

                location /a {
                    proxy_cache_bypass $cookie_sessao;
                    proxy_pass http://x:1;
                }
                location /b {
                    proxy_pass http://x:1;
                }
            } }"#,
        );
        let vhost = &r.config.vhosts[0];
        let route_a = vhost.routes.iter().find(|r| r.path_prefix == "/a").unwrap();
        let route_b = vhost.routes.iter().find(|r| r.path_prefix == "/b").unwrap();

        assert_eq!(
            route_a.cache.as_ref().unwrap().bypass_cookies,
            vec!["sessao".to_string()]
        );
        assert!(route_b.cache.as_ref().unwrap().bypass_cookies.is_empty());
    }

    #[test]
    fn proxy_cache_off_in_a_location_disables_an_inherited_cache() {
        let r = convert(
            r#"http { server {
                listen 80;
                proxy_cache conteudo;

                location /sem-cache {
                    proxy_cache off;
                    proxy_pass http://x:1;
                }
            } }"#,
        );
        let route = first_route(&r);
        assert!(route.cache.is_none());
    }

    #[test]
    fn add_header_x_cache_status_upstream_variable_is_not_stored_literally() {
        let r = convert(
            r#"http { server {
                listen 80;
                add_header X-Cache-Status $upstream_cache_status always;
                location / { proxy_pass http://x:1; }
            } }"#,
        );
        assert!(!r.config.vhosts[0].headers.contains_key("X-Cache-Status"));
        assert!(r
            .report
            .notes
            .iter()
            .any(|n| n.directive == "add_header" && n.level == NoteLevel::Converted));
    }
}
