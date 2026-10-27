# 03. Internal Architecture

**Ferrix** is engineered around modern distributed systems principles and asynchronous network I/O. This section explains the internal components and how they cooperate to deliver high throughput and zero-downtime operation.

---

## 🏛️ Structural Overview

```
                          ┌────────────────────────────────────────────────────────┐
                          │                      FERRIX PROCESS                    │
                          │                                                        │
Incoming HTTP / WS  ─────►│  Proxy Listener (Port 8000 / Configurable)             │
                          │  ├── Client Address Extractor (ConnectInfo)            │
                          │  ├── Host Header Matcher (VirtualHost Finder)          │
                          │  ├── Middleware Pipeline (IP ACL, RateLimit, Auth)     │
                          │  ├── Route Rule Prefix & Regex Matcher                 │
                          │  │   ├── Target: Static File Server (Zero-Copy)        │
                          │  │   ├── Target: HTTP Redirect (301/302/307/308)       │
                          │  │   └── Target: Upstream Pool (Load Balancer)         │
                          │  │       ├── WS Upgrade: Tokio Bidirectional Tunnel   │
                          │  │       └── HTTP: Hyper / Reqwest Client + Failover   │
                          │  │                                                     │
                          │  └── Telemetry Interceptor                             │
                          │      ├── Atomic Counters (2xx, 3xx, 4xx, 5xx, Bytes)   │
                          │      ├── Lock-Free Ring Buffer (Recent 1000 logs)      │
                          │      └── Broadcast Channel (tokio::sync::broadcast)    │
                          │                     │                                  │
                          │                     ▼                                  │
Browser / REST API  ─────►│  Admin Listener (Port 8081 / Configurable)             │
                          │  ├── REST API Handlers (/status, /config, etc.)        │
                          │  ├── Server-Sent Events (SSE Stream)                   │
                          │  └── Embedded React SPA (rust-embed Assets)            │
                          └────────────────────────────────────────────────────────┘
```

---

## ⚡ Zero-Downtime Hot Reload via `ArcSwap`

In traditional Nginx, reloading configuration without stopping the server (`nginx -s reload`) relies on the master process spawning new worker processes and waiting for old ones to terminate active connections (*graceful worker restart*).

In Ferrix, taking advantage of Rust's atomic pointer primitives, we use the **`arc-swap`** crate (`ArcSwap<FerrixConfig>`):

1. The runtime configuration is wrapped in an atomic pointer:
   ```rust
   pub shared_config: Arc<ArcSwap<FerrixConfig>>
   ```
2. During request processing, Tokio worker threads perform a quick lock-free load (`shared_config.load()`), requiring only a single atomic CPU instruction (no mutex locks, zero lock contention).
3. When new configuration is submitted through the web dashboard or REST API:
   - The configuration is validated;
   - Persisted safely to `ferrix.yaml` on disk;
   - Stored atomically into the pointer via `shared_config.store(Arc::new(new_config))`.
4. All incoming connections immediately see the updated routing table, while in-flight requests complete uninterrupted.
5. **Result**: Zero downtime, zero dropped connections, and sub-millisecond reload latency.

---

## 🔄 Full-Duplex Bidirectional WebSocket Tunnel

When a client sends a request with `Upgrade: websocket` and `Connection: Upgrade` headers, Ferrix automatically initiates a protocol upgrade:

1. The initial handshake is processed via Axum's `WebSocketUpgrade`.
2. Concurrently, Ferrix establishes an asynchronous connection to the target backend chosen by the load balancing algorithm using `tokio_tungstenite::connect_async`.
3. Once connected, two concurrent asynchronous tasks bridge the client and upstream:
   - Task A: reads frames from the client and forwards them to the backend.
   - Task B: reads frames from the backend and relays them to the client.
4. When either peer sends a `Close` frame, the tunnel terminates cleanly.

---

## 📊 Telemetry, Metrics, and Server-Sent Events (SSE)

The telemetry subsystem is designed to introduce zero overhead to the critical request path:

- **Atomic Counters**: `total_requests`, `active_connections`, `status_2xx`, `status_3xx`, `status_4xx`, `status_5xx`, and `total_bytes_sent` utilize `AtomicU64` with relaxed ordering (`Ordering::Relaxed`).
- **Latency Percentiles (p50, p95, p99)**: Computed over a circular ring buffer tracking the last 1,000 requests.
- **Real-Time Streaming**: Each finished request emits a `RequestLog` to an asynchronous broadcast channel (`tokio::sync::broadcast::Sender`).
- **SSE Endpoint (`/api/logs/stream`)**: The React dashboard consumes this event stream over `EventSource`, updating the live requests table without polling.

---

## 📁 Source Code Organization

The core follows the same layering as nginx's `src/` directory: a base layer with no
HTTP in it, the event loop, the HTTP server split into small modules (one per nginx
module it mirrors), and the OS-specific bits apart.

```
core/src/
├── main.rs              process bootstrap, CLI, signals, worker threads  (nginx.c, os/unix/ngx_process*)
├── config/              JSON config model, validation, hot-reload store   (ngx_conf_file + each module's directives)
├── event/
│   ├── accept.rs        accept loop, worker_connections, hyper handoff    (event/ngx_event_accept.c)
│   └── clock.rs         cached time + per-worker coarse timers           (core/ngx_times.c, event/ngx_event_timer.c)
├── os/                  RLIMIT_NOFILE, CPU affinity                       (os/unix)
├── tls/
│   ├── mod.rs           which ports speak TLS
│   ├── sni.rs           certificate per SNI name, rustls acceptor         (event/ngx_event_openssl.c, ngx_http_ssl_module)
│   └── acme.rs          Let's Encrypt certificates
├── http/
│   ├── mod.rs           request lifecycle: vhost → route → handler → filters   (ngx_http_core_module)
│   ├── server.rs        Ferrix's own HTTP/1.1 parser/writer, sendfile         (ngx_http_request.c, ngx_http_parse.c)
│   ├── matcher.rs       server_name and location matching                     (ngx_http_core_module locations)
│   ├── proxy.rs         reverse proxy handler, failover                       (ngx_http_proxy_module)
│   ├── static_files.rs  static files, ETag/304, ranges, precompressed copies  (static, not_modified, range, gzip_static)
│   ├── cache.rs         response cache                                        (proxy_cache)
│   ├── access.rs        allow/deny + basic auth                               (access, auth_basic)
│   ├── limit_req.rs     rate limiting                                         (limit_req)
│   ├── limit_conn.rs    connection limiting                                   (limit_conn)
│   ├── realip.rs        client IP from trusted proxies                        (realip)
│   ├── websocket.rs     WebSocket tunnel                                      (proxy upgrade)
│   ├── default_page.rs  "Welcome to Ferrix" page                              (default site)
│   ├── filters/         response filter chain, run in order:                  (header/body filters)
│   │                    compress → headers → cors → request_id, plus error_page
│   └── upstream/
│       ├── mod.rs       client to upstreams (timeouts, retries on stale conns) (ngx_http_upstream.c)
│       ├── h1.rs        HTTP/1.1 client with a per-thread connection pool      (upstream_keepalive)
│       ├── balancer.rs  round robin, weighted, least_conn, ip_hash, random     (upstream_*_module)
│       └── hash.rs      consistent hashing (ketama)                            (upstream_hash)
├── stream.rs            TCP/UDP proxy                                         (src/stream/)
├── health/              active health checks
├── metrics/             counters, latency histogram, request log, Prometheus  (stub_status, much extended)
├── admin/               control-plane REST API and SSE
├── net/                 CIDR matching
└── nginx/               nginx.conf importer (lexer, parser, converter, report)
```

**Adding an HTTP feature:** a new response filter goes in `http/filters/` and is called
from `filters::run` in the right order; a new content handler gets its own file under
`http/` and a `RouteTargetType`; a new balancing algorithm goes in
`http/upstream/balancer.rs`.
