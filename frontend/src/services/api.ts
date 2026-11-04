export interface ServerStatus {
  name: string;
  version: string;
  uptime_seconds: number;
  start_time_utc: string;
  admin_listen: string;
  proxy_listen: string;
  total_vhosts: number;
  total_upstreams: number;
  admin_auth_enabled: boolean;
}

export interface MetricsSnapshot {
  /** false when the core runs with metrics disabled (the default). */
  enabled?: boolean;
  uptime_seconds: number;
  total_requests: number;
  active_connections: number;
  requests_per_second: number;
  status_2xx: number;
  status_3xx: number;
  status_4xx: number;
  status_5xx: number;
  total_bytes_sent: number;
  avg_latency_ms: number;
  p95_latency_ms: number;
  p99_latency_ms: number;
}

export interface RequestLog {
  id: string;
  timestamp: string;
  client_ip: string;
  method: string;
  uri: string;
  host: string;
  status: number;
  latency_ms: number;
  bytes_sent: number;
  upstream?: string;
  user_agent?: string;
  is_websocket: boolean;
  error?: string;
}

export interface UpstreamTarget {
  url: string;
  weight: number;
  max_fails: number;
  fail_timeout_secs: number;
  backup: boolean;
  down: boolean;
  max_conns?: number | null;
}

export interface HealthCheckConfig {
  path: string;
  interval_secs: number;
  timeout_secs: number;
  expected_status: number;
}

export type LoadBalanceAlgorithm =
  | "round_robin"
  | "weighted_round_robin"
  | "least_conn"
  | "ip_hash"
  | "consistent_hash"
  | "least_time"
  | "random";

export interface UpstreamConfig {
  name: string;
  algorithm: LoadBalanceAlgorithm;
  targets: UpstreamTarget[];
  health_check?: HealthCheckConfig | null;
  verify_tls: boolean;
  hash_key?: string | null;
  sticky?: StickyConfig | null;
  tls?: UpstreamTlsConfig | null;
}

export interface StickyConfig {
  cookie: string;
  path?: string | null;
  max_age_secs?: number | null;
}

export interface TargetHealthStatus {
  upstream_name: string;
  target_url: string;
  healthy: boolean;
  latency_ms: number;
  last_checked?: string;
  consecutive_fails: number;
  status_code?: number;
}

export interface UpstreamWithHealth {
  name: string;
  config: UpstreamConfig;
  statuses: TargetHealthStatus[];
}

export type RateLimitMode = "no_delay" | "queue" | "delay";

export interface RateLimitConfig {
  requests_per_second: number;
  burst: number;
  mode: RateLimitMode;
  delay?: number | null;
  status: number;
  retry_after?: number | null;
}

export interface LimitConnConfig {
  max_connections: number;
  status: number;
}

export interface BasicAuthConfig {
  username: string;
  password: string;
}

export type Satisfy = "all" | "any";

export type RouteMatchType =
  | "prefix"
  | "prefix_literal"
  | "exact"
  | "prefix_priority"
  | "regex"
  | "regex_case_insensitive";

export type NextUpstreamCondition =
  | "error"
  | "timeout"
  | "http_500"
  | "http_502"
  | "http_503"
  | "http_504"
  | "http_403"
  | "http_404";

export interface ProxyNextUpstreamConfig {
  conditions: NextUpstreamCondition[];
  tries?: number | null;
  timeout_ms?: number | null;
}

export interface RouteRule {
  id: string;
  path_prefix: string;
  match_type: RouteMatchType;
  strip_prefix: boolean;
  target_type: "proxy_upstream" | "static_files" | "redirect" | "return" | "cgi" | "memcached";
  upstream_name?: string | null;
  static_root?: string | null;
  try_files?: string | null;
  autoindex?: boolean;
  redirect_url?: string | null;
  redirect_code?: number | null;
  return_body?: string | null;
  websocket_enabled: boolean;
  proxy_set_headers: Record<string, string>;
  add_headers: Record<string, string>;
  rate_limit?: RateLimitConfig | null;
  limit_conn?: LimitConnConfig | null;
  allowed_ips: string[];
  denied_ips: string[];
  basic_auth?: BasicAuthConfig | null;
  satisfy: Satisfy;
  proxy_connect_timeout_ms?: number | null;
  proxy_read_timeout_ms?: number | null;
  proxy_send_timeout_ms?: number | null;
  proxy_next_upstream?: ProxyNextUpstreamConfig | null;
  access_log?: boolean;
  valid_referers?: string[];
  auth_request?: AuthRequestConfig | null;
  mirror?: string[];
  secure_link?: SecureLinkConfig | null;
  sub_filter?: SubFilterConfig | null;
  set?: [string, string][];
  conditions?: ConditionConfig[];
  rewrites?: RewriteRule[];
  dav?: DavConfig | null;
  flv?: boolean;
  ssi?: boolean;
  ssi_types?: string[];
  image_filter?: ImageFilterConfig | null;
  cgi?: CgiConfig | null;
  grpc?: boolean;
  cache?: CacheConfig | null;
  proxy_redirect?: [string, string][];
  proxy_cookie_path?: [string, string][];
  proxy_cookie_domain?: [string, string][];
  proxy_cookie_flags?: [string, string][];
  proxy_intercept_errors?: boolean;
  limit_except?: { methods: string[]; allow: string[] } | null;
  limit_rate?: number;
  limit_rate_after?: number;
  empty_gif?: boolean;
  random_index?: boolean;
  userid?: UserIdConfig | null;
  add_before_body?: string | null;
  add_after_body?: string | null;
  addition_types?: string[];
  gunzip?: boolean;
  charset?: string | null;
  source_charset?: string | null;
  override_charset?: boolean;
  charset_types?: string[];
  memcached?: MemcachedConfig | null;
  tunnel?: TunnelConfig | null;
}

export interface CacheConfig {
  valid_secs: number;
  bypass_headers: string[];
  bypass_cookies: string[];
  bypass_query_params: string[];
  use_stale: string[];
  stale_keep_secs: number;
  lock: boolean;
  lock_timeout_ms?: number | null;
  revalidate: boolean;
  background_update: boolean;
}

export interface UserIdConfig {
  name?: string | null;
  domain?: string | null;
  path?: string | null;
  expires_secs?: number | null;
  flags?: string | null;
}

export interface MemcachedConfig {
  pass: string;
  key?: string | null;
  content_type?: string | null;
  gzip_flag?: number | null;
}

export interface TunnelConfig {
  pass?: string | null;
  allowed_ports: number[];
  allowed_hosts: string[];
  idle_timeout_ms?: number | null;
}

export type ClientVerify = "off" | "on" | "optional";

export interface TlsConfig {
  cert_path: string;
  key_path: string;
  acme: boolean;
  verify_client?: ClientVerify;
  client_ca?: string | null;
  ocsp_file?: string | null;
}

export interface UpstreamTlsConfig {
  client_cert?: string | null;
  client_key?: string | null;
  trusted_ca?: string | null;
  server_name?: string | null;
}

export interface AuthRequestConfig {
  url: string;
  set_headers: string[];
  timeout_ms?: number | null;
}

export interface SecureLinkConfig {
  md5: string;
  hash_arg?: string | null;
  expires_arg?: string | null;
}

export interface SubFilterConfig {
  replacements: [string, string][];
  once: boolean;
  types: string[];
}

export type RewriteFlag = "none" | "last" | "break" | "redirect" | "permanent";

export interface RewriteRule {
  pattern: string;
  replacement: string;
  flag: RewriteFlag;
}

export interface ConditionConfig {
  condition: string;
  set: [string, string][];
  return_code?: number | null;
  return_value?: string | null;
  rewrite?: RewriteRule | null;
}

export interface DavConfig {
  methods: string[];
  create_full_put_path: boolean;
  access?: number | null;
}

export type ImageOp = "test" | "size" | "resize" | "crop" | "rotate";

export interface ImageFilterConfig {
  op: ImageOp;
  width: number;
  height: number;
  rotate: number;
  jpeg_quality: number;
  buffer_bytes: number;
}

export type CgiProtocol = "fastcgi" | "scgi" | "uwsgi";

export interface CgiConfig {
  protocol: CgiProtocol;
  pass: string;
  root?: string | null;
  index?: string | null;
  params: Record<string, string>;
}

export interface VirtualHost {
  id: string;
  name: string;
  server_names: string[];
  listen_port: number;
  headers: Record<string, string>;
  rate_limit?: RateLimitConfig | null;
  limit_conn?: LimitConnConfig | null;
  allowed_ips: string[];
  denied_ips: string[];
  basic_auth?: BasicAuthConfig | null;
  satisfy: Satisfy;
  routes: RouteRule[];
  tls?: TlsConfig | null;
  error_pages?: Record<string, string>;
  conditions?: ConditionConfig[];
  rewrites?: RewriteRule[];
}

export type AdminAuthMode = "none" | "token" | "basic";

export interface AdminAuthConfig {
  mode: AdminAuthMode;
  token?: string | null;
  username?: string | null;
  password?: string | null;
}

export interface AdminConfig {
  listen: string;
  auth: AdminAuthConfig;
  cors_origins: string[];
}

export type RealIpHeader = "none" | "x_forwarded_for" | "x_real_ip";

export interface RealIpConfig {
  trusted_proxies: string[];
  header: RealIpHeader;
  recursive: boolean;
}

export interface HttpConfig {
  listen: string;
  worker_threads: number;
  client_header_timeout_ms: number;
  client_body_timeout_ms: number;
  keepalive_timeout_ms: number;
  proxy_connect_timeout_ms: number;
  proxy_read_timeout_ms: number;
  proxy_send_timeout_ms: number;
  client_max_body_size: number;
  proxy_request_buffer_size: number;
  access_log?: AccessLogConfig | null;
  variables?: Record<string, VariableDef>;
  proxy_protocol?: boolean;
  merge_slashes?: boolean;
  absolute_redirect?: boolean;
  port_in_redirect?: boolean;
  server_name_in_redirect?: boolean;
  geoip?: { country_db?: string | null; city_db?: string | null; asn_db?: string | null } | null;
  http2?: Record<string, number | null>;
  http3?: Record<string, number | null>;
}

export interface AccessLogConfig {
  path: string;
  format: string;
}

export type VariableDef =
  | { type: "map"; source: string; entries: { pattern: string; value: string }[]; default: string }
  | { type: "geo"; source?: string | null; ranges: { network: string; value: string }[]; default: string }
  | { type: "split_clients"; source: string; buckets: { percent: number; value: string }[] }
  | { type: "json"; source: string; path: string };

export type StreamProtocol = "tcp" | "udp";

export interface StreamConfig {
  id: string;
  listen: string;
  protocol: StreamProtocol;
  upstream: string;
  connect_timeout_ms: number;
  idle_timeout_ms: number;
  sni_routes?: Record<string, string>;
  accept_proxy_protocol?: boolean;
  send_proxy_protocol?: boolean;
  tls?: { cert_path: string; key_path: string } | null;
  upstream_tls?: ({ verify: boolean } & UpstreamTlsConfig) | null;
  allowed_ips?: string[];
  denied_ips?: string[];
  limit_conn?: number | null;
  upload_rate?: number;
  download_rate?: number;
  return_text?: string | null;
  pass?: string | null;
}

export interface FerrixConfig {
  admin: AdminConfig;
  http: HttpConfig;
  real_ip: RealIpConfig;
  upstreams: Record<string, UpstreamConfig>;
  vhosts: VirtualHost[];
  streams?: StreamConfig[];
}

export type IssueSeverity = "error" | "warning";

export interface ValidationIssue {
  severity: IssueSeverity;
  path: string;
  message: string;
}

export interface ValidationReport {
  valid: boolean;
  issues: ValidationIssue[];
  summary: string;
}

export type MigrationNoteLevel = "converted" | "partial" | "unsupported" | "ignored";

export interface MigrationNote {
  level: MigrationNoteLevel;
  file: string;
  line: number;
  directive: string;
  source: string;
  reason: string;
}

export interface MigrationReport {
  files: string[];
  notes: MigrationNote[];
  vhosts_created: number;
  routes_created: number;
  upstreams_created: number;
}

export interface ImportNginxResult {
  config: FerrixConfig;
  /** The converted config as YAML, ready to save as ferrix.yaml. */
  config_yaml?: string;
  report: MigrationReport;
  validation: ValidationReport;
}

export interface RouteTestResult {
  matched: boolean;
  vhost_id?: string | null;
  vhost_name?: string | null;
  vhost_reason?: string | null;
  route_id?: string | null;
  route_pattern?: string | null;
  route_reason?: string | null;
  target?: string | null;
  explanation: string;
}

export class ApiError extends Error {
  status: number;
  body: any;

  constructor(message: string, status: number, body: any) {
    super(message);
    this.name = "ApiError";
    this.status = status;
    this.body = body;
  }
}

const API_BASE = "/api";
export const TOKEN_STORAGE_KEY = "ferrix_admin_token";

export function getAdminToken(): string | null {
  return localStorage.getItem(TOKEN_STORAGE_KEY);
}

export function setAdminToken(token: string | null): void {
  if (token && token.trim()) {
    localStorage.setItem(TOKEN_STORAGE_KEY, token.trim());
  } else {
    localStorage.removeItem(TOKEN_STORAGE_KEY);
  }
}

async function authFetch(
  input: string,
  init?: RequestInit,
  customToken?: string,
): Promise<Response> {
  const headers = new Headers(init?.headers);
  const token = customToken ?? getAdminToken();
  if (token && !headers.has("Authorization")) {
    headers.set("Authorization", `Bearer ${token}`);
  }
  const res = await fetch(input, { ...init, headers });
  if (res.status === 401 && !customToken) {
    window.dispatchEvent(new CustomEvent("ferrix:unauthorized"));
  }
  return res;
}

async function throwApiError(res: Response, fallback: string): Promise<never> {
  let body: any = null;
  const text = await res.text();
  try {
    body = text ? JSON.parse(text) : null;
  } catch {
    body = text;
  }
  const message =
    (body && typeof body === "object" && (body.message || body.error)) ||
    (typeof body === "string" && body) ||
    fallback;
  throw new ApiError(message, res.status, body);
}

async function getJson<T>(path: string): Promise<T> {
  const res = await authFetch(`${API_BASE}${path}`);
  if (!res.ok) return throwApiError(res, `GET ${path} failed`);
  return res.json();
}

async function postText<T>(path: string, text: string): Promise<T> {
  const res = await authFetch(`${API_BASE}${path}`, {
    method: "POST",
    headers: { "Content-Type": "application/yaml" },
    body: text,
  });
  if (!res.ok) return throwApiError(res, `POST ${path} failed`);
  return res.json();
}

async function postJson<T>(path: string, payload: unknown): Promise<T> {
  const res = await authFetch(`${API_BASE}${path}`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(payload),
  });
  if (!res.ok) return throwApiError(res, `POST ${path} failed`);
  return res.json();
}

async function deleteJson<T>(path: string): Promise<T> {
  const res = await authFetch(`${API_BASE}${path}`, { method: "DELETE" });
  if (!res.ok) return throwApiError(res, `DELETE ${path} failed`);
  return res.json();
}

export const api = {
  getAdminToken,
  setAdminToken,

  async testToken(token: string): Promise<boolean> {
    try {
      const res = await authFetch(`${API_BASE}/status`, {}, token);
      return res.ok;
    } catch {
      return false;
    }
  },

  getStatus: () => getJson<ServerStatus>("/status"),
  getMetrics: () => getJson<MetricsSnapshot>("/metrics"),

  async getLogs(params?: {
    limit?: number;
    status?: number;
    q?: string;
  }): Promise<RequestLog[]> {
    const query = new URLSearchParams();
    if (params?.limit) query.set("limit", params.limit.toString());
    if (params?.status) query.set("status", params.status.toString());
    if (params?.q) query.set("q", params.q);
    return getJson<RequestLog[]>(`/logs?${query.toString()}`);
  },

  getConfig: () => getJson<FerrixConfig>("/config"),

  updateConfig: (config: FerrixConfig) =>
    postJson<{ status: string; report: ValidationReport }>("/config", config),

  validateConfig: (config: FerrixConfig) =>
    postJson<ValidationReport>("/config/validate", config),

  /** The active configuration as YAML, as written to ferrix.yaml. */
  async getConfigYaml(): Promise<string> {
    const res = await authFetch(`${API_BASE}/config/yaml`);
    if (!res.ok) return throwApiError(res, "GET /config/yaml failed");
    return res.text();
  },

  async updateConfigYaml(yaml: string): Promise<{ status: string; report: ValidationReport }> {
    return postText("/config/yaml", yaml);
  },

  async validateConfigYaml(yaml: string): Promise<ValidationReport> {
    return postText("/config/yaml/validate", yaml);
  },

  routeTest: (params: { host: string; path: string; port?: number }) =>
    postJson<RouteTestResult>("/route-test", params),

  importNginx: (source: string, fileName?: string) =>
    postJson<ImportNginxResult>("/import/nginx", { source, file_name: fileName }),

  getVHosts: () => getJson<VirtualHost[]>("/vhosts"),

  saveVHost: (vhost: VirtualHost) =>
    postJson<{ status: string; report: ValidationReport }>("/vhosts", vhost),

  deleteVHost: (id: string) =>
    deleteJson<{ status: string }>(`/vhosts/${encodeURIComponent(id)}`),

  getUpstreams: () => getJson<UpstreamWithHealth[]>("/upstreams"),

  saveUpstream: (upstream: UpstreamConfig) =>
    postJson<{ status: string; report: ValidationReport }>("/upstreams", upstream),

  deleteUpstream: (name: string) =>
    deleteJson<{ status: string }>(`/upstreams/${encodeURIComponent(name)}`),

  subscribeLiveLogs(
    onLog: (log: RequestLog) => void,
    onError?: (err: any) => void,
  ): () => void {
    const token = getAdminToken();
    const url = token
      ? `${API_BASE}/logs/stream?token=${encodeURIComponent(token)}`
      : `${API_BASE}/logs/stream`;
    const es = new EventSource(url);
    es.addEventListener("request", (e) => {
      try {
        const log = JSON.parse((e as MessageEvent).data);
        onLog(log);
      } catch (err) {
        console.error("Error parsing SSE log:", err);
      }
    });

    es.onerror = (err) => {
      if (onError) onError(err);
    };

    return () => {
      es.close();
    };
  },
};

export function newRoute(overrides: Partial<RouteRule> = {}): RouteRule {
  return {
    id: `route-${Date.now()}-${Math.floor(Math.random() * 1000)}`,
    path_prefix: "/",
    match_type: "prefix",
    strip_prefix: false,
    target_type: "proxy_upstream",
    upstream_name: null,
    static_root: null,
    try_files: null,
    redirect_url: null,
    redirect_code: 302,
    websocket_enabled: true,
    proxy_set_headers: {},
    add_headers: {},
    rate_limit: null,
    limit_conn: null,
    allowed_ips: [],
    denied_ips: [],
    basic_auth: null,
    satisfy: "all",
    proxy_connect_timeout_ms: null,
    proxy_read_timeout_ms: null,
    proxy_send_timeout_ms: null,
    proxy_next_upstream: null,
    ...overrides,
  };
}

export function newVHost(overrides: Partial<VirtualHost> = {}): VirtualHost {
  return {
    id: `vhost-${Date.now()}`,
    name: "",
    server_names: ["localhost"],
    listen_port: 8000,
    headers: {},
    rate_limit: null,
    limit_conn: null,
    allowed_ips: [],
    denied_ips: [],
    basic_auth: null,
    satisfy: "all",
    routes: [],
    ...overrides,
  };
}

export function newUpstreamTarget(overrides: Partial<UpstreamTarget> = {}): UpstreamTarget {
  return {
    url: "",
    weight: 1,
    max_fails: 3,
    fail_timeout_secs: 10,
    backup: false,
    down: false,
    max_conns: null,
    ...overrides,
  };
}

export function newUpstream(overrides: Partial<UpstreamConfig> = {}): UpstreamConfig {
  return {
    name: "",
    algorithm: "round_robin",
    targets: [newUpstreamTarget()],
    health_check: null,
    verify_tls: true,
    ...overrides,
  };
}

