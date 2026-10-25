# Changelog

All notable changes to **Ferrix** will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added
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

