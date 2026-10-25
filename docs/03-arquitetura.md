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
   - Persisted safely to `ferrix.json` on disk;
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

| Module / Path               | Responsibility                                                                             |
| --------------------------- | ------------------------------------------------------------------------------------------ |
| `core/src/main.rs`          | Tokio runtime bootstrapping, CLI parsing, signal handling, and listener spawning.          |
| `core/src/config/`          | Configuration models (`VirtualHost`, `RouteRule`, `UpstreamConfig`) and JSON store.        |
| `core/src/proxy/mod.rs`     | Main routing engine, domain matching, failover logic, and reverse proxy handler.          |
| `core/src/proxy/upstream.rs`| Upstream pool management and implementation of all 5 load balancing algorithms.            |
| `core/src/proxy/websocket.rs`| Full-duplex bidirectional WebSocket tunneling.                                            |
| `core/src/proxy/static_files.rs`| Static file serving with MIME detection and `try_files` SPA routing.                   |
| `core/src/proxy/middleware.rs`| Rate limiting (Token Bucket), IP CIDR access control lists, and HTTP Basic Auth.         |
| `core/src/admin/mod.rs`     | Control plane HTTP router, CORS configuration, and embedded static asset delivery.         |
| `core/src/admin/handlers.rs`| REST API handlers (`/status`, `/metrics`, `/config`, etc.).                                |
| `core/src/admin/sse.rs`     | Broadcast channel adapter for Server-Sent Events (SSE).                                    |
| `core/src/metrics/mod.rs`   | Statistical metrics collection and latency percentile calculations.                        |
| `core/src/health/checker.rs`| Background health checker pinging upstream backends periodically.                          |
