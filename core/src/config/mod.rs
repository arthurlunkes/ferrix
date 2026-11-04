pub mod store;
pub mod validate;

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;

const DEFAULT_CLIENT_MAX_BODY_SIZE: u64 = 1024 * 1024 * 1024;

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
    #[serde(default)]
    pub acme: AcmeConfig,
    #[serde(default)]
    pub metrics: MetricsConfig,
    #[serde(default)]
    pub streams: Vec<StreamConfig>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum StreamProtocol {
    #[default]
    Tcp,
    Udp,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StreamConfig {
    pub id: String,
    pub listen: String,
    #[serde(default)]
    pub protocol: StreamProtocol,
    pub upstream: String,
    #[serde(default = "default_stream_connect_timeout_ms")]
    pub connect_timeout_ms: u64,
    #[serde(default = "default_stream_idle_timeout_ms")]
    pub idle_timeout_ms: u64,

    /// nginx `ssl_preread on` + `map $ssl_preread_server_name`: TLS server name
    /// (exact, `*.suffix` or `default`) → upstream. Empty = always `upstream`.
    #[serde(default)]
    pub sni_routes: HashMap<String, String>,
}

fn default_stream_connect_timeout_ms() -> u64 {
    5_000
}
fn default_stream_idle_timeout_ms() -> u64 {
    600_000
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct MetricsConfig {
    #[serde(default)]
    pub enabled: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AcmeConfig {
    #[serde(default)]
    pub email: Option<String>,
    #[serde(default = "default_acme_directory")]
    pub directory_url: String,
    #[serde(default = "default_acme_storage")]
    pub storage_dir: String,
    #[serde(default = "default_acme_renew_days")]
    pub renew_before_days: u32,
}

pub const LETS_ENCRYPT_PRODUCTION: &str = "https://acme-v02.api.letsencrypt.org/directory";
pub const LETS_ENCRYPT_STAGING: &str = "https://acme-staging-v02.api.letsencrypt.org/directory";

fn default_acme_directory() -> String {
    LETS_ENCRYPT_PRODUCTION.to_string()
}
fn default_acme_storage() -> String {
    "./acme".to_string()
}
fn default_acme_renew_days() -> u32 {
    30
}

impl Default for AcmeConfig {
    fn default() -> Self {
        Self {
            email: None,
            directory_url: default_acme_directory(),
            storage_dir: default_acme_storage(),
            renew_before_days: default_acme_renew_days(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AdminConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,

    #[serde(default = "default_admin_listen")]
    pub listen: String,

    #[serde(default)]
    pub auth: AdminAuthConfig,

    #[serde(default)]
    pub cors_origins: Vec<String>,

    #[serde(default)]
    pub metrics_public: bool,

    #[serde(default)]
    pub dashboard_dir: Option<String>,
}

fn default_admin_listen() -> String {
    "127.0.0.1:8081".to_string()
}

impl Default for AdminConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            listen: default_admin_listen(),
            auth: AdminAuthConfig::default(),
            cors_origins: Vec::new(),
            metrics_public: false,
            dashboard_dir: None,
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HttpConfig {
    #[serde(default = "default_http_listen")]
    pub listen: String,
    #[serde(default = "default_worker_threads")]
    pub worker_threads: usize,

    #[serde(default)]
    pub worker_cpu_affinity: CpuAffinity,

    #[serde(default = "default_worker_connections")]
    pub worker_connections: u32,

    #[serde(default)]
    pub worker_rlimit_nofile: Option<u64>,

    #[serde(default = "default_true")]
    pub server_tokens: bool,

    #[serde(default = "default_true")]
    pub default_page: bool,

    #[serde(default = "default_client_header_timeout_ms")]
    pub client_header_timeout_ms: u64,
    #[serde(default = "default_client_body_timeout_ms")]
    pub client_body_timeout_ms: u64,
    #[serde(default = "default_keepalive_timeout_ms")]
    pub keepalive_timeout_ms: u64,

    #[serde(default = "default_proxy_connect_timeout_ms")]
    pub proxy_connect_timeout_ms: u64,
    #[serde(default = "default_proxy_read_timeout_ms")]
    pub proxy_read_timeout_ms: u64,
    #[serde(default = "default_proxy_send_timeout_ms")]
    pub proxy_send_timeout_ms: u64,

    #[serde(default = "default_client_max_body_size")]
    pub client_max_body_size: u64,

    #[serde(default = "default_proxy_request_buffer_size")]
    pub proxy_request_buffer_size: u64,

    #[serde(default)]
    pub compression: CompressionConfig,

    #[serde(default)]
    pub static_cache: StaticCacheConfig,

    #[serde(default)]
    pub request_id: bool,

    #[serde(default = "default_protocols")]
    pub protocols: Vec<HttpProtocol>,

    /// nginx `access_log <path> <format>`: log every request to a file.
    #[serde(default)]
    pub access_log: Option<AccessLogConfig>,

    /// nginx `map` / `geo` / `split_clients`: named variables (without `$`).
    #[serde(default)]
    pub variables: HashMap<String, VariableDef>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum VariableDef {
    Map {
        source: String,
        #[serde(default)]
        entries: Vec<MapEntry>,
        #[serde(default)]
        default: String,
    },
    Geo {
        /// Defaults to `$remote_addr`.
        #[serde(default)]
        source: Option<String>,
        #[serde(default)]
        ranges: Vec<GeoRange>,
        #[serde(default)]
        default: String,
    },
    SplitClients {
        source: String,
        buckets: Vec<SplitBucket>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MapEntry {
    /// Exact value, `~regex` or `~*regex`.
    pub pattern: String,
    pub value: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GeoRange {
    pub network: String,
    pub value: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SplitBucket {
    /// Percentage; `0` marks the `*` bucket (the rest).
    pub percent: f64,
    pub value: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum RewriteFlag {
    /// No flag: keep applying the next rewrites.
    #[default]
    None,
    Last,
    Break,
    Redirect,
    Permanent,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RewriteRule {
    pub pattern: String,
    pub replacement: String,
    #[serde(default)]
    pub flag: RewriteFlag,
}

/// nginx `if (condition) { set ...; rewrite ...; return ...; }`.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ConditionConfig {
    /// e.g. `$http_user_agent ~* bot`, `$arg_debug`, `!-f $request_filename`.
    pub condition: String,
    #[serde(default)]
    pub set: Vec<(String, String)>,
    #[serde(default)]
    pub return_code: Option<u16>,
    /// Redirect URL for 3xx, body otherwise.
    #[serde(default)]
    pub return_value: Option<String>,
    #[serde(default)]
    pub rewrite: Option<RewriteRule>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AccessLogConfig {
    pub path: String,
    /// nginx `log_format` string, or `combined` (default).
    #[serde(default)]
    pub format: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum HttpProtocol {
    Http1,
    Http2,
    Http3,
}

fn default_protocols() -> Vec<HttpProtocol> {
    if cfg!(feature = "http2") {
        vec![HttpProtocol::Http1, HttpProtocol::Http2]
    } else {
        vec![HttpProtocol::Http1]
    }
}

impl HttpConfig {
    pub fn serves(&self, protocol: HttpProtocol) -> bool {
        self.protocols.contains(&protocol)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StaticCacheConfig {
    #[serde(default = "default_static_cache_file")]
    pub max_file_bytes: u64,
    #[serde(default = "default_static_cache_total")]
    pub max_total_bytes: u64,
}

fn default_static_cache_file() -> u64 {
    1024 * 1024
}
fn default_static_cache_total() -> u64 {
    64 * 1024 * 1024
}

impl Default for StaticCacheConfig {
    fn default() -> Self {
        Self {
            max_file_bytes: default_static_cache_file(),
            max_total_bytes: default_static_cache_total(),
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum CompressionLevel {
    Fastest,
    #[default]
    Default,
    Best,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CompressionAlgorithm {
    Zstd,
    Br,
    Gzip,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompressionConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub level: CompressionLevel,
    #[serde(default = "default_compression_min_length")]
    pub min_length: u64,
    #[serde(default = "default_compression_algorithms")]
    pub algorithms: Vec<CompressionAlgorithm>,
    #[serde(default = "default_compression_types")]
    pub types: Vec<String>,
}

fn default_compression_min_length() -> u64 {
    1024
}
fn default_compression_algorithms() -> Vec<CompressionAlgorithm> {
    vec![
        CompressionAlgorithm::Zstd,
        CompressionAlgorithm::Br,
        CompressionAlgorithm::Gzip,
    ]
}
fn default_compression_types() -> Vec<String> {
    [
        "text/*",
        "application/json",
        "application/javascript",
        "application/xml",
        "application/xhtml+xml",
        "application/rss+xml",
        "application/atom+xml",
        "application/manifest+json",
        "application/wasm",
        "image/svg+xml",
        "font/ttf",
        "font/otf",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect()
}

impl Default for CompressionConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            level: CompressionLevel::default(),
            min_length: default_compression_min_length(),
            algorithms: default_compression_algorithms(),
            types: default_compression_types(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum CpuAffinity {
    #[default]
    Off,
    Auto,
    Cores(Vec<usize>),
}

impl Serialize for CpuAffinity {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        match self {
            CpuAffinity::Off => s.serialize_str("off"),
            CpuAffinity::Auto => s.serialize_str("auto"),
            CpuAffinity::Cores(c) => c.serialize(s),
        }
    }
}

impl<'de> Deserialize<'de> for CpuAffinity {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Raw {
            Word(String),
            Cores(Vec<usize>),
        }
        match Raw::deserialize(d)? {
            Raw::Word(w) if w.eq_ignore_ascii_case("off") => Ok(CpuAffinity::Off),
            Raw::Word(w) if w.eq_ignore_ascii_case("auto") => Ok(CpuAffinity::Auto),
            Raw::Word(w) => Err(serde::de::Error::custom(format!(
                "worker_cpu_affinity must be \"off\", \"auto\" or a list of cores, not \"{}\"",
                w
            ))),
            Raw::Cores(c) => Ok(CpuAffinity::Cores(c)),
        }
    }
}

fn default_worker_connections() -> u32 {
    4096
}

fn default_http_listen() -> String {
    "0.0.0.0:80".to_string()
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
            worker_cpu_affinity: CpuAffinity::Off,
            worker_connections: default_worker_connections(),
            worker_rlimit_nofile: None,
            server_tokens: true,
            default_page: true,
            client_header_timeout_ms: default_client_header_timeout_ms(),
            client_body_timeout_ms: default_client_body_timeout_ms(),
            keepalive_timeout_ms: default_keepalive_timeout_ms(),
            proxy_connect_timeout_ms: default_proxy_connect_timeout_ms(),
            proxy_read_timeout_ms: default_proxy_read_timeout_ms(),
            proxy_send_timeout_ms: default_proxy_send_timeout_ms(),
            client_max_body_size: default_client_max_body_size(),
            proxy_request_buffer_size: default_proxy_request_buffer_size(),
            compression: CompressionConfig::default(),
            static_cache: StaticCacheConfig::default(),
            request_id: false,
            protocols: default_protocols(),
            access_log: None,
            variables: HashMap::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct RealIpConfig {
    #[serde(default)]
    pub trusted_proxies: Vec<String>,
    #[serde(default)]
    pub header: RealIpHeader,

    #[serde(default)]
    pub recursive: bool,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum RealIpHeader {
    #[default]
    None,
    XForwardedFor,
    XRealIp,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum LoadBalanceAlgorithm {
    #[default]
    RoundRobin,
    WeightedRoundRobin,
    LeastConn,
    IpHash,
    Random,
    ConsistentHash,
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

    #[serde(default)]
    pub backup: bool,

    #[serde(default)]
    pub down: bool,

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

    #[serde(default = "default_true")]
    pub verify_tls: bool,

    #[serde(default)]
    pub hash_key: Option<String>,

    /// nginx `sticky cookie`: keep each client on the same target.
    #[serde(default)]
    pub sticky: Option<StickyConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct StickyConfig {
    pub cookie: String,
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default)]
    pub max_age_secs: Option<u64>,
}

impl StickyConfig {
    pub fn path(&self) -> &str {
        self.path.as_deref().unwrap_or("/")
    }
}

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

    #[serde(default)]
    pub satisfy: Satisfy,

    #[serde(default)]
    pub tls: Option<TlsConfig>,
    #[serde(default)]
    pub routes: Vec<RouteRule>,

    #[serde(default)]
    pub error_pages: HashMap<String, String>,

    /// Server-level nginx `if` blocks (run before route matching).
    #[serde(default)]
    pub conditions: Vec<ConditionConfig>,
    /// Server-level nginx `rewrite` (run before route matching).
    #[serde(default)]
    pub rewrites: Vec<RewriteRule>,
}

fn default_vhost_port() -> u16 {
    0
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct TlsConfig {
    #[serde(default)]
    pub cert_path: String,

    #[serde(default)]
    pub key_path: String,

    #[serde(default)]
    pub acme: bool,
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

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum RateLimitMode {
    #[default]
    NoDelay,

    Queue,

    Delay,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RateLimitConfig {
    pub requests_per_second: u32,
    pub burst: u32,
    #[serde(default)]
    pub mode: RateLimitMode,

    #[serde(default)]
    pub delay: Option<u32>,
    #[serde(default = "default_limit_status")]
    pub status: u16,

    #[serde(default)]
    pub retry_after: Option<u64>,
}

fn default_limit_status() -> u16 {
    429
}

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

    /// nginx `return <code> [text]` with a non-3xx code.
    Return,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash, Default)]
#[serde(rename_all = "snake_case")]
pub enum RouteMatchType {
    #[default]
    Prefix,

    PrefixLiteral,

    Exact,

    PrefixPriority,

    Regex,

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

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum NextUpstreamCondition {
    Error,

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
    #[serde(default)]
    pub conditions: Vec<NextUpstreamCondition>,

    #[serde(default)]
    pub tries: Option<u32>,

    #[serde(default)]
    pub timeout_ms: Option<u64>,
}

impl Default for ProxyNextUpstreamConfig {
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
pub struct CacheConfig {
    #[serde(default = "default_cache_valid_secs")]
    pub valid_secs: u64,

    #[serde(default)]
    pub bypass_headers: Vec<String>,

    #[serde(default)]
    pub bypass_cookies: Vec<String>,

    #[serde(default)]
    pub bypass_query_params: Vec<String>,
}

fn default_cache_valid_secs() -> u64 {
    60
}

impl Default for CacheConfig {
    fn default() -> Self {
        Self {
            valid_secs: default_cache_valid_secs(),
            bypass_headers: Vec::new(),
            bypass_cookies: Vec::new(),
            bypass_query_params: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RouteRule {
    pub id: String,

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

    /// nginx `autoindex on`: list a directory that has no index.html.
    #[serde(default)]
    pub autoindex: bool,
    #[serde(default)]
    pub redirect_url: Option<String>,
    #[serde(default = "default_redirect_code")]
    pub redirect_code: Option<u16>,
    /// Body of a `return` route (`$scheme`, `$host`, `$request_uri` are expanded).
    #[serde(default)]
    pub return_body: Option<String>,
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

    #[serde(default)]
    pub allowed_ips: Vec<String>,
    #[serde(default)]
    pub denied_ips: Vec<String>,
    #[serde(default)]
    pub basic_auth: Option<BasicAuthConfig>,
    #[serde(default)]
    pub satisfy: Satisfy,

    #[serde(default)]
    pub proxy_connect_timeout_ms: Option<u64>,
    #[serde(default)]
    pub proxy_read_timeout_ms: Option<u64>,
    #[serde(default)]
    pub proxy_send_timeout_ms: Option<u64>,

    #[serde(default)]
    pub proxy_next_upstream: Option<ProxyNextUpstreamConfig>,

    #[serde(default = "default_true")]
    pub access_log: bool,

    #[serde(default)]
    pub cache: Option<CacheConfig>,

    #[serde(default)]
    pub compression: Option<bool>,

    #[serde(default)]
    pub cors: Option<CorsConfig>,

    /// nginx `valid_referers`: when non-empty, other Referers get 403.
    #[serde(default)]
    pub valid_referers: Vec<String>,

    /// nginx `auth_request`: subrequest to an auth service before serving.
    #[serde(default)]
    pub auth_request: Option<AuthRequestConfig>,

    /// nginx `mirror`: base URLs (`http://host:port`) that get a copy of each request.
    #[serde(default)]
    pub mirror: Vec<String>,

    /// nginx `secure_link` + `secure_link_md5`.
    #[serde(default)]
    pub secure_link: Option<SecureLinkConfig>,

    /// nginx `sub_filter`: string replacements in text responses.
    #[serde(default)]
    pub sub_filter: Option<SubFilterConfig>,

    /// nginx `set $name value` (in order).
    #[serde(default)]
    pub set: Vec<(String, String)>,
    /// nginx `if` blocks.
    #[serde(default)]
    pub conditions: Vec<ConditionConfig>,
    /// nginx `rewrite`.
    #[serde(default)]
    pub rewrites: Vec<RewriteRule>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SubFilterConfig {
    /// `[from, to]` pairs, applied in order.
    #[serde(default)]
    pub replacements: Vec<[String; 2]>,
    /// nginx `sub_filter_once` (nginx default is `on`).
    #[serde(default = "default_true")]
    pub once: bool,
    /// Extra MIME types besides text/html (nginx `sub_filter_types`); `*` = all.
    #[serde(default)]
    pub types: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SecureLinkConfig {
    /// `secure_link_md5` template, e.g. `$secure_link_expires$uri$remote_addr secret`.
    pub md5: String,
    /// Query argument with the hash (default `md5`).
    #[serde(default)]
    pub hash_arg: Option<String>,
    /// Query argument with the expiry timestamp (default `expires`).
    #[serde(default)]
    pub expires_arg: Option<String>,
}

impl SecureLinkConfig {
    pub fn hash_arg(&self) -> &str {
        self.hash_arg.as_deref().unwrap_or("md5")
    }
    pub fn expires_arg(&self) -> &str {
        self.expires_arg.as_deref().unwrap_or("expires")
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AuthRequestConfig {
    /// Full URL of the auth endpoint, e.g. `http://auth:9000/check`.
    pub url: String,
    /// Auth-response headers copied to the proxied request (nginx `auth_request_set`).
    #[serde(default)]
    pub set_headers: Vec<String>,
    #[serde(default)]
    pub timeout_ms: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct CorsConfig {
    #[serde(default)]
    pub allow_origins: Vec<String>,
    #[serde(default)]
    pub allow_methods: Vec<String>,
    #[serde(default)]
    pub allow_headers: Vec<String>,
    #[serde(default)]
    pub expose_headers: Vec<String>,
    #[serde(default)]
    pub allow_credentials: bool,
    #[serde(default)]
    pub max_age_secs: Option<u64>,
}

fn default_true() -> bool {
    true
}

fn default_redirect_code() -> Option<u16> {
    Some(302)
}

static DEFAULT_NEXT_UPSTREAM: std::sync::LazyLock<ProxyNextUpstreamConfig> =
    std::sync::LazyLock::new(ProxyNextUpstreamConfig::default);

impl RouteRule {
    pub fn next_upstream(&self) -> &ProxyNextUpstreamConfig {
        self.proxy_next_upstream
            .as_ref()
            .unwrap_or(&DEFAULT_NEXT_UPSTREAM)
    }
}

impl Default for RouteRule {
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
            autoindex: false,
            redirect_url: None,
            redirect_code: None,
            return_body: None,
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
            access_log: true,
            cache: None,
            compression: None,
            cors: None,
            valid_referers: Vec::new(),
            auth_request: None,
            mirror: Vec::new(),
            secure_link: None,
            sub_filter: None,
            set: Vec::new(),
            conditions: Vec::new(),
            rewrites: Vec::new(),
        }
    }
}

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
                hash_key: None,
                sticky: None,
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
            listen_port: 80,
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
            routes: vec![RouteRule {
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
            }],
            error_pages: Default::default(),
            conditions: Vec::new(),
            rewrites: Vec::new(),
        };

        Self {
            admin: AdminConfig::default(),
            http: HttpConfig::default(),
            real_ip: RealIpConfig::default(),
            upstreams,
            vhosts: vec![default_vhost],
            acme: Default::default(),
            metrics: Default::default(),
            streams: Vec::new(),
        }
    }
}

pub enum ConfigLoad {
    Loaded(FerrixConfig),

    Created {
        config: FerrixConfig,
        admin_token: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigFormat {
    Yaml,
    Json,
}

impl ConfigFormat {
    pub fn of(path: &Path) -> Self {
        match path.extension().and_then(|e| e.to_str()) {
            Some(e) if e.eq_ignore_ascii_case("json") => ConfigFormat::Json,
            _ => ConfigFormat::Yaml,
        }
    }
}

pub const DEFAULT_CONFIG_TEMPLATE: &str = include_str!("../../../ferrix.example.yaml");

const YAML_HEADER: &str = "# Ferrix configuration. Every option is described in\n\
# https://arthurlunkes.github.io/ferrix (04. Configuration Guide); the commented\n\
# reference with all of them is ferrix.example.yaml (/usr/share/doc/ferrix/ in the\n\
# packages). When this file is saved from the dashboard, the previous version is\n\
# kept as ferrix.yaml.bak.\n\
# Apply changes without a restart: `ferrix reload` (or SIGHUP).\n\n";

impl FerrixConfig {
    pub fn parse(data: &str, format: ConfigFormat) -> Result<Self, String> {
        let mut cfg: Self = match format {
            ConfigFormat::Yaml => serde_saphyr::from_str(data).map_err(|e| e.to_string())?,
            ConfigFormat::Json => serde_json::from_str(data).map_err(|e| e.to_string())?,
        };
        cfg.resolve_ports();
        Ok(cfg)
    }

    pub fn resolve_ports(&mut self) {
        for vhost in &mut self.vhosts {
            if vhost.listen_port == 0 {
                vhost.listen_port = if vhost.tls.is_some() { 443 } else { 80 };
            }
        }
    }

    pub fn render(&self, format: ConfigFormat) -> Result<String, String> {
        match format {
            ConfigFormat::Yaml => serde_saphyr::to_string(self)
                .map(|y| format!("{}{}", YAML_HEADER, y))
                .map_err(|e| e.to_string()),
            ConfigFormat::Json => serde_json::to_string_pretty(self).map_err(|e| e.to_string()),
        }
    }

    pub fn load<P: AsRef<Path>>(path: P) -> Result<Self, String> {
        let p = path.as_ref();
        let data = std::fs::read_to_string(p)
            .map_err(|e| format!("Failed to read config file {}: {}", p.display(), e))?;
        Self::parse(&data, ConfigFormat::of(p))
            .map_err(|e| format!("Invalid config {}: {}", p.display(), e))
    }

    pub fn load_or_create<P: AsRef<Path>>(path: P) -> Result<ConfigLoad, String> {
        let p = path.as_ref();

        if p.exists() {
            return Self::load(p).map(ConfigLoad::Loaded);
        }

        let admin_token = uuid::Uuid::new_v4().simple().to_string();
        let default_cfg = if ConfigFormat::of(p) == ConfigFormat::Yaml {
            let text = DEFAULT_CONFIG_TEMPLATE.replace("@ADMIN_TOKEN@", &admin_token);
            if let Some(dir) = p.parent().filter(|d| !d.as_os_str().is_empty()) {
                let _ = std::fs::create_dir_all(dir);
            }
            std::fs::write(p, &text).map_err(|e| {
                format!(
                    "Failed to create config file {}: {}. Create it once with \
                     `sudo mkdir -p {dir} && sudo chown $USER {dir}`, or pass \
                     `--config <file>` to use another one.",
                    p.display(),
                    e,
                    dir = p
                        .parent()
                        .map(|d| d.display().to_string())
                        .unwrap_or_default()
                )
            })?;
            Self::parse(&text, ConfigFormat::Yaml)?
        } else {
            let mut cfg = Self::default();
            cfg.admin.auth = AdminAuthConfig {
                mode: AdminAuthMode::Token,
                token: Some(admin_token.clone()),
                username: None,
                password: None,
            };
            cfg.save_to_file(p)?;
            cfg
        };

        Ok(ConfigLoad::Created {
            config: default_cfg,
            admin_token,
        })
    }

    pub fn save_to_file<P: AsRef<Path>>(&self, path: P) -> Result<(), String> {
        let p = path.as_ref();
        let text = self
            .render(ConfigFormat::of(p))
            .map_err(|e| format!("Failed to serialize config: {}", e))?;
        if let Some(parent) = p.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        std::fs::write(p, text)
            .map_err(|e| format!("Failed to write config to {}: {}", p.display(), e))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn yaml_round_trip_keeps_every_field() {
        let mut cfg = FerrixConfig::default();
        cfg.http.worker_cpu_affinity = CpuAffinity::Cores(vec![0, 2]);
        cfg.http.compression.enabled = true;
        cfg.metrics.enabled = true;
        let yaml = cfg.render(ConfigFormat::Yaml).unwrap();
        assert!(yaml.starts_with("# Ferrix configuration"));
        let back = FerrixConfig::parse(&yaml, ConfigFormat::Yaml).unwrap();
        assert_eq!(
            serde_json::to_value(&back).unwrap(),
            serde_json::to_value(&cfg).unwrap()
        );
    }

    #[test]
    fn hand_written_yaml_uses_defaults_for_missing_fields() {
        let yaml = "http:\n  listen: 0.0.0.0:8080\n  worker_cpu_affinity: auto\nvhosts:\n  - id: site\n    name: Site\n    server_names: [example.com]\n    routes:\n      - id: root\n        path_prefix: /\n        target_type: static_files\n        static_root: /var/www\n";
        let cfg = FerrixConfig::parse(yaml, ConfigFormat::Yaml).unwrap();
        assert_eq!(cfg.http.listen, "0.0.0.0:8080");
        assert_eq!(cfg.http.worker_cpu_affinity, CpuAffinity::Auto);
        assert_eq!(
            cfg.vhosts[0].routes[0].static_root.as_deref(),
            Some("/var/www")
        );
        assert_eq!(cfg.admin.listen, "127.0.0.1:8081");
    }

    #[test]
    fn format_follows_the_extension() {
        assert_eq!(
            ConfigFormat::of(Path::new("ferrix.yaml")),
            ConfigFormat::Yaml
        );
        assert_eq!(
            ConfigFormat::of(Path::new("/etc/ferrix/ferrix.yml")),
            ConfigFormat::Yaml
        );
        assert_eq!(
            ConfigFormat::of(Path::new("old/ferrix.JSON")),
            ConfigFormat::Json
        );
    }

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

    #[test]
    fn default_admin_listen_does_not_collide_with_the_gateway_and_is_loopback() {
        let cfg = FerrixConfig::default();
        assert_eq!(cfg.admin.listen, "127.0.0.1:8081");
        assert!(crate::net::cidr::listen_addr_is_loopback(&cfg.admin.listen));
    }

    #[test]
    fn default_template_is_valid_as_is_and_fully_uncommented() {
        let as_is = FerrixConfig::parse(DEFAULT_CONFIG_TEMPLATE, ConfigFormat::Yaml).unwrap();
        assert!(validate::validate(&as_is).is_valid());
        assert_eq!(as_is.http.listen, "0.0.0.0:80");
        assert_eq!(as_is.vhosts[0].listen_port, 80);

        let uncommented: String = DEFAULT_CONFIG_TEMPLATE
            .lines()
            .map(|l| {
                let t = l.trim_start();
                if t.starts_with("# ") && !t.starts_with("##") {
                    let indent = l.len() - t.len();
                    format!("{}{}", &l[..indent], &t[2..])
                } else {
                    l.to_string()
                }
            })
            .filter(|l| !l.trim_start().starts_with("protocols: [http1, http2]"))
            .collect::<Vec<_>>()
            .join("\n");
        let all = FerrixConfig::parse(&uncommented, ConfigFormat::Yaml)
            .unwrap_or_else(|e| panic!("{}\n{}", e, uncommented));
        assert_eq!(all.vhosts[0].routes.len(), 3);
        assert_eq!(all.vhosts[0].listen_port, 80);
        assert!(all.http.compression.enabled);
        assert_eq!(all.streams.len(), 1);
    }
}
