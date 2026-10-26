# Changelog

All notable changes to **Ferrix** will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added
- Compression (zstd, brotli, gzip) negotiated from `Accept-Encoding`, with static files compressed once and cached.
- `ETag` / `Last-Modified` revalidation (`304`) and single byte ranges (`206`) for static files.
- Several TLS certificates per port, chosen by SNI and reloaded on config changes.
- Automatic certificates via ACME / Let's Encrypt (`tls.acme`, HTTP-01).
- TCP/UDP stream proxying (`streams`), including import of nginx `stream {}` blocks.
- `consistent_hash` load balancing (ketama ring on a configurable key).
- Per-route CORS, per-vhost custom error pages and optional `X-Request-Id`.
- Prometheus endpoint (`/metrics` on the admin port).
- `admin.enabled` to run without the control plane.
- The nginx importer converts `gzip*`, `hash ... consistent`, `error_page` and `stream`.

### Changed
- **Metrics are now opt-in** (`metrics.enabled`, default `false`). When disabled the proxy does no per-request bookkeeping.
- Upstream requests use a built-in HTTP/1.1 client with a per-thread connection pool (HTTPS upstreams use rustls directly instead of reqwest).
- Proxy ports are served by Ferrix's own accept loop (`TCP_NODELAY`, header/keep-alive timeouts applied).
- Release builds use LTO and the mimalloc allocator; rate limiting and metrics counters are sharded to avoid cross-core contention.
- The benchmark runs entirely in Docker, including Ferrix.

- GitHub Actions CI/CD workflows for multi-platform test, lint, and automated binary releases.
- Automated GitHub Pages documentation deployment using mdBook.
- High-resolution widescreen repository banner (`docs/assets/ferrix-banner.png`).
- Standardized Semantic Versioning (SemVer) across workspace and packages.

---

## [0.1.0] - 2026-10-25

### Added
- **Core Engine (Rust)**:
  - High-performance asynchronous HTTP/1.1 & WebSocket reverse proxy engine using Tokio and Hyper.
  - Multi-host Virtual Host routing (wildcards, regex, exact host matching).
  - Load balancing algorithms: `round_robin`, `weighted_round_robin`, `least_conn`, `ip_hash`, and `random`.
  - Active background health checks and passive failover (`proxy_next_upstream`).
  - In-memory response caching with LRU eviction and cache-bypass rules (`Cache-Control`, cookies, query params).
  - Zero-downtime hot reloading powered by atomic pointers (`ArcSwap`).
  - Rate limiting (token bucket) per IP and CIDR ACL access controls (allow/deny lists).
  - Static file serving with MIME detection and SPA `try_files` routing.
  - TLS / HTTPS termination using rustls.
  - Nginx configuration parser and converter (`ferrix import-nginx`).
- **Control Plane & Admin API**:
  - RESTful management API with authentication modes (none, bearer token, basic auth).
  - Live metric collector (RPS, latencies, p95/p99, error rates).
  - Real-time live request inspection via Server-Sent Events (SSE).
- **Web Dashboard (Frontend)**:
  - Modern React + TypeScript + Tailwind CSS administrative interface.
  - Interactive route builder, visual upstream pool monitor, and live telemetry viewer.

