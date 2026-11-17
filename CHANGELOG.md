# Changelog

All notable changes to **Ferrix** will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.3.0] - 2026-11-17

### Added
- Request hardening like nginx: ambiguous requests (`Content-Length` + `Transfer-Encoding`, duplicate `Host`), unknown methods, underscores in header names and oversized header lines are refused; HTTP/0.9 works.
- Static files: percent-encoded paths, directory redirects, multipart byte ranges, HTML error pages, `default_type` / `mime_types`, `etag`, `if_modified_since`, `max_ranges`.
- Proxy parity with nginx: only `proxy_set_headers` are sent, `Host` is the upstream name, normalized paths, `proxy_hide_headers`, `proxy_pass_headers`, `proxy_method`, `proxy_set_body`.
- htpasswd files and hashed passwords for basic auth; `auth_delay`.
- Named routes (`@name`), `internal` routes, `error_page` redirects and `try_files` fallbacks.
- Cache rules (`key`, `valid_by_status`, `methods`, `min_uses`, `no_cache`, `bypass`) and disk / Redis cache stores.
- `logging`: error and access log destinations (file with rotation, syslog, HTTP, stdout/stderr, off) and JSON format.
- TLS: `http.tls` (protocols, ciphers, session tickets, fixed ticket key), encrypted private keys, client and upstream CRLs, `reject_handshake`.
- `keepalive_requests`, `keepalive_time_ms` and graceful shutdown (`shutdown_timeout_ms`).
- `autoindex_format` (html/json/xml), `disable_symlinks`, dry-run limits, `proxy_store`, `post_action`, mirrors with the request body, SSI and stream options, more variables.
- Configuration split in folders: `sites/`, `upstreams/`, `streams/` next to `ferrix.yaml` (`ferrix split-config`).
- Exact compression levels (`gzip_level`, `brotli_level`, `zstd_level`).
- Dashboard: workers and protocols, request validation, static files, Let's Encrypt account, logging, TLS, compression, connection limits; CORS and compression per route.

### Changed
- Proxied requests carry only the headers in `proxy_set_headers` and `Host` is the upstream name, like nginx; `http.proxy_forwarded_headers: true` restores the automatic `X-Forwarded-*` headers.
- `if_modified_since` defaults to `exact` and keep-alive connections close after 1000 requests (`keepalive_requests`), like nginx.
- `ferrix stop` waits up to 30 s (`shutdown_timeout_ms`) for requests in flight.

### Fixed
- FastCGI: nginx variables in imported `fastcgi_param` lines are expanded; read timeout applies between reads; a sent POST is not repeated on another target.
- nginx import: relative `include` inside an included file resolves from the configuration root.
- Requests in flight are no longer cut on `ferrix stop` / SIGTERM.
- Requests refused while parsing (400/405) are written to the access log.
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

