# 04. Configuration & Features Guide

Ferrix's configuration file is written in **YAML** (`ferrix.yaml` by default), easy to edit by hand, review in Git or generate from infrastructure-as-code tools. The REST API and the dashboard exchange the same structure as JSON. A `ferrix.json` from an older version is still read; `ferrix convert-config ferrix.json ferrix.yaml` converts it.

---

## 📄 General Structure of `ferrix.yaml`

```yaml
admin:
  listen: 127.0.0.1:8081
http:
  listen: 0.0.0.0:8000
  worker_threads: 4
upstreams:
  # ... see "Upstreams and Load Balancing"
vhosts:
  # ... see "Virtual Hosts"
```

---

## 🌐 Virtual Hosts (Sites)

Each Virtual Host represents a server block (equivalent to an Nginx `server { ... }` block).

```yaml
id: main-vhost
name: My Web Application
server_names: [mydomain.com, www.mydomain.com, localhost]
listen_port: 8000
headers:
  X-Powered-By: Ferrix/0.1.0
  X-Frame-Options: SAMEORIGIN
  X-Content-Type-Options: nosniff
allowed_ips: []
denied_ips: [192.168.1.100]
basic_auth:
  username: admin
  password: secure_password_here
rate_limit:
  requests_per_second: 50
  burst: 100
routes:
  # ... see "Routing Rules"
```

### Virtual Host Properties:

- `id`: Unique identifier (used in REST API URLs).
- `name`: Friendly human-readable name displayed on the dashboard.
- `server_names`: List of hostnames or IPs accepted in the `Host` header. Supports `*` as a catch-all for all domains.
- `listen_port`: Inbound traffic listening port.
- `headers`: HTTP headers injected into all responses served by this virtual host.
- `allowed_ips`: List of authorized IPs. If empty, all IPs are allowed (except explicitly denied ones).
- `denied_ips`: Blocked IP addresses (returns `403 Forbidden`).
- `basic_auth` (optional): Protects all routes with HTTP Basic Authentication.
- `rate_limit` (optional): Global host rate limit applied per client IP.

---

## 🛣️ Routing Rules (Routes)

Inside each Virtual Host, you configure path routing rules (`routes`), equivalent to Nginx `location` blocks:

### 1. Reverse Proxy Route

Forwards traffic to a backend server pool:

```yaml
id: api-route
path_prefix: /api
strip_prefix: false
target_type: proxy_upstream
upstream_name: my_backend_cluster
websocket_enabled: true
proxy_set_headers:
  Host: $host
  X-Real-IP: $remote_addr
  X-Forwarded-For: $proxy_add_x_forwarded_for
  X-Forwarded-Proto: $scheme
add_headers:
  Access-Control-Allow-Origin: '*'
```

- `strip_prefix`: If `true`, strips `/api` from the URI path before forwarding to the backend.
- `websocket_enabled`: Enables long-lived bidirectional WebSocket tunneling.
- `proxy_set_headers`: Custom upstream headers with special variable substitution (`$host`, `$remote_addr`, `$proxy_add_x_forwarded_for`, `$scheme`).

### 2. Static Files Route with SPA Fallback

Serves local disk files and routes missing paths to `index.html`:

```yaml
id: frontend-route
path_prefix: /
strip_prefix: false
target_type: static_files
static_root: ./frontend/dist
try_files: $uri $uri/ /index.html
websocket_enabled: false
```

- `static_root`: Local disk path where files are hosted.
- `try_files`: Checks for the exact file (`$uri`), then directory index (`$uri/`), falling back to (`/index.html`). Tailor-made for React Router, Vue Router, etc.

### 3. HTTP Redirect Route

Redirects the browser to an alternate URL:

```yaml
id: docs-route
path_prefix: /docs
strip_prefix: false
target_type: redirect
redirect_url: https://github.com/my-project
redirect_code: 301
```

---

## ⚖️ Upstreams and Load Balancing

Upstreams define pools of backend servers that share incoming workload.

```yaml
upstreams:
  my_backend_cluster:
    name: my_backend_cluster
    algorithm: round_robin
    targets:
      - url: http://10.0.0.1:3000
        weight: 3
        max_fails: 3
        fail_timeout_secs: 10
      - url: http://10.0.0.2:3000
        weight: 1
        max_fails: 3
        fail_timeout_secs: 10
    health_check:
      path: /health
      interval_secs: 5
      timeout_secs: 2
      expected_status: 200
```

### Supported Balancing Algorithms (`algorithm`):

1. **`round_robin`**: Cycles sequentially through healthy targets.
2. **`weighted_round_robin`**: Distributes proportionally to each server's configured `weight` (e.g., weight 3 receives 3x more requests than weight 1).
3. **`least_conn`**: Routes to the target with the fewest active concurrent connections at request time.
4. **`ip_hash`**: Hashes client IP to ensure continuous affinity with the same backend (sticky session).
5. **`random`**: Uniform random distribution among healthy nodes.
6. **`consistent_hash`**: Ketama-style consistent hashing on `hash_key` (nginx `hash ... consistent`). Adding or removing a server only remaps the keys that belonged to it. `hash_key` accepts `$remote_addr` (default), `$request_uri`, `$uri`, `$host`, `$http_<header>`, `$cookie_<name>` and `$arg_<name>`.

```yaml
upstreams:
  api:
    name: api
    algorithm: consistent_hash
    hash_key: $request_uri
    targets:
      - url: http://10.0.0.1:8080
      - url: http://10.0.0.2:8080
```

### Automatic Failover (_proxy_next_upstream_):

If a pool node fails (connection error, timeout, or HTTP 502/504 status code):

1. Ferrix increments the node's consecutive failure counter.
2. Immediately tries the next available target in the pool without dropping the client request.
3. Once consecutive failures reach `max_fails`, the node is temporarily marked **UNHEALTHY** until active background health checks confirm recovery.


---

## 📊 Metrics (opt-in)

Metrics are **off by default**, like nginx's `stub_status`. While disabled, the proxy does no per-request bookkeeping at all: no counters, no in-memory request log, no byte counting. Turn them on when you want the dashboard charts, Live Requests or Prometheus:

```yaml
metrics:
  enabled: true
```

When enabled:

- `GET /metrics` on the admin port serves Prometheus text format (`ferrix_requests_total`, `ferrix_request_duration_seconds` histogram, `ferrix_response_bytes_total`, `ferrix_active_requests`, `ferrix_upstream_up`). It uses the admin authentication unless `admin.metrics_public` is `true`.
- `/api/metrics`, `/api/logs` and `/api/logs/stream` feed the dashboard.

Turning metrics on costs roughly 4–9% of throughput in the benchmarks. The control plane itself can be switched off with `admin: { enabled: false }`; Ferrix is then configured only through `ferrix.yaml` and `ferrix reload`. The web dashboard is optional: it's served by the admin server from `admin.dashboard_dir` when that is set, with no extra process.

## 🗜️ Compression

```yaml
http:
  compression:
    enabled: true
    level: default
    min_length: 1024
    algorithms:
      - zstd
      - br
      - gzip
    types:
      - text/*
      - application/json
      - application/javascript
      - image/svg+xml
```

The encoding is negotiated from `Accept-Encoding` in the order of `algorithms`. `level` is `fastest`, `default` or `best`. A route can override the global switch with `compression: true|false`. Static files are compressed once and kept in memory (like `gzip_static`), so repeated requests cost no CPU. Proxied responses are compressed on the fly.

## 📁 Static files: caching and ranges

Small files are kept in memory so they're served without touching the disk; larger ones
are sent with `sendfile` straight from the kernel's page cache. Both limits are
configurable:

```yaml
http:
  static_cache:
    max_file_bytes: 1048576      # files up to 1 MB are cached (0 disables the cache)
    max_total_bytes: 67108864    # the cache uses at most 64 MB
```

Static responses carry `ETag` and `Last-Modified`. `If-None-Match` and `If-Modified-Since` are answered with `304 Not Modified`, and single `Range` requests are answered with `206 Partial Content` (or `416`).

## 🌍 CORS

```yaml
cors:
  allow_origins:
    - https://app.example.com
  allow_methods:
    - GET
    - POST
  allow_headers:
    - Authorization
    - Content-Type
  expose_headers:
    - X-Request-Id
  allow_credentials: true
  max_age_secs: 600
```

Set this on a route. Preflight requests (`OPTIONS` with `Access-Control-Request-Method`) are answered by Ferrix without reaching the upstream. `"*"` in `allow_origins` allows any origin. With credentials enabled, the origin is echoed back instead of `*`.

## 🧾 Custom error pages and request IDs

```yaml
error_pages:
  '404': /var/www/errors/404.html
  5xx: /var/www/errors/50x.html
```

`error_pages` belongs to a virtual host and replaces the body of the errors Ferrix generates itself, while keeping the status code. Error responses that come from an upstream pass through untouched, like nginx without `proxy_intercept_errors`.

`http: { request_id: true }` adds an `X-Request-Id` to the upstream request and to the response. An ID already sent by the client is kept.

## 🌐 HTTP versions: HTTP/1.1, HTTP/2 and HTTP/3

```yaml
http:
  protocols: [http1, http2, http3]   # default: [http1, http2]
```

`protocols` lists the HTTP versions Ferrix serves to clients; leave one out and it is refused:

| Value | Where it runs |
| --- | --- |
| `http1` | HTTP/1.0 and 1.1, on plain and TLS ports |
| `http2` | Over TLS (negotiated with ALPN) and, on plain ports, for clients that start with it (h2c) |
| `http3` | Over QUIC (UDP) on the **same port number as every TLS port**, with the same certificates. TCP responses carry `Alt-Svc: h3=":443"` so browsers switch to it |

Notes:
- `http3` needs at least one TLS virtual host, and either `http1` or `http2` must stay on: clients discover HTTP/3 through a first TCP connection.
- Open the UDP port in your firewall (and publish it in Docker: `-p 443:443/udp`).
- Requests are routed, filtered and proxied the same way whatever the version; upstreams are always reached over HTTP/1.1, like nginx.
- Changing `protocols` needs a restart (`ferrix restart`), since it changes the listeners.
- HTTP/3 is built in by default (feature `http3`) but only runs when listed here. It adds about 1 MB to the binary, and ~5 MB of memory once in use.

## 🔐 HTTPS: several certificates per port (SNI) and Let's Encrypt

Any number of virtual hosts can share a TLS port. The certificate is chosen per connection from the SNI name: exact `server_names` first, then wildcards (`*.example.com`), then the first TLS host on the port. Certificates are re-read when the configuration changes, so a renewal only needs `ferrix reload`.

For automatic certificates, use `acme` instead of file paths:

```yaml
acme:
  email: admin@example.com
  storage_dir: ./acme
vhosts:
  - id: site
    server_names: [example.com, www.example.com]
    listen_port: 443
    tls:
      acme: true
    routes:
      # ...
```

Ferrix answers the HTTP-01 challenge on its plain-HTTP ports, so port 80 must reach it. Certificates are stored in `storage_dir` and renewed `renew_before_days` (default 30) before they expire. To test against Let's Encrypt staging, set `"directory_url": "https://acme-staging-v02.api.letsencrypt.org/directory"`. Wildcard names can't be validated over HTTP-01 and are skipped.

## 🔌 TCP/UDP streams

Layer-4 proxying, like nginx's `stream {}` block. It works for databases, Redis, MQTT, DNS and anything else that runs over TCP or UDP:

```yaml
upstreams:
  pg:
    name: pg
    algorithm: least_conn
    targets:
      - url: tcp://10.0.0.5:5432
      - url: tcp://10.0.0.6:5432
streams:
  - id: postgres
    listen: 0.0.0.0:5432
    protocol: tcp
    upstream: pg
    connect_timeout_ms: 5000
    idle_timeout_ms: 600000
```

Streams use the pool's balancing algorithm and passive health marking. `ferrix import` converts nginx `stream {}` blocks automatically. Streams start with the process; adding a new one needs a restart.

## ⚙️ Workers, CPU, open files and the default page (nginx-style settings)

These live in the `http` section and mirror nginx's main-context directives:

```yaml
http:
  worker_threads: 4
  worker_cpu_affinity: auto
  worker_connections: 4096
  worker_rlimit_nofile: 65535
  server_tokens: true
  default_page: true
```

| Ferrix (`http.*`) | nginx | What it does |
| --- | --- | --- |
| `worker_threads` | `worker_processes` | Worker threads serving the proxy ports. Default: one per CPU core available to the process. |
| `worker_cpu_affinity` | `worker_cpu_affinity` | `"off"` (default), `"auto"` (worker N on the N-th allowed core) or a list of core ids such as `[0, 2, 4, 6]`. Linux only. |
| `worker_connections` | `worker_connections` | Maximum simultaneous client connections per worker (default 4096). A worker at the limit stops accepting until a connection closes; new connections wait in the kernel's listen queue. |
| `worker_rlimit_nofile` | `worker_rlimit_nofile` | Raises the open-file limit (`RLIMIT_NOFILE`) at startup. Every connection and served file uses one descriptor, so set it above `worker_threads × worker_connections`. Going above the system's hard limit needs root. |
| `server_tokens` | `server_tokens` | `true` (default) sends `Server: ferrix/<version>` and shows the version on the default page; `false` sends just `Server: ferrix`. |
| `default_page` | the default site | `true` (default) answers requests that match no virtual host (or a host with no routes yet) with a "Welcome to Ferrix" page; `false` answers a plain `404`. |

`ferrix import` converts the matching nginx directives (`worker_processes`, `worker_cpu_affinity`, `events { worker_connections }`, `worker_rlimit_nofile`, `server_tokens`) automatically.
