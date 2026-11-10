<p align="center">
  <img src="docs/assets/ferrix-banner.png" alt="Ferrix — Rust-based HTTP server" width="100%">
</p>

<p align="center">
  <a href="https://github.com/arthurlunkes/ferrix/actions/workflows/ci.yml"><img src="https://github.com/arthurlunkes/ferrix/actions/workflows/ci.yml/badge.svg" alt="CI"></a>
  <a href="https://arthurlunkes.github.io/ferrix"><img src="https://img.shields.io/badge/docs-GitHub%20Pages-blue.svg" alt="Documentation"></a>
  <a href="https://semver.org"><img src="https://img.shields.io/badge/semver-2.0.0-orange.svg" alt="SemVer"></a>
  <a href="LICENSE"><img src="https://img.shields.io/badge/license-Apache--2.0-blue.svg" alt="License: Apache-2.0"></a>
  <a href="CONTRIBUTING.md"><img src="https://img.shields.io/badge/PRs-welcome-brightgreen.svg" alt="PRs welcome"></a>
  <a href="https://github.com/arthurlunkes/ferrix/issues?q=is%3Aissue+is%3Aopen+label%3A%22good+first+issue%22"><img src="https://img.shields.io/github/issues/arthurlunkes/ferrix/good%20first%20issue?label=good%20first%20issues&color=7057ff" alt="Good first issues"></a>
</p>

# Ferrix — A high-performance web server and reverse proxy in Rust, inspired by Nginx

**Ferrix** is an asynchronous web server and reverse proxy built in **Rust** (Tokio, Hyper and Axum). It comes with a modern admin web UI in **React + Tailwind CSS** and a **REST API** for dynamic configuration without restarts (_zero-downtime hot-reload_), real-time metrics and live request inspection via Server-Sent Events (SSE).

> **Where does the name come from?** _Ferrix_ combines **ferr-**, from _ferrum_ (Latin for iron) — because iron, over time, turns into **rust**, the language it is written in — with the trailing **-x** of **nginx**, the server that inspired it. In other words: an nginx made of rust. 🦀

> **🌱 Ferrix is open source (Apache-2.0) and looking for contributors!** Whether you know Rust, React, or just want to improve the docs or test it against your nginx configs, your help is welcome. Jump to [How you can help](#-how-you-can-help).

---

## 🚀 Features

1. **Reverse proxy with WebSocket support**:
   - Transparent, asynchronous full-duplex tunneling for `Upgrade: websocket` connections.
   - Proxy header forwarding (`Host`, `X-Real-IP`, `X-Forwarded-For`, `X-Forwarded-Proto`, custom headers).
   - Streaming and Server-Sent Events support without buffering whole responses in memory.
   - **HTTP/1.1, HTTP/2 and HTTP/3 (QUIC)**, each switched on or off in `http.protocols`.

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
   - Changes made in the web UI or via the REST API are applied instantly in memory using atomic pointers (`ArcSwap`) and persisted to `ferrix.yaml`, without closing sockets or dropping active connections.

6. **HTTPS, compression and layer 4**:
   - Several certificates per port chosen by **SNI**, and automatic **Let's Encrypt** certificates (ACME HTTP-01).
   - **zstd / brotli / gzip** compression; static files are compressed once and cached.
   - `ETag`/`Last-Modified` revalidation (304) and byte ranges (206) for static files.
   - **TCP/UDP stream proxying** (nginx `stream {}`), `consistent_hash` balancing, per-route CORS, custom error pages and `X-Request-Id`.

7. **Optional React web dashboard and telemetry API**:
   - Metrics are **opt-in** (`metrics: { enabled: true }` in `ferrix.yaml`), like nginx's `stub_status`. When they're off, the proxy does no per-request bookkeeping. When they're on, Prometheus `/metrics` and the dashboard light up.
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
│   ├── src/               # Laid out like nginx's src/: event/, http/ (+ filters/, upstream/), tls/, os/...
│   ├── tests/             # Unit and integration tests
│   └── scripts/           # Install scripts: install.sh (Linux/systemd), install.ps1 (Windows)
│
├── frontend/              # ⚛️ Web dashboard in React + Vite + Tailwind CSS
│   ├── package.json
│   ├── src/               # Components, metrics screens and auth modal
│   └── dist/              # Built assets, served by the core itself (admin.dashboard_dir)
│
├── static_demo/           # Demo folder of static files served by the core
├── docs/                  # Detailed documentation (Portuguese): overview, configuration, API...
├── ferrix.yaml            # Local config (generated on first run, not tracked by git)
└── test_backends.js       # Mock servers for load-balancing tests
```

---

## 🏁 Running and installing

### 1. Requirements

- **Rust & Cargo** (1.75+)
- **Node.js** (v18+) & **npm**, only to build the web dashboard

### 2. Running in development

```bash
# 1. Build the web dashboard (optional, only needed for the UI):
(cd frontend && npm install && npm run build)

# 2. Start Ferrix (proxy on :80, admin API on :8081). The first run creates
#    ferrix.yaml, with every option commented out and a random admin token.
#    Port 80 needs privileges: run once `sudo setcap cap_net_bind_service=+ep
#    target/release/ferrix`, or add `--http-listen 0.0.0.0:8080`.
cargo build --release --workspace
./target/release/ferrix run --config ferrix.yaml

# 3. (Optional) Start the mock servers to try out load balancing:
node test_backends.js
```

Then:

1. To serve the dashboard, set `admin.dashboard_dir: frontend/dist` and `metrics: { enabled: true }` in `ferrix.yaml`, run `ferrix reload` (or restart), and open **`http://localhost:8081`**. There is no separate web server: Ferrix serves the dashboard and its API itself.
2. Enter the admin token (the `admin.auth.token` field Ferrix generated in `ferrix.yaml`).
3. Send requests to `http://localhost/api` and watch the balanced traffic live on the dashboard!

### 🐳 3. Running with Docker (GitHub Packages)

You can choose between the **ultra-lean Core image** (~20 MB) and the **Full All-in-One image** with the Web Dashboard:

#### Option A: Ultra-lean Core only (~20 MB, pure Rust proxy & API)

```bash
docker run -d \
  --name ferrix \
  -p 80:80 \
  -p 8081:8081 \
  -v $(pwd)/ferrix.yaml:/etc/ferrix/ferrix.yaml \
  --restart unless-stopped \
  ghcr.io/arthurlunkes/ferrix:latest
```

#### Option B: Full, with the web dashboard on :8080

One Ferrix process serves the proxy and the dashboard (no Node.js in the image). The first start creates `ferrix.yaml` in the mounted directory with a random admin token, printed in the container log:

```bash
docker run -d \
  --name ferrix-full \
  -p 80:80 \
  -p 8080:8080 \
  -v $(pwd)/ferrix-data:/etc/ferrix \
  --restart unless-stopped \
  ghcr.io/arthurlunkes/ferrix:full
docker logs ferrix-full   # shows the admin token
```

Or using Docker Compose:

```bash
docker compose up ferrix-core   # Run ultra-lean Core
# or
docker compose up ferrix-full   # Run All-in-One with Web Dashboard
```

### 📦 4. Installing Ferrix system-wide

#### On Linux: one command, any distribution

```bash
curl -fsSL https://arthurlunkes.github.io/ferrix/install.sh | sh
```

It uses your package manager: the Ferrix APT repository on Debian/Ubuntu (updates come with `apt upgrade`), the `.rpm` on Fedora/RHEL/openSUSE and the Arch package on Arch. Ferrix then runs as a systemd service on port 80; open `http://localhost/`. Details, manual steps and removal: [Installation guide](docs/02-instalacao-e-execucao.md#-installing-on-linux).

From a checkout of this repository, `./core/scripts/install.sh` builds and installs it instead.

#### On macOS (Homebrew)

```bash
brew tap arthurlunkes/ferrix https://github.com/arthurlunkes/ferrix
brew install ferrix
```

#### On Windows (Scoop)

```powershell
scoop install https://raw.githubusercontent.com/arthurlunkes/ferrix/main/packaging/scoop/ferrix.json
```

#### On Windows (PowerShell as Administrator):

```powershell
.\core\scripts\install.ps1
```

---

## 📡 Management REST API

| Method   | Endpoint               | Description                                       |
| -------- | ---------------------- | ------------------------------------------------- |
| `GET`    | `/api/status`          | Uptime, ports and counters                        |
| `GET`    | `/api/metrics`         | Metrics snapshot (RPS, p95 latency, status codes) |
| `GET`    | `/api/logs`            | Paginated log history with search and filters     |
| `GET`    | `/api/logs/stream`     | Live request stream via Server-Sent Events (SSE)  |
| `GET`    | `/api/config`          | Active JSON configuration                         |
| `POST`   | `/api/config`          | Updates the global config with atomic hot-reload  |
| `GET`    | `/api/config/yaml`     | Active configuration as `ferrix.yaml` text        |
| `POST`   | `/api/config/yaml`     | Applies YAML text (validated, saved, hot-reloaded) |
| `GET`    | `/api/vhosts`          | List virtual hosts                                |
| `POST`   | `/api/vhosts`          | Create or update a virtual host                   |
| `DELETE` | `/api/vhosts/:id`      | Delete a virtual host                             |
| `GET`    | `/api/upstreams`       | List upstream pools and their health              |
| `POST`   | `/api/upstreams`       | Create or update an upstream pool                 |
| `DELETE` | `/api/upstreams/:name` | Delete an upstream pool                           |

Full documentation (in Portuguese) in [`docs/`](docs/README.md).

---

## 📊 Benchmark: Ferrix vs. nginx, Traefik, Pingora, Pingap and Ferron

An honest comparison on the same machine. Everything needed to reproduce it lives in [`benchmarks/`](benchmarks/), and **everything runs in Docker with pinned versions**, so results don't depend on what's installed on the host. The script can compare Ferrix with **nginx 1.27, Traefik 3.5.6, Pingora 0.9 (Cloudflare), Pingap 0.16.1 and Ferron 2.8.1**; each server runs alone on the same 4 cores. Traefik and Pingora are proxies only, so they skip the static-file scenarios. Pingora is a library, so [`benchmarks/pingora/`](benchmarks/pingora/) holds a minimal proxy built from its official example.

```bash
benchmarks/run.sh                       # all six servers, 10 s per scenario
benchmarks/run.sh ferrix nginx -d 30s   # only these two, 30 s per scenario
METRICS=1 benchmarks/run.sh ferrix      # Ferrix with metrics enabled
```

For meaningful numbers, close other heavy programs (browsers, IDEs, other containers) first: they aren't pinned to the benchmark's cores and add noise to every server.

> Unfamiliar with a term (p50, p99, PSS, sendfile...)? See the [glossary](docs/07-glossario.md).

**Setup:** every server pinned to the same 4 CPU cores with 4 workers/threads (`worker_threads: 4` for Ferrix, `worker_processes 4` for nginx, and so on), nginx with `sendfile on` and upstream keepalive, load generated by `wrk` (8 threads, 256 connections) on separate cores, and the upstream is a separate nginx returning a 20-byte body. Requires Docker and a machine with 16+ logical cores.

**Latest results** (default configs, metrics off; each server measured alone with `benchmarks/run.sh`, 10 s per scenario. Ferrix and nginx are the mean of 5 runs each (req/s standard deviation under 4%), on a machine that was not idle; the other four come from a single earlier run on the same machine; absolute numbers depend on the hardware, so compare servers, not machines):

| Server | 1.4 KB file req/s | p99 | 133 KB file req/s | p99 | Proxy req/s | p99 | Memory (PSS) |
| --- | --- | --- | --- | --- | --- | --- | --- |
| **Ferrix** | **164.3k** | **2.1 ms** | 45.9k | **6.6 ms** | **92.4k** | **3.8 ms** | **7.0 MB** |
| nginx 1.27 | 101.8k | 3.0 ms | **47.1k** | 6.7 ms | 81.1k | 4.0 ms | 10.8 MB |
| Pingora 0.9 | – | – | – | – | 42.0k | 11.5 ms | 31.3 MB |
| Pingap 0.16.1 | 21.0k | 17.7 ms | 2.4k | 132 ms | 31.5k | 15.2 ms | 67.1 MB |
| Ferron 2.8.1 | 39.5k | 14.3 ms | 17.4k | 45.8 ms | 30.3k | 15.5 ms | 65.8 MB |
| Traefik 3.5.6 | – | – | – | – | 22.7k | 27.6 ms | 167.4 MB |

"–": Traefik and Pingora are proxies only and don't serve files. The other servers run with the minimal configs in [`benchmarks/conf/`](benchmarks/conf/) (4 workers/threads each, no extra tuning), so they may do better with tuning their own docs recommend; contributions that improve those configs are welcome. Pingora is measured through the minimal proxy in [`benchmarks/pingora/`](benchmarks/pingora/).

**Memory in detail**, measured in a fresh container for each situation, right after that load alone (`wrk`, 256 connections, 8 s). The "Memory" column of the table above is different: it's measured once, after all three scenarios ran back to back. Values are **PSS** (proportional set size), the fair number for comparing a single process (Ferrix, Traefik, Pingora, Pingap, Ferron) with nginx's master + 4 workers: plain RSS counts the memory nginx's processes share (binary, libraries) once per process, which would make nginx look about twice as big (~21 MB).

| Server | Idle | After 1.4 KB file load | After 133 KB file load | After proxy load |
| --- | --- | --- | --- | --- |
| **Ferrix** | **3.6 MB** | **6.0 MB** | **5.9 MB** | **5.9 MB** |
| nginx 1.27 | 10.5 MB | 10.7 MB | 10.6 MB | 10.8 MB |
| Pingora 0.9 | 5.2 MB | – | – | 18.8 MB |
| Pingap 0.16.1 | 22.5 MB | 40.8 MB | 46.0 MB | 39.6 MB |
| Ferron 2.8.1 | 19.9 MB | 42.3 MB | 44.8 MB | 59.9 MB |
| Traefik 3.5.6 | 80.4 MB | – | – | 177.9 MB |

Before the memory work, Ferrix's RSS was 25 MB idle and 56–68 MB under these loads. About 6 MB of nginx's figure is its connection tables, preallocated for `worker_connections 4096`; with 512 it idles at 4.5 MB.

With `metrics.enabled` the cost is about 4–9%.

**Multi-megabyte files** (`LARGE=only benchmarks/run.sh`; random, incompressible files; 256 connections for 1 and 10 MB, 32 for 100 MB; Ferrix and nginx are the median of 3 runs, Pingap and Ferron a single run):

| Server | 1 MB req/s | p99 | 10 MB req/s | p99 | 100 MB req/s | p99 | Memory (PSS) |
| --- | --- | --- | --- | --- | --- | --- | --- |
| **Ferrix** | **12.1k** (12 GB/s) | **34 ms** | 1.24k (12 GB/s) | 378 ms | **99** (10 GB/s) | 2.7 s | **8.1 MB** |
| nginx 1.27 | 7.4k (7 GB/s) | 55 ms | **1.28k** (13 GB/s) | **336 ms** | 79 (8 GB/s) | **2.0 s** | 10.8 MB |
| Ferron 2.8.1 | 2.6k | 194 ms | 228 | 1.6 s | 27 | 1.7 s | 60.3 MB |
| Pingap 0.16.1 | 381 | 712 ms | 25 | 6.6 s | 3.2 | 7.4 s | 64.3 MB |

From 10 MB up Ferrix and nginx are level: both hand the file to the kernel with `sendfile`, the loopback copy is the limit (10–13 GB/s on this machine), and the runs overlap (nginx ranged 0.70k–1.29k req/s at 10 MB and 66–102 at 100 MB; Ferrix 1.22k–1.29k and 91–103). At 1 MB Ferrix was ahead in two of three runs (7.6k, 12.4k, 12.1k vs. nginx's 7.1k–7.5k). Memory does not grow with file size in either: while serving the 10 MB file to 256 connections Ferrix held 7.9 MB and nginx 13.7 MB.

**TLS** (Ferrix vs. nginx only; same 4 cores, `wrk` with 256 connections, TLS 1.2 with ECDHE-ECDSA-AES256-GCM-SHA384 and a P-256 certificate, since the `wrk` image's TLS library has no TLS 1.3; mean of 3 runs on a busy machine):

| Scenario | Ferrix | nginx 1.27 |
| --- | --- | --- |
| 1.4 KB file, keep-alive | **91.8k req/s** | 69.6k req/s |
| 133 KB file, keep-alive | **16.2k req/s** | 9.8k req/s |
| Proxy, keep-alive | 56.3k req/s | 54.3k req/s |
| Full handshake per request (`Connection: close`) | 6.7k–11.1k/s | 5.4k–8.9k/s |
| Memory (PSS) after all of the above | **9.7 MB** | 14.8 MB |

**CPU:** under these loads every server saturates its 4 cores, so CPU per request is the inverse of the req/s above (1.4 KB file: ~24 µs for Ferrix, ~39 µs for nginx). Idle, with no connections, Ferrix uses about 1 ms of CPU every 10 s, the same as nginx for practical purposes.

**A caveat on these numbers:** the load generator shares the machine with the servers. When the desktop is busy, `wrk` itself gets preempted (its threads spent 0.7–4.3 s of a 6.5 s window waiting for a CPU, against 7–117 ms for the server under test), and that shows up as a higher p99 and lower req/s, more so against the faster server because `wrk` needs ~45% more CPU to keep up with it. Treat run-to-run differences of a few percent as noise.

**Where we stand:** Ferrix serves small static files ~1.6× faster than nginx, proxies ~14% more requests per second with a lower p99, and is level with it on large files, from 133 KB (nginx ~3% ahead) to 100 MB (zero-copy `sendfile` + `TCP_CORK`, like nginx's `tcp_nopush`). Over TLS it serves small files ~30% faster and 133 KB files ~65% faster, and ties on proxying. Among the Rust and Go alternatives it is the fastest in every scenario and uses the least memory under load. On memory it uses about a third of nginx idle (3.6 vs. 10.5 MB PSS) and a little over half after any of the loads (~6 vs. ~10.7 MB).

**What made it faster** (proxy went from 18.8k to 80k req/s, small files from 14.7k to 176k):

- **Thread-per-core workers:** each worker thread runs its own single-threaded runtime and its own `SO_REUSEPORT` socket, like nginx workers, so a request never hops between threads.
- **Own HTTP/1.1 server for plain connections:** bodiless requests are parsed and answered directly, large static files go out with `sendfile(2)` under `TCP_CORK`, and anything unusual (request bodies, upgrades, HTTP/2) is handed to hyper with the bytes already read.
- **Coarse per-worker timers and a coarse clock**, like nginx's timer tree and `ngx_time_update`: the clock is the kernel's tick-resolution one (read from the vDSO, no syscall per request, which matters on hosts without a TSC), and each worker sleeps until its next deadline instead of ticking, so an idle server uses no CPU (it used to burn ~6% of a core on wakeups).
- **Built-in HTTP/1.1 upstream client with a per-thread connection pool** (like Pingora): no channels or extra tasks per request, no locks, and response headers parsed without copying values.
- **No shared hot spots:** metrics counters, rate-limit buckets and request IDs are sharded per thread, and per-route headers are compiled when the config loads.
- **LTO + mimalloc** release builds, with mimalloc committing memory on demand, returning freed memory immediately, with transparent huge pages off, and each worker handing its spare heap pages back after a second without traffic; `panic = "abort"` (a master process, or systemd/Docker, restarts a crashed server, like nginx's master).
- **Raw proxy path:** on a proxy route with nothing else configured (no auth, limits, cache, compression, CORS or header rules, and metrics off), plain HTTP/1.1 requests are re-encoded byte for byte to the upstream and back, without building request/response objects in between. It turns on by itself; adding any of those features to the route simply uses the full pipeline.
- **Less resident code:** no web framework router in the admin API, no `reqwest`, `clap`, full `regex` engine or `tracing-subscriber`; optional modules (`http2`, `http3`, `brotli`, `zstd`, `acme`, `admin`, `nginx-import`) can be left out at build time like nginx's `--with-*` options.
- **No helper threads on the request path:** static files are read and checked inline like nginx does (they're small and in the page cache), so a cold start doesn't spawn blocking threads that each keep an allocator heap; rare, large request paths (TLS handshakes, static files, cache fills, WebSocket upgrades) are boxed so an in-flight proxied request holds ~5 KB instead of ~12 KB.
- **Small idle footprint:** an idle keep-alive connection costs ~3.8 KB, measured with 2,000 of them open (request state is boxed only while a request runs, the read buffer is only allocated once data arrives and the output buffer is shared per worker, like nginx's keep-alive handling; nginx itself is at ~0.7 KB), metrics storage is only allocated when metrics are on, and file checks are plain `stat` calls instead of a growing blocking-thread pool.
- **Startup code leaves memory:** once the listeners are up, the read-only pages of the executable are dropped (`MADV_DONTNEED`), so config parsing and other one-time code stop counting; the request path faults back only what it uses (~4 MB less).
- **In-memory static file cache** (files up to 1 MB, revalidated against the disk every second) and a **coarse clock** that costs nothing while idle, in the spirit of nginx's `ngx_time_update`.

Performance contributions are very welcome!

---

## 🤝 How you can help

Ferrix is a community project: you are free to use it, fork it, change it and send improvements back. No contribution is too small.

**Good places to start:**

- 🦀 **Rust core** — close the gap with nginx on proxy throughput and large files (`sendfile`/zero-copy), add missing nginx directives, write tests.
- 🔄 **nginx importer** — try `core/src/nginx` with your real `nginx.conf` files and report what doesn't convert.
- ⚛️ **Dashboard** — UX improvements, new charts, accessibility (`frontend/`).
- 📚 **Docs** — the docs in [`docs/`](docs/README.md) are in Portuguese; English translations are very welcome.
- 🐛 **Testing** — run it, break it, and [open an issue](https://github.com/arthurlunkes/ferrix/issues/new/choose).

Look for issues labeled [`good first issue`](https://github.com/arthurlunkes/ferrix/issues?q=is%3Aissue+is%3Aopen+label%3A%22good+first+issue%22) or [`help wanted`](https://github.com/arthurlunkes/ferrix/issues?q=is%3Aissue+is%3Aopen+label%3A%22help+wanted%22). Read [CONTRIBUTING.md](CONTRIBUTING.md) for the setup and PR workflow, and please follow the [Code of Conduct](CODE_OF_CONDUCT.md). Found a vulnerability? See [SECURITY.md](SECURITY.md).

If you like the project, a ⭐ on GitHub helps others find it!

## 📄 License

Distributed under the Apache-2.0 License. See [LICENSE](LICENSE).
