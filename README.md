# Ferrix — A high-performance web server and reverse proxy in Rust, inspired by Nginx

**Ferrix** is an asynchronous web server and reverse proxy built in **Rust** (Tokio, Hyper and Axum). It comes with a modern admin web UI in **React + Tailwind CSS** and a **REST API** for dynamic configuration without restarts (_zero-downtime hot-reload_), real-time metrics and live request inspection via Server-Sent Events (SSE).

> **Where does the name come from?** _Ferrix_ combines **ferr-**, from _ferrum_ (Latin for iron) — because iron, over time, turns into **rust**, the language it is written in — with the trailing **-x** of **nginx**, the server that inspired it. In other words: an nginx made of rust. 🦀

---

## 🚀 Features

1. **Reverse proxy with WebSocket support**:
   - Transparent, asynchronous full-duplex tunneling for `Upgrade: websocket` connections.
   - Proxy header forwarding (`Host`, `X-Real-IP`, `X-Forwarded-For`, `X-Forwarded-Proto`, custom headers).
   - Streaming and Server-Sent Events support without buffering whole responses in memory.

2. **Load balancing and high availability**:
   - **Supported algorithms**:
     - `round_robin`: uniform cyclic distribution.
     - `weighted_round_robin`: distribution proportional to per-server weights.
     - `least_conn`: forwards to the server with the fewest active connections.
     - `ip_hash`: sticky sessions based on the client IP.
     - `random`: random choice among healthy nodes.
   - **Active and passive health checks**:
     - Periodic background health monitoring of backends (path, interval and expected status).
     - Automatic failover (_proxy_next_upstream_): if a node returns 502/504 or times out, Ferrix immediately retries the next server in the pool.

3. **Static files and SPA routing**:
   - Fast static file serving with MIME type detection via `mime_guess`.
   - Nginx-style **`try_files`** directive for single-page applications (React, Vue, Angular, etc.).
   - Cache headers (`Cache-Control`) and directory traversal (`../`) protection.

4. **Traffic rules, security and headers**:
   - **Rate limiting**: token bucket algorithm with per-IP rate and burst.
   - **Access control**: IP allow/deny lists.
   - **Basic authentication**: HTTP Basic Auth per route or per virtual host.
   - **Redirects and rewrites**: native 301, 302, 307 and 308 support.

5. **Zero-downtime hot-reload**:
   - Changes made in the web UI or via the REST API are applied instantly in memory using atomic pointers (`ArcSwap`) and persisted to `ferrix.json`, without closing sockets or dropping active connections.

6. **React web dashboard and telemetry API**:
   - Metrics panel with RPS, average latency, p95, p99 and HTTP status distribution (2xx, 3xx, 4xx, 5xx).
   - Live request inspection table via **SSE**, with full details for each request.
   - Visual manager for virtual hosts, routes and upstream pools.
   - Configuration editor with syntax validation and a "validate & hot-reload" button.

---

## 🛠️ Project architecture

Ferrix is split into 3 independent, decoupled layers:

```
ferrix/
├── core/                  # 🦀 Rust core (Nginx-style proxy engine, standalone and lightweight)
│   ├── Cargo.toml         # Rust package, no dependency on web assets
│   ├── src/               # Proxy engine, load balancing, WebSockets, cache, CLI
│   ├── tests/             # Unit and integration tests
│   └── scripts/           # Install scripts: install.sh (Linux/systemd), install.ps1 (Windows)
│
├── backend/               # 🟢 Management backend in Node.js + Express (TypeScript)
│   ├── package.json
│   ├── tsconfig.json
│   └── src/server.ts      # REST API, proxy to the core, SSE stream and dashboard server
│
├── frontend/              # ⚛️ Web dashboard in React + Vite + Tailwind CSS
│   ├── package.json
│   ├── src/               # Components, metrics screens and auth modal
│   └── dist/              # Built assets served by the Node.js backend
│
├── static_demo/           # Demo folder of static files served by the core
├── docs/                  # Detailed documentation (Portuguese): overview, configuration, API...
├── ferrix.json            # Local config (generated on first run, not tracked by git)
└── test_backends.js       # Mock servers for load-balancing tests
```

---

## 🏁 Running and installing

### 1. Requirements

- **Rust & Cargo** (1.75+)
- **Node.js** (v18+) & **npm**

### 2. Running in development

```bash
# 1. Start the Rust core (proxy on :8000, control plane on :8081):
cargo build --release --workspace
./target/release/ferrix --config ferrix.json

# 2. Start the Node.js backend (API and dashboard on :8080):
cd backend
npm install
npm run build
npm start

# 3. (Optional) Start the mock servers to try out load balancing:
node test_backends.js
```

Then:

1. Open **`http://localhost:8080`** in your browser to access the dashboard.
2. Enter the admin token. On first run, Ferrix creates `ferrix.json` with a random token (the `token` field); use that value.
3. Send requests to `http://localhost:8000/api` and watch the balanced traffic live on the dashboard!

### 📦 3. Installing the Ferrix core system-wide

The **Ferrix core** can be installed as a system utility (`/usr/local/bin/ferrix` or `C:\Program Files\Ferrix\ferrix.exe`) with no Node.js dependency:

#### On Linux (with an automatic systemd service):

```bash
sudo ./core/scripts/install.sh
```

#### On Windows (PowerShell as Administrator):

```powershell
.\core\scripts\install.ps1
```

---

## 📡 Management REST API

| Method   | Endpoint               | Description                                           |
| -------- | ---------------------- | ----------------------------------------------------- |
| `GET`    | `/api/status`          | Uptime, ports and counters                            |
| `GET`    | `/api/metrics`         | Metrics snapshot (RPS, p95 latency, status codes)     |
| `GET`    | `/api/logs`            | Paginated log history with search and filters         |
| `GET`    | `/api/logs/stream`     | Live request stream via Server-Sent Events (SSE)      |
| `GET`    | `/api/config`          | Active JSON configuration                             |
| `POST`   | `/api/config`          | Updates the global config with atomic hot-reload      |
| `GET`    | `/api/vhosts`          | List virtual hosts                                    |
| `POST`   | `/api/vhosts`          | Create or update a virtual host                       |
| `DELETE` | `/api/vhosts/:id`      | Delete a virtual host                                 |
| `GET`    | `/api/upstreams`       | List upstream pools and their health                  |
| `POST`   | `/api/upstreams`       | Create or update an upstream pool                     |
| `DELETE` | `/api/upstreams/:name` | Delete an upstream pool                               |

Full documentation (in Portuguese) in [`docs/`](docs/README.md).

---

## 📊 Benchmark: Ferrix vs. nginx

An honest comparison against nginx 1.27 on the same machine, so you know what to expect today.

**Setup:** both servers pinned to 4 CPU cores (`worker_threads: 4` vs. `worker_processes 4`), nginx with `sendfile on` and upstream keepalive, load generated by `wrk` (8 threads) on separate cores, upstream being a separate nginx returning a 20-byte body. Each test ran twice for 10 s after a warm-up; numbers are averages.

| Scenario (256 connections)   | Ferrix req/s          | nginx req/s          | Ferrix p99 | nginx p99 |
| ---------------------------- | --------------------- | -------------------- | ---------- | --------- |
| Static file, 1.4 KB          | ~14.7k                | ~56k                 | 33 ms      | 6–9 ms    |
| Static file, 133 KB          | ~6.5k (0.83 GB/s)     | ~30k (3.7 GB/s)      | 68 ms      | 10–12 ms  |
| Reverse proxy                | ~18.8k                | ~51k                 | 23 ms      | 6–7 ms    |

At 50 connections the ratios were the same. Memory (RSS) after the runs: Ferrix ~53 MB, nginx ~11 MB.

**Takeaway:** nginx is currently 2.7–4.5× faster in raw throughput and has much lower tail latency. Ferrix's strengths today are its features — web dashboard, API-driven hot-reload, built-in metrics and live request inspection — not raw speed. Likely areas for optimization (not yet profiled): zero-copy static file serving (`sendfile`) and open-file caching, upstream connection reuse, and per-request metrics/logging overhead. Performance contributions are very welcome!

---

## 🤝 Contributing

Contributions are very welcome! Open an _issue_ to report bugs or suggest ideas, or send a _pull request_. See [CONTRIBUTING.md](CONTRIBUTING.md) for the step-by-step guide.

## 📄 License

Distributed under the MIT License. See [LICENSE](LICENSE).
