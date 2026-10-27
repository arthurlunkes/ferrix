<p align="center">
  <img src="assets/ferrix-banner.png" alt="Ferrix Banner" width="100%">
</p>

# Ferrix Official Documentation

Welcome to the official documentation for **Ferrix** — a high-performance web server and reverse proxy inspired by Nginx, built in **Rust** (Tokio, Hyper, Axum), featuring a modern **React Web Dashboard** and a **RESTful Control Plane API** for zero-downtime hot reloading.

---

## 📚 Table of Contents

1. [**01. Overview and Comparison**](01-visao-geral.md)
   - What Ferrix is and project motivation.
   - Architectural comparison with Nginx and Caddy.
   - Memory safety and concurrency advantages of Rust.

2. [**02. Installation and Execution**](02-instalacao-e-execucao.md)
   - System prerequisites.
   - Building the core proxy (Rust) and dashboard (React).
   - Running via CLI, command flags, and background daemon execution.
   - Containerization with Docker.

3. [**03. Internal Architecture**](03-arquitetura.md)
   - Asynchronous runtime with Tokio, Hyper, and Axum.
   - Zero-Downtime Hot-Reload via atomic pointer swaps (`ArcSwap`).
   - Metric collector, lock-free circular log ring buffer, and Server-Sent Events (SSE).
   - Codebase structure and modularity.

4. [**04. Feature & Configuration Guide**](04-guia-de-configuracao.md)
   - `ferrix.yaml` configuration structure.
   - Virtual Hosts, domain matching, and prefix/regex routing rules.
   - Load balancing algorithms (Round-Robin, Weighted, Least Connections, IP Hash, Random).
   - Active health checks and automated failover (`proxy_next_upstream`).
   - Transparent WebSocket tunneling.
   - Static file serving and `try_files` for Single Page Applications (SPAs).
   - Rate limiting (Token Bucket), IP CIDR ACLs (Allow/Deny), and HTTP Basic Auth.

5. [**05. REST API Reference**](05-api-rest.md)
   - Complete endpoint specification and SSE live stream.
   - Practical `curl` examples and JSON payloads.
   - CRUD management for Virtual Hosts and Upstreams via API.

6. [**06. Web Dashboard Manual**](06-dashboard-web.md)
   - Overview of all views (Dashboard, Virtual Hosts, Live Requests, Upstreams, Nginx Import, Config Editor).
   - Real-time request inspection and telemetry.
   - Visual configuration management without manually editing files.

7. [**07. Glossary**](07-glossario.md)
   - What p50, p99, req/s, RSS/PSS, sendfile, SNI, ACME and the other terms mean.
