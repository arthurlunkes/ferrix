# 04. Configuration & Features Guide

Ferrix's configuration file is written in **YAML** (`ferrix.yaml` by default), easy to edit by hand, review in Git or generate from infrastructure-as-code tools. The REST API and the dashboard exchange the same structure as JSON. A `ferrix.json` from an older version is still read; `ferrix convert-config ferrix.json ferrix.yaml` converts it.

---

## 📄 General Structure of `ferrix.yaml`

```yaml
admin:
  listen: 127.0.0.1:8081
http:
  listen: 0.0.0.0:80
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
listen_port: 443        # default: 443 with tls, 80 without
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
- `autoindex`: `true` lists the directory (HTML, like nginx `autoindex on`) when it has no `index.html`. Default `false`.
- `target_type: return` + `redirect_code` + `return_body`: fixed response (nginx `return 200 "ok"`). `$scheme`, `$host` and `$request_uri` are expanded.
- `valid_referers`: list like nginx (`none`, `blocked`, `server_names`, `*.ex.com`, `ex.*`, `ex.com/path`, `~regex`). Other Referers get 403 (the nginx `if ($invalid_referer) { return 403; }` pattern).
- `auth_request: { url, set_headers, timeout_ms }`: asks `url` (GET, original headers + `X-Original-URI`/`X-Original-Method`) before serving. 2xx passes and copies `set_headers` to the backend request; 401/403 go to the client; anything else is 500.
- `mirror: [http://shadow:8080]`: sends a copy of each request (method, URI, headers; no body, like `mirror_request_body off`) and ignores the response.
- `secure_link: { md5, hash_arg, expires_arg }`: nginx `secure_link`/`secure_link_md5` (base64url MD5). Wrong hash = 403, expired = 410.
- `sub_filter: { replacements: [[from, to]], once, types }`: nginx `sub_filter` on text/html (plus `types`); uncompressed bodies up to 8 MB.
- `proxy_redirect: [[from, to]]` (`default`, prefix or `~regex` with `$1`), `proxy_cookie_path` / `proxy_cookie_domain: [[from, to]]`, `proxy_cookie_flags: [[cookie|~regex|*, "secure httponly samesite=strict nosecure ..."]]`: nginx rewrites of `Location`/`Refresh` and `Set-Cookie` from the backend.
- `proxy_intercept_errors: true`: backend 4xx/5xx are answered with the vhost `error_pages`.
- `limit_except: { methods: [GET, POST], allow: [10.0.0.0/8] }`: other methods get 403 unless the client IP is in `allow` (GET also allows HEAD).
- `limit_rate: 102400`, `limit_rate_after: 1048576`: nginx `limit_rate` / `limit_rate_after`, bytes per second for each response body after the first N bytes (turns off zero-copy sendfile for that route).
- `flv: true`: nginx `flv` pseudo-streaming; `video.flv?start=<byte>` streams from that byte with an FLV header in front.
- `empty_gif: true`: nginx `empty_gif`, answers a 1×1 transparent GIF (tracking pixels).
- `random_index: true`: nginx `random_index`, a request for a directory serves a random visible file from it.
- `userid: { name: uid, domain, path: /, expires_secs: 31536000, flags: "Secure HttpOnly" }`: nginx `userid on`, a visitor-id cookie set on the first response (and added to that request so the backend sees it).
- `add_before_body: /header.html`, `add_after_body: /footer.html`, `addition_types`: nginx addition filter; the URI is fetched as a subrequest from the same vhost (or a full URL) and wrapped around text/html bodies.
- `gunzip: true`: nginx `gunzip on`, gzip responses are decompressed (streaming) for clients that do not accept gzip.
- `charset: utf-8`, `source_charset: windows-1251`, `override_charset`, `charset_types`: nginx charset module; text responses get `; charset=` and, when the source differs, the body is converted (WHATWG encodings).
- `ssi: true` (+ `ssi_types`): nginx SSI subset on text/html — `include file|virtual` (read from `static_root`, nested up to 10), `echo var default`, `set`, `if/elif/else/endif expr`. Bodies up to 8 MB.

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
- HTTP/3 is **not in the default build**, so the packages and Docker images don't include it: build Ferrix with `cargo build --release --features http3`. Even when unused it would add about 1 MB to the binary and 0.7 MB of memory, and ~5 MB once in use. Without it, `http3` in `protocols` is ignored with a warning.

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

### Access log in a file (`http.access_log`)

Equivalent to nginx `log_format` + `access_log`. Works with metrics on or off; routes with `access_log: false` are skipped.

```yaml
http:
  access_log:
    path: /var/log/ferrix/access.log
    format: combined   # or e.g. '$remote_addr "$request" $status $request_time $upstream_addr'
```

Variables: `$remote_addr $remote_user $time_local $time_iso8601 $msec $request $request_method $request_uri $uri $args $status $body_bytes_sent $request_time $host $http_host $http_referer $http_user_agent $upstream_addr $scheme $request_id`.

### Sticky sessions (`upstreams.<name>.sticky`)

nginx `sticky cookie srv_id expires=1h path=/`. The cookie stores a hash of the target, never its address.

```yaml
upstreams:
  app:
    sticky: { cookie: srv_id, path: /, max_age_secs: 3600 }
```

### SNI routing for TCP streams (`streams[].sni_routes`)

nginx `ssl_preread on` + `map $ssl_preread_server_name`. Ferrix peeks at the TLS ClientHello (without terminating TLS) and picks the upstream by server name: exact, `*.suffix`, then `default`, then `upstream`.

```yaml
streams:
  - id: tls-443
    listen: 0.0.0.0:443
    upstream: web
    sni_routes: { api.example.com: api, "*.example.com": web, default: web }
```

### Variables, `if`, `set` and `rewrite`

nginx `map`, `geo` and `split_clients` become `http.variables`; `set`, `if` and `rewrite` exist per route (and `conditions`/`rewrites` per vhost, run before route matching). Variables work in `rewrite` targets, `if` returns, `proxy_set_headers` values and `upstream_name: $var`.

```yaml
http:
  variables:
    is_bot:  { type: map, source: $http_user_agent, entries: [{ pattern: "~*bot", value: "1" }], default: "0" }
    network: { type: geo, ranges: [{ network: 10.0.0.0/8, value: lan }], default: wan }
    canary:  { type: split_clients, source: $remote_addr, buckets: [{ percent: 10, value: app_v2 }, { percent: 0, value: app_v1 }] }
vhosts:
  - id: site
    rewrites: [{ pattern: "^/old/(.*)$", replacement: "/new/$1", flag: permanent }]
    routes:
      - id: app
        path_prefix: /
        target_type: proxy_upstream
        upstream_name: $canary
        conditions: [{ condition: "$is_bot", return_code: 403 }]
        set: [["$tier", "$network"]]
        proxy_set_headers: { X-Tier: $tier }
```

Built-ins: `$uri $request_uri $args $arg_* $http_* $cookie_* $host $remote_addr $scheme $https $request_method $content_type $content_length $1..$9`. `if` operators: `= != ~ ~* !~ !~* -f !-f -d !-d -e !-e`. Rewrite flags: `last` (re-match routes, up to 10 times), `break`, `redirect`, `permanent`. `split_clients` hashes with Ferrix's own hash, so buckets keep their sizes but not nginx's exact client assignment.

### gRPC (`grpc: true`)

nginx `grpc_pass grpc://backend;`. The route proxies over HTTP/2 to the upstream (targets `http://`/`grpc://` = h2c, `https://`/`grpcs://` = TLS + ALPN h2), keeping one multiplexed connection per target. Trailers (`grpc-status`) pass through. If the backend is unreachable the client gets `grpc-status: 14` (UNAVAILABLE). Needs the `http2` feature (on by default), and clients must reach Ferrix over HTTP/2.

```yaml
routes:
  - id: api-grpc
    path_prefix: /pkg.Service/
    target_type: proxy_upstream
    upstream_name: grpc_backend
    grpc: true
```

### FastCGI, SCGI and uwsgi (`target_type: cgi`)

nginx `fastcgi_pass` / `scgi_pass` / `uwsgi_pass` (PHP-FPM, Python apps...). `pass` is `host:port`, `unix:/path.sock` or an upstream name (its live targets are tried in order). Ferrix sends the standard `fastcgi_params` (`SCRIPT_FILENAME` = root + script, `PATH_INFO` split at `.php/`, `HTTP_*` headers...); `params` add or override entries and accept `$document_root`, `$fastcgi_script_name` and `$fastcgi_path_info`. The request body is buffered (up to `client_max_body_size`) and so is the reply (up to 64 MB).

```yaml
routes:
  - id: php
    path_prefix: '\.php(/|$)'
    match_type: regex
    target_type: cgi
    static_root: /var/www/html
    cgi: { protocol: fastcgi, pass: "unix:/run/php/php8.3-fpm.sock", index: index.php }
```

### WebDAV (`dav`)

nginx `dav_methods PUT DELETE MKCOL COPY MOVE;` on a static route. Paths stay inside `static_root`; PUT writes atomically (temp file + rename). Protect the route (basic auth / IP list) — it lets clients change files.

```yaml
routes:
  - id: uploads
    path_prefix: /dav/
    strip_prefix: true
    target_type: static_files
    static_root: /srv/dav
    dav: { methods: [PUT, DELETE, MKCOL, COPY, MOVE], create_full_put_path: true, access: 0o644 }
    basic_auth: { username: admin, password: "$2b$..." }
```

### Image filter (`image_filter`, build feature `image-filter`)

nginx `image_filter resize|crop|rotate|size|test`, for JPEG, PNG, GIF and WebP, on static or proxied responses. Not in the default build (it adds image codecs): `cargo build --release --features image-filter`. Work runs on a blocking thread.

```yaml
routes:
  - id: thumbs
    path_prefix: /thumbs/
    target_type: static_files
    static_root: /srv/images
    strip_prefix: true
    image_filter: { op: resize, width: 300, height: 0, jpeg_quality: 80, buffer_bytes: 5242880 }
```

### `least_time` balancing

`algorithm: least_time` (nginx `least_time header`): each target keeps a moving average of its response time; the pool picks the lowest `average × (active connections + 1)`. Targets not measured yet are tried first.

### URL and redirect normalization (`http`)

- `merge_slashes` (default `true`, like nginx): `//a///b` is routed and forwarded as `/a/b`.
- `absolute_redirect` (default `true`): redirects Ferrix builds itself with a relative target (`/new`, directory slash redirects, `rewrite ... redirect`, `return 301 /x`) get `scheme://host`. Backend redirects are untouched (see `proxy_redirect`).
- `port_in_redirect` (default `true`): keep the Host port in those redirects.
- `server_name_in_redirect` (default `false`): use the vhost's first concrete server name instead of the Host header.

### Cache: stale, lock, revalidate, background update

Extra `cache:` fields on a proxy route (nginx `proxy_cache_*`):

```yaml
cache:
  valid_secs: 60
  use_stale: [error, timeout, updating, http_500, http_502, http_503, http_504]
  stale_keep_secs: 86400     # how long expired entries are kept for the options below
  lock: true                 # proxy_cache_lock: one request fills, the rest wait (lock_timeout_ms, default 5000)
  revalidate: true           # proxy_cache_revalidate: If-None-Match / If-Modified-Since; 304 refreshes the entry
  background_update: true    # serve stale (needs "updating") and refresh in the background
```

`x-cache-status` shows `HIT`, `MISS`, `STALE`, `UPDATING` or `REVALIDATED`.

### PROXY protocol

- `http.proxy_protocol: true` (nginx `listen ... proxy_protocol`): every HTTP/HTTPS connection must start with a PROXY protocol v1 or v2 header (AWS NLB, HAProxy, ...); its source address becomes the client IP. Connections without it are closed.
- `streams[].accept_proxy_protocol: true`: the same for a TCP stream.
- `streams[].send_proxy_protocol: true` (nginx `proxy_protocol on`): send a v1 header with the client address to the upstream.

### TLS to the backend (`upstreams.<name>.tls`)

nginx `proxy_ssl_certificate`, `proxy_ssl_certificate_key`, `proxy_ssl_trusted_certificate`, `proxy_ssl_name` (`proxy_ssl_verify` is `verify_tls`):

```yaml
upstreams:
  internal_api:
    targets: [{ url: "https://10.0.0.5:8443" }]
    verify_tls: true
    tls:
      client_cert: /etc/ferrix/client.pem   # mTLS: certificate shown to the backend
      client_key: /etc/ferrix/client.key
      trusted_ca: /etc/ferrix/internal-ca.pem   # instead of the public web roots
      server_name: api.internal            # SNI and certificate name instead of the IP
```

### Client certificates (`tls.verify_client`)

nginx `ssl_verify_client on|optional` + `ssl_client_certificate`:

```yaml
vhosts:
  - id: api
    listen_port: 443
    tls: { cert_path: /etc/ferrix/api.pem, key_path: /etc/ferrix/api.key, verify_client: on, client_ca: /etc/ferrix/clients-ca.pem }
```

`on` answers 400 without a valid certificate; `optional` lets the request through and `$ssl_client_verify` says `SUCCESS` or `NONE`. Variables for `proxy_set_headers`: `$ssl_client_verify`, `$ssl_client_s_dn`, `$ssl_client_i_dn`, `$ssl_client_escaped_cert`. Vhosts sharing a port share one trust list (the union of their CAs). Changing `verify_client` or `client_ca` takes effect after a restart.

OCSP stapling: `tls.ocsp_file: /etc/ferrix/site.ocsp` (nginx `ssl_stapling_file`) staples that DER OCSP response; refresh the file (e.g. `openssl ocsp ... -respout`) and run `ferrix reload`. Ferrix does not query OCSP responders itself (Let's Encrypt no longer runs one).

### memcached (`target_type: memcached`)

nginx `memcached_pass` + `set $memcached_key`: the body comes from memcached `get <key>`; a miss answers 404 (pair it with `error_pages`), errors answer 502.

```yaml
routes:
  - id: cached-pages
    path_prefix: /
    target_type: memcached
    memcached: { pass: "127.0.0.1:11211", key: "$uri?$args", content_type: text/html, gzip_flag: 2 }
```

### GeoIP (`http.geoip`, build feature `geoip`, on by default)

MaxMind GeoLite2/GeoIP2 `.mmdb` files (the legacy `.dat` format of nginx's geoip module is not read):

```yaml
http:
  geoip:
    country_db: /usr/share/GeoIP/GeoLite2-Country.mmdb
    city_db: /usr/share/GeoIP/GeoLite2-City.mmdb
    asn_db: /usr/share/GeoIP/GeoLite2-ASN.mmdb
```

Variables (usable in `map`, `if`, `set`, `rewrite` and `proxy_set_headers`): `$geoip_country_code`, `$geoip_country_name`, `$geoip_continent_code`, `$geoip_city`, `$geoip_city_country_code`, `$geoip_region`, `$geoip_region_name`, `$geoip_postal_code`, `$geoip_latitude`, `$geoip_longitude`, `$geoip_org`, `$geoip_asn`. They use the client IP after `real_ip`.

### JSON variables (`type: json`)

nginx `json_set $user_id $request_body .user.id;` → `http.variables.user_id: { type: json, source: $request_body, path: .user.id }`. Paths use `.key`, `[index]` and `["key with spaces"]`; strings come out raw, other values as compact JSON, missing ones empty. When a variable reads `$request_body`, request bodies up to `http.proxy_request_buffer_size` are read into it (and still forwarded unchanged); larger ones leave it empty.

### CONNECT forward proxy (`tunnel`)

nginx `tunnel_pass`: `CONNECT host:port` requests on the route open a TCP connection and relay bytes both ways (HTTPS through a forward proxy). Other methods get the route's normal target.

```yaml
routes:
  - id: forward-proxy
    path_prefix: /
    target_type: return
    redirect_code: 405
    tunnel: { allowed_ports: [443], allowed_hosts: ["*.example.com"], idle_timeout_ms: 600000 }
    allowed_ips: [10.0.0.0/8]   # keep a forward proxy private
```

`pass: "10.0.0.5:5432"` sends every tunnel to one fixed target.

### More stream options

```yaml
streams:
  - id: pg
    listen: 0.0.0.0:5433
    upstream: postgres
    tls: { cert_path: /etc/ferrix/pg.pem, key_path: /etc/ferrix/pg.key }   # terminate TLS (listen ... ssl)
    upstream_tls: { verify: true, server_name: db.internal, trusted_ca: /etc/ferrix/ca.pem }   # proxy_ssl on
    allowed_ips: [10.0.0.0/8]       # allow / deny
    denied_ips: []
    limit_conn: 20                  # per client IP
    upload_rate: 1048576            # proxy_upload_rate, bytes/s
    download_rate: 1048576          # proxy_download_rate
  - id: banner
    listen: 0.0.0.0:7000
    return_text: "maintenance\n"    # stream return: send and close, no upstream
  - id: to-https
    listen: 0.0.0.0:8443
    pass: 127.0.0.1:443             # stream pass: hand the connection to a local listener
```

### HTTP/2 and HTTP/3 limits

```yaml
http:
  http2:
    max_concurrent_streams: 128      # http2_max_concurrent_streams
    max_header_list_size: 65536      # http2_max_header_size
    initial_stream_window: 65535     # http2_recv_buffer_size / http2_body_preread_size
    initial_connection_window: 1048576
    max_frame_size: 16384            # http2_chunk_size
    keepalive_interval_ms: 30000     # PING idle connections
  http3:
    max_concurrent_streams: 128      # http3_max_concurrent_streams
    stream_buffer_size: 65536        # http3_stream_buffer_size
    idle_timeout_ms: 75000
```

They apply after a restart (the reload warns about it).

### Request validation (nginx behaviour)

- Ambiguous requests get 400 and never reach a backend: `Content-Length` with `Transfer-Encoding`, two different `Content-Length`s, two `Host` headers, HTTP/1.1 without `Host`, lowercase or non-token methods.
- `http.allowed_methods`: methods accepted at all (default: HTTP and WebDAV methods, without TRACE); others get 405. Static routes answer only GET and HEAD (405 with `Allow: GET, HEAD` otherwise), unless WebDAV handles the method.
- `http.underscores_in_headers: false` (default): request headers with `_` are dropped, like nginx.
- `http.ignore_invalid_headers: true` (default): malformed header lines are skipped instead of 400.
- `http.max_header_line_bytes: 8192`: longer request lines get 414 and longer header lines 400.
- HTTP/0.9 (`GET /path` with no version) is answered with the bare body on plain HTTP ports, like nginx.
