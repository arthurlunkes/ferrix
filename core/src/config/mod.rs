pub mod store;
pub mod validate;

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;

/// Tamanho máximo de corpo aceito de um cliente, quando não configurado.
///
/// Generoso de propósito: o objetivo aqui é evitar que um upload sem fim derrube o
/// processo, não impor a política de 1 MB do nginx a quem já roda com o Ferrix sem
/// limite nenhum. Quem quer o comportamento do nginx configura explicitamente.
const DEFAULT_CLIENT_MAX_BODY_SIZE: u64 = 1024 * 1024 * 1024;

/// Acima deste tamanho o corpo passa a ser encaminhado em streaming.
///
/// A troca é real e está documentada: reenviar um stream já consumido é impossível, então
/// requisição acima deste limiar perde o failover para o próximo upstream.
const DEFAULT_PROXY_REQUEST_BUFFER_SIZE: u64 = 1024 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FerrixConfig {
    #[serde(default)]
    pub admin: AdminConfig,
    #[serde(default)]
    pub http: HttpConfig,
    #[serde(default)]
    pub real_ip: RealIpConfig,
    #[serde(default)]
    pub upstreams: HashMap<String, UpstreamConfig>,
    #[serde(default)]
    pub vhosts: Vec<VirtualHost>,
}

// ---------------------------------------------------------------------------
// Administração
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AdminConfig {
    #[serde(default = "default_admin_listen")]
    pub listen: String,
    /// Autenticação da API de administração. Sem isto, quem alcança a porta reescreve a
    /// configuração inteira — o processo se recusa a subir exposto sem autenticação.
    #[serde(default)]
    pub auth: AdminAuthConfig,
    /// Origens permitidas no CORS. Vazio significa same-origin apenas.
    #[serde(default)]
    pub cors_origins: Vec<String>,
}

fn default_admin_listen() -> String {
    // O Ferrix roda atrás do gateway Node (backend/), que ocupa a porta pública 8080 e
    // fala com esta API de administração internamente. 127.0.0.1:8081 é o padrão que não
    // colide com o gateway e, sendo loopback, também é o que dispensa autenticação
    // obrigatória numa instalação nova — ver `validate::validate_admin_auth`.
    "127.0.0.1:8081".to_string()
}

impl Default for AdminConfig {
    fn default() -> Self {
        Self {
            listen: default_admin_listen(),
            auth: AdminAuthConfig::default(),
            cors_origins: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum AdminAuthMode {
    #[default]
    None,
    Token,
    Basic,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AdminAuthConfig {
    #[serde(default)]
    pub mode: AdminAuthMode,
    /// Token de portador, usado quando `mode = token`.
    #[serde(default)]
    pub token: Option<String>,
    #[serde(default)]
    pub username: Option<String>,
    #[serde(default)]
    pub password: Option<String>,
}

impl AdminAuthConfig {
    pub fn is_enabled(&self) -> bool {
        self.mode != AdminAuthMode::None
    }
}

// ---------------------------------------------------------------------------
// HTTP
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HttpConfig {
    #[serde(default = "default_http_listen")]
    pub listen: String,
    #[serde(default = "default_worker_threads")]
    pub worker_threads: usize,

    // Timeouts do cliente — quanto o Ferrix espera por quem está do lado de fora.
    #[serde(default = "default_client_header_timeout_ms")]
    pub client_header_timeout_ms: u64,
    #[serde(default = "default_client_body_timeout_ms")]
    pub client_body_timeout_ms: u64,
    #[serde(default = "default_keepalive_timeout_ms")]
    pub keepalive_timeout_ms: u64,

    // Timeouts do upstream — quanto o Ferrix espera pelo backend. Servem de default para
    // rotas que não definirem os seus.
    #[serde(default = "default_proxy_connect_timeout_ms")]
    pub proxy_connect_timeout_ms: u64,
    #[serde(default = "default_proxy_read_timeout_ms")]
    pub proxy_read_timeout_ms: u64,
    #[serde(default = "default_proxy_send_timeout_ms")]
    pub proxy_send_timeout_ms: u64,

    /// Corpo maior que isto recebe `413 Payload Too Large`.
    #[serde(default = "default_client_max_body_size")]
    pub client_max_body_size: u64,
    /// Acima disto o corpo vai em streaming e a requisição perde o failover.
    #[serde(default = "default_proxy_request_buffer_size")]
    pub proxy_request_buffer_size: u64,
}

fn default_http_listen() -> String {
    "0.0.0.0:8000".to_string()
}
fn default_worker_threads() -> usize {
    std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4)
}
fn default_client_header_timeout_ms() -> u64 {
    60_000
}
fn default_client_body_timeout_ms() -> u64 {
    60_000
}
fn default_keepalive_timeout_ms() -> u64 {
    75_000
}
fn default_proxy_connect_timeout_ms() -> u64 {
    60_000
}
fn default_proxy_read_timeout_ms() -> u64 {
    60_000
}
fn default_proxy_send_timeout_ms() -> u64 {
    60_000
}
fn default_client_max_body_size() -> u64 {
    DEFAULT_CLIENT_MAX_BODY_SIZE
}
fn default_proxy_request_buffer_size() -> u64 {
    DEFAULT_PROXY_REQUEST_BUFFER_SIZE
}

impl Default for HttpConfig {
    fn default() -> Self {
        Self {
            listen: default_http_listen(),
            worker_threads: default_worker_threads(),
            client_header_timeout_ms: default_client_header_timeout_ms(),
            client_body_timeout_ms: default_client_body_timeout_ms(),
            keepalive_timeout_ms: default_keepalive_timeout_ms(),
            proxy_connect_timeout_ms: default_proxy_connect_timeout_ms(),
            proxy_read_timeout_ms: default_proxy_read_timeout_ms(),
            proxy_send_timeout_ms: default_proxy_send_timeout_ms(),
            client_max_body_size: default_client_max_body_size(),
            proxy_request_buffer_size: default_proxy_request_buffer_size(),
        }
    }
}

// ---------------------------------------------------------------------------
// IP real do cliente
// ---------------------------------------------------------------------------

/// De onde tirar o IP do cliente quando o Ferrix está atrás de outro proxy.
///
/// O header só é considerado se o peer da conexão estiver em `trusted_proxies`. Sem essa
/// checagem qualquer cliente forjaria o próprio IP e escaparia de rate limit e ACL.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct RealIpConfig {
    #[serde(default)]
    pub trusted_proxies: Vec<String>,
    #[serde(default)]
    pub header: RealIpHeader,
    /// Anda a cadeia da direita para a esquerda descartando proxies confiáveis.
    #[serde(default)]
    pub recursive: bool,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum RealIpHeader {
    /// Usa sempre o endereço do socket. É o padrão e o comportamento seguro.
    #[default]
    None,
    XForwardedFor,
    XRealIp,
}

// ---------------------------------------------------------------------------
// Upstreams
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum LoadBalanceAlgorithm {
    #[default]
    RoundRobin,
    WeightedRoundRobin,
    LeastConn,
    IpHash,
    Random,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpstreamTarget {
    pub url: String,
    #[serde(default = "default_weight")]
    pub weight: u32,
    #[serde(default = "default_max_fails")]
    pub max_fails: u32,
    #[serde(default = "default_fail_timeout_secs")]
    pub fail_timeout_secs: u64,
    /// Só recebe tráfego quando todos os não-backup estiverem indisponíveis.
    #[serde(default)]
    pub backup: bool,
    /// Retirado da rotação manualmente.
    #[serde(default)]
    pub down: bool,
    /// Teto de conexões simultâneas para este alvo.
    #[serde(default)]
    pub max_conns: Option<u32>,
}

fn default_weight() -> u32 {
    1
}
fn default_max_fails() -> u32 {
    3
}
fn default_fail_timeout_secs() -> u64 {
    10
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HealthCheckConfig {
    #[serde(default = "default_health_path")]
    pub path: String,
    #[serde(default = "default_health_interval")]
    pub interval_secs: u64,
    #[serde(default = "default_health_timeout")]
    pub timeout_secs: u64,
    #[serde(default = "default_health_status")]
    pub expected_status: u16,
}

fn default_health_path() -> String {
    "/health".to_string()
}
fn default_health_interval() -> u64 {
    10
}
fn default_health_timeout() -> u64 {
    3
}
fn default_health_status() -> u16 {
    200
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpstreamConfig {
    pub name: String,
    #[serde(default)]
    pub algorithm: LoadBalanceAlgorithm,
    pub targets: Vec<UpstreamTarget>,
    #[serde(default)]
    pub health_check: Option<HealthCheckConfig>,
    /// Verificar o certificado TLS do backend em upstreams `https://`.
    ///
    /// Era desligado incondicionalmente no código; agora é opt-out explícito por pool.
    #[serde(default = "default_true")]
    pub verify_tls: bool,
}

// ---------------------------------------------------------------------------
// Virtual hosts e rotas
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VirtualHost {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub server_names: Vec<String>,
    #[serde(default = "default_vhost_port")]
    pub listen_port: u16,
    #[serde(default)]
    pub headers: HashMap<String, String>,
    #[serde(default)]
    pub rate_limit: Option<RateLimitConfig>,
    #[serde(default)]
    pub limit_conn: Option<LimitConnConfig>,
    #[serde(default)]
    pub allowed_ips: Vec<String>,
    #[serde(default)]
    pub denied_ips: Vec<String>,
    #[serde(default)]
    pub basic_auth: Option<BasicAuthConfig>,
    /// `All` exige passar no ACL **e** na autenticação; `Any` aceita qualquer um dos dois.
    #[serde(default)]
    pub satisfy: Satisfy,
    /// Certificado para este host. Presente = a porta de `listen_port` termina TLS para
    /// este host (com SNI, se outros hosts na mesma porta também tiverem `tls`).
    #[serde(default)]
    pub tls: Option<TlsConfig>,
    #[serde(default)]
    pub routes: Vec<RouteRule>,
}

fn default_vhost_port() -> u16 {
    8000
}

/// Certificado TLS de um virtual host. A porta correspondente passa a aceitar TLS assim
/// que qualquer host que a declare tiver isto preenchido.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TlsConfig {
    /// Caminho do certificado (ou da cadeia completa) em PEM.
    pub cert_path: String,
    /// Caminho da chave privada em PEM.
    pub key_path: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum Satisfy {
    #[default]
    All,
    Any,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BasicAuthConfig {
    pub username: String,
    pub password: String,
}

/// Como o excedente de uma rajada é tratado, espelhando `limit_req` do nginx.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum RateLimitMode {
    /// Excedente dentro do burst passa na hora. É o comportamento que o Ferrix já tinha,
    /// por isso continua sendo o padrão — mudá-lo alteraria silenciosamente configurações
    /// existentes.
    #[default]
    NoDelay,
    /// Excedente é enfileirado e liberado na taxa configurada (o default do nginx).
    Queue,
    /// As primeiras `delay` requisições do burst passam na hora; o resto é enfileirado.
    Delay,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RateLimitConfig {
    pub requests_per_second: u32,
    pub burst: u32,
    #[serde(default)]
    pub mode: RateLimitMode,
    /// Quantas requisições do burst passam sem espera, quando `mode = delay`.
    #[serde(default)]
    pub delay: Option<u32>,
    #[serde(default = "default_limit_status")]
    pub status: u16,
    /// Valor do header `Retry-After`, em segundos, na resposta de recusa.
    #[serde(default)]
    pub retry_after: Option<u64>,
}

fn default_limit_status() -> u16 {
    429
}

/// Teto de requisições simultâneas por cliente, espelhando `limit_conn`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LimitConnConfig {
    pub max_connections: u32,
    #[serde(default = "default_limit_status")]
    pub status: u16,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RouteTargetType {
    ProxyUpstream,
    StaticFiles,
    Redirect,
}

/// Como `path_prefix` é interpretado, espelhando os modificadores de `location`.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash, Default)]
#[serde(rename_all = "snake_case")]
pub enum RouteMatchType {
    /// Prefixo com fronteira de segmento: `/api` casa `/api` e `/api/x`, mas **não**
    /// `/apifoo`. É o padrão para configuração escrita à mão, por evitar o casamento
    /// acidental que o modo literal permite.
    #[default]
    Prefix,
    /// `location /caminho` do nginx ao pé da letra: prefixo comparado como texto puro,
    /// então `/api` casa com `/apifoo`. É o que o importador de nginx.conf emite, para
    /// que uma configuração migrada se comporte exatamente como se comportava antes.
    PrefixLiteral,
    /// `location = /caminho` — casa o caminho inteiro e nada mais.
    Exact,
    /// `location ^~ /caminho` — prefixo literal que, ao vencer, impede a avaliação de regex.
    PrefixPriority,
    /// `location ~ padrão` — expressão regular sensível a maiúsculas.
    Regex,
    /// `location ~* padrão` — expressão regular insensível a maiúsculas.
    RegexCaseInsensitive,
}

impl RouteMatchType {
    pub fn is_regex(&self) -> bool {
        matches!(
            self,
            RouteMatchType::Regex | RouteMatchType::RegexCaseInsensitive
        )
    }
}

/// Em que situações tentar o próximo servidor do pool, espelhando `proxy_next_upstream`.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum NextUpstreamCondition {
    /// Falha de conexão ou de transporte.
    Error,
    /// Timeout ao conectar ou ao ler.
    Timeout,
    Http500,
    Http502,
    Http503,
    Http504,
    Http403,
    Http404,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProxyNextUpstreamConfig {
    /// Lista vazia equivale a `proxy_next_upstream off`.
    #[serde(default)]
    pub conditions: Vec<NextUpstreamCondition>,
    /// Teto de tentativas, incluindo a primeira. `None` significa tentar todos os candidatos.
    #[serde(default)]
    pub tries: Option<u32>,
    /// Teto de tempo gasto no conjunto de tentativas.
    #[serde(default)]
    pub timeout_ms: Option<u64>,
}

impl Default for ProxyNextUpstreamConfig {
    /// Preserva o comportamento que estava embutido no proxy antes de virar configuração:
    /// repetir em falha de conexão e nos status 502 e 504.
    fn default() -> Self {
        Self {
            conditions: vec![
                NextUpstreamCondition::Error,
                NextUpstreamCondition::Http502,
                NextUpstreamCondition::Http504,
            ],
            tries: None,
            timeout_ms: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RouteRule {
    pub id: String,
    /// O padrão de caminho. Como ele é lido depende de `match_type`.
    pub path_prefix: String,
    #[serde(default)]
    pub match_type: RouteMatchType,
    #[serde(default)]
    pub strip_prefix: bool,
    pub target_type: RouteTargetType,
    #[serde(default)]
    pub upstream_name: Option<String>,
    #[serde(default)]
    pub static_root: Option<String>,
    #[serde(default)]
    pub try_files: Option<String>,
    #[serde(default)]
    pub redirect_url: Option<String>,
    #[serde(default = "default_redirect_code")]
    pub redirect_code: Option<u16>,
    #[serde(default = "default_true")]
    pub websocket_enabled: bool,
    #[serde(default)]
    pub proxy_set_headers: HashMap<String, String>,
    #[serde(default)]
    pub add_headers: HashMap<String, String>,
    #[serde(default)]
    pub rate_limit: Option<RateLimitConfig>,
    #[serde(default)]
    pub limit_conn: Option<LimitConnConfig>,

    // Controle de acesso por rota, equivalente a `allow`/`deny`/`auth_basic` dentro de um
    // `location`. Aplicado **depois** do controle do host: passar no do host não dispensa
    // passar no da rota.
    #[serde(default)]
    pub allowed_ips: Vec<String>,
    #[serde(default)]
    pub denied_ips: Vec<String>,
    #[serde(default)]
    pub basic_auth: Option<BasicAuthConfig>,
    #[serde(default)]
    pub satisfy: Satisfy,

    // Timeouts específicos desta rota; `None` herda o default global de `HttpConfig`.
    #[serde(default)]
    pub proxy_connect_timeout_ms: Option<u64>,
    #[serde(default)]
    pub proxy_read_timeout_ms: Option<u64>,
    #[serde(default)]
    pub proxy_send_timeout_ms: Option<u64>,

    #[serde(default)]
    pub proxy_next_upstream: Option<ProxyNextUpstreamConfig>,
}

fn default_true() -> bool {
    true
}

fn default_redirect_code() -> Option<u16> {
    Some(302)
}

impl RouteRule {
    pub fn next_upstream(&self) -> ProxyNextUpstreamConfig {
        self.proxy_next_upstream.clone().unwrap_or_default()
    }
}

impl Default for RouteRule {
    /// Uma rota vazia para ser preenchida com `..Default::default()`.
    ///
    /// `id`, `path_prefix` e `target_type` sempre são definidos por quem constrói; o resto
    /// tem padrão útil, o que evita repetir uma dúzia de `None` a cada rota criada.
    fn default() -> Self {
        Self {
            id: String::new(),
            path_prefix: "/".to_string(),
            match_type: RouteMatchType::default(),
            strip_prefix: false,
            target_type: RouteTargetType::ProxyUpstream,
            upstream_name: None,
            static_root: None,
            try_files: None,
            redirect_url: None,
            redirect_code: None,
            websocket_enabled: true,
            proxy_set_headers: HashMap::new(),
            add_headers: HashMap::new(),
            rate_limit: None,
            limit_conn: None,
            allowed_ips: Vec::new(),
            denied_ips: Vec::new(),
            basic_auth: None,
            satisfy: Satisfy::All,
            proxy_connect_timeout_ms: None,
            proxy_read_timeout_ms: None,
            proxy_send_timeout_ms: None,
            proxy_next_upstream: None,
        }
    }
}

// ---------------------------------------------------------------------------
// Padrão de fábrica, carga e persistência
// ---------------------------------------------------------------------------

impl Default for FerrixConfig {
    fn default() -> Self {
        let mut upstreams = HashMap::new();
        upstreams.insert(
            "default_backend".to_string(),
            UpstreamConfig {
                name: "default_backend".to_string(),
                algorithm: LoadBalanceAlgorithm::RoundRobin,
                targets: vec![
                    UpstreamTarget {
                        url: "http://127.0.0.1:9001".to_string(),
                        weight: 1,
                        max_fails: 3,
                        fail_timeout_secs: 10,
                        backup: false,
                        down: false,
                        max_conns: None,
                    },
                    UpstreamTarget {
                        url: "http://127.0.0.1:9002".to_string(),
                        weight: 1,
                        max_fails: 3,
                        fail_timeout_secs: 10,
                        backup: false,
                        down: false,
                        max_conns: None,
                    },
                ],
                health_check: Some(HealthCheckConfig {
                    path: "/health".to_string(),
                    interval_secs: 5,
                    timeout_secs: 2,
                    expected_status: 200,
                }),
                verify_tls: true,
            },
        );

        let default_vhost = VirtualHost {
            id: "default-vhost".to_string(),
            name: "Default Virtual Host".to_string(),
            server_names: vec![
                "localhost".to_string(),
                "127.0.0.1".to_string(),
                "*".to_string(),
            ],
            listen_port: 8000,
            headers: {
                let mut h = HashMap::new();
                h.insert("X-Powered-By".to_string(), "Ferrix/0.1.0".to_string());
                h.insert("X-Content-Type-Options".to_string(), "nosniff".to_string());
                h
            },
            rate_limit: None,
            limit_conn: None,
            allowed_ips: Vec::new(),
            denied_ips: Vec::new(),
            basic_auth: None,
            satisfy: Satisfy::All,
            tls: None,
            routes: vec![
                RouteRule {
                    id: "route-api".to_string(),
                    path_prefix: "/api".to_string(),
                    upstream_name: Some("default_backend".to_string()),
                    proxy_set_headers: {
                        let mut p = HashMap::new();
                        p.insert("Host".to_string(), "$host".to_string());
                        p.insert("X-Real-IP".to_string(), "$remote_addr".to_string());
                        p.insert(
                            "X-Forwarded-For".to_string(),
                            "$proxy_add_x_forwarded_for".to_string(),
                        );
                        p.insert("X-Forwarded-Proto".to_string(), "$scheme".to_string());
                        p
                    },
                    ..Default::default()
                },
                RouteRule {
                    id: "route-static".to_string(),
                    path_prefix: "/".to_string(),
                    target_type: RouteTargetType::StaticFiles,
                    static_root: Some("./static_demo".to_string()),
                    try_files: Some("$uri $uri/ /index.html".to_string()),
                    websocket_enabled: false,
                    ..Default::default()
                },
            ],
        };

        Self {
            admin: AdminConfig::default(),
            http: HttpConfig::default(),
            real_ip: RealIpConfig::default(),
            upstreams,
            vhosts: vec![default_vhost],
        }
    }
}

/// O que aconteceu ao abrir o arquivo de configuração.
pub enum ConfigLoad {
    /// Já existia e foi lido.
    Loaded(FerrixConfig),
    /// Não existia e foi criado com os padrões — incluindo um token de administração
    /// gerado na hora, que precisa ser mostrado ao usuário uma única vez.
    Created {
        config: FerrixConfig,
        admin_token: String,
    },
}

impl FerrixConfig {
    pub fn load<P: AsRef<Path>>(path: P) -> Result<Self, String> {
        let p = path.as_ref();
        let data = std::fs::read_to_string(p)
            .map_err(|e| format!("Failed to read config file {}: {}", p.display(), e))?;
        let cfg: Self = serde_json::from_str(&data)
            .map_err(|e| format!("Invalid JSON config {}: {}", p.display(), e))?;
        Ok(cfg)
    }

    pub fn load_or_create<P: AsRef<Path>>(path: P) -> Result<ConfigLoad, String> {
        let p = path.as_ref();

        if p.exists() {
            let data = std::fs::read_to_string(p)
                .map_err(|e| format!("Failed to read config file {}: {}", p.display(), e))?;
            let cfg: Self = serde_json::from_str(&data)
                .map_err(|e| format!("Invalid JSON config {}: {}", p.display(), e))?;
            return Ok(ConfigLoad::Loaded(cfg));
        }

        // A instalação nasce protegida. Deixar a API de administração aberta por padrão
        // e só avisar depois seria o mesmo que não protegê-la: a porta 8080 escuta em
        // 0.0.0.0 e o POST em /api/config reescreve o proxy inteiro.
        let mut default_cfg = Self::default();
        let admin_token = uuid::Uuid::new_v4().simple().to_string();
        default_cfg.admin.auth = AdminAuthConfig {
            mode: AdminAuthMode::Token,
            token: Some(admin_token.clone()),
            username: None,
            password: None,
        };

        let json = serde_json::to_string_pretty(&default_cfg)
            .map_err(|e| format!("Failed to serialize default config: {}", e))?;
        if let Some(parent) = p.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        std::fs::write(p, json)
            .map_err(|e| format!("Failed to write default config to {}: {}", p.display(), e))?;

        Ok(ConfigLoad::Created {
            config: default_cfg,
            admin_token,
        })
    }

    pub fn save_to_file<P: AsRef<Path>>(&self, path: P) -> Result<(), String> {
        let p = path.as_ref();
        let json = serde_json::to_string_pretty(self)
            .map_err(|e| format!("Failed to serialize config: {}", e))?;
        if let Some(parent) = p.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        std::fs::write(p, json)
            .map_err(|e| format!("Failed to write config to {}: {}", p.display(), e))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Uma configuração escrita antes destes campos existirem precisa continuar carregando.
    #[test]
    fn legacy_config_still_deserializes() {
        let legacy = r#"{
            "admin": { "listen": "0.0.0.0:8080" },
            "http": { "listen": "0.0.0.0:8000", "worker_threads": 16 },
            "upstreams": {
                "b": { "name": "b", "algorithm": "round_robin",
                       "targets": [{ "url": "http://127.0.0.1:9001", "weight": 1,
                                     "max_fails": 3, "fail_timeout_secs": 10 }] }
            },
            "vhosts": [{
                "id": "v", "name": "V", "server_names": ["*"], "listen_port": 8000,
                "headers": {}, "rate_limit": null, "allowed_ips": [], "denied_ips": [],
                "basic_auth": null,
                "routes": [{ "id": "r", "path_prefix": "/api", "strip_prefix": false,
                             "target_type": "proxy_upstream", "upstream_name": "b",
                             "websocket_enabled": true, "proxy_set_headers": {},
                             "add_headers": {} }]
            }]
        }"#;

        let cfg: FerrixConfig = serde_json::from_str(legacy).expect("config antiga deve carregar");
        let route = &cfg.vhosts[0].routes[0];

        assert_eq!(route.match_type, RouteMatchType::Prefix);
        assert_eq!(cfg.vhosts[0].satisfy, Satisfy::All);
        assert_eq!(cfg.real_ip.header, RealIpHeader::None);
        assert!(cfg.upstreams["b"].verify_tls);
        assert!(!cfg.upstreams["b"].targets[0].backup);
        assert_eq!(cfg.admin.auth.mode, AdminAuthMode::None);
        assert_eq!(cfg.http.client_max_body_size, DEFAULT_CLIENT_MAX_BODY_SIZE);
    }

    /// O default de retry precisa reproduzir o que estava embutido no código antes.
    #[test]
    fn next_upstream_default_preserves_previous_behaviour() {
        let d = ProxyNextUpstreamConfig::default();
        assert!(d.conditions.contains(&NextUpstreamCondition::Error));
        assert!(d.conditions.contains(&NextUpstreamCondition::Http502));
        assert!(d.conditions.contains(&NextUpstreamCondition::Http504));
        assert!(!d.conditions.contains(&NextUpstreamCondition::Http500));
    }

    #[test]
    fn empty_conditions_mean_retry_disabled() {
        let cfg: ProxyNextUpstreamConfig = serde_json::from_str(r#"{ "conditions": [] }"#).unwrap();
        assert!(cfg.conditions.is_empty());
    }

    #[test]
    fn default_config_roundtrips_through_json() {
        let cfg = FerrixConfig::default();
        let json = serde_json::to_string(&cfg).unwrap();
        let back: FerrixConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(back.vhosts.len(), cfg.vhosts.len());
        assert_eq!(back.upstreams.len(), cfg.upstreams.len());
    }

    /// Uma instalação nova precisa nascer utilizável sem nenhum ajuste manual: o padrão
    /// não pode colidir com a porta pública do gateway (8080) nem exigir configurar
    /// autenticação antes do primeiro `ferrix run` — daí loopback.
    #[test]
    fn default_admin_listen_does_not_collide_with_the_gateway_and_is_loopback() {
        let cfg = FerrixConfig::default();
        assert_eq!(cfg.admin.listen, "127.0.0.1:8081");
        assert!(crate::net::cidr::listen_addr_is_loopback(&cfg.admin.listen));
    }
}
