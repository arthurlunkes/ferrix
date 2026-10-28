# 02. Installation and Execution

This guide explains how to compile, run, and operate **Ferrix** in both development and production environments.

---

## 📋 Prerequisites

- **Operating System**: Linux, macOS, or Windows.
- **Rust Toolchain**: Stable release (1.75 or later).
  ```bash
  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
  ```
- **Node.js & npm** (only required if modifying or rebuilding the React dashboard):
  - Node.js 18+ (Node.js 20+ recommended).

---

## 🔨 Building the Project

Ferrix includes the compiled React dashboard assets (HTML, CSS, JavaScript) directly into the final executable. This allows you to distribute **a single self-contained binary** with no runtime dependencies.

### Step 1: Build the React Frontend

```bash
cd frontend
npm install
npm run build
cd ..
```

This compiles static assets into `frontend/dist/`.

### Step 2: Build the Core Proxy in Rust

For development:

```bash
cargo build
```

Binary location: `target/debug/ferrix`.

For production (maximum optimizations and performance):

```bash
cargo build --release
```

Optimized binary location: `target/release/ferrix`.

---

### Optional modules (smaller builds)

Like nginx's `--with-*` build options, some modules can be left out at compile time.
All of them are built by default:

| Feature | What it adds |
| --- | --- |
| `http2` | HTTP/2 for clients (over TLS via ALPN, and h2c) |
| `http3` | HTTP/3 over QUIC on TLS ports (served when `http.protocols` lists `http3`) |
| `brotli` | Brotli response compression (gzip is always available) |
| `zstd` | zstd response compression |
| `acme` | Automatic Let's Encrypt certificates (`tls.acme`) |
| `admin` | Admin REST API, live request stream and the web dashboard (`admin:`) |
| `nginx-import` | `ferrix import` and the dashboard's "Import nginx" page |

```bash
# Everything (default)
cargo build --release
# Minimal: HTTP/1.1, gzip, certificates from files, configured only through
# ferrix.yaml + `ferrix reload` (no admin API).
cargo build --release --no-default-features
# Pick and choose
cargo build --release --no-default-features --features "http2,brotli"
```

A configuration that needs a missing module (for example `tls.acme: true` without
`acme`) is rejected by `ferrix -t` with an explanation; a client asking for an encoding
that isn't built in simply gets one that is.

## 📦 Installing on a Linux machine (systemd)

### From source

```bash
git clone https://github.com/arthurlunkes/ferrix && cd ferrix
./core/scripts/install.sh            # or --core-only to skip the web dashboard
sudo systemctl enable --now ferrix
```

The script builds Ferrix, installs `/usr/local/bin/ferrix`, writes `/etc/ferrix/ferrix.yaml` (every option listed, commented out) and the `ferrix.service` unit, which may bind ports 80 and 443 without running as root.

### From a release binary

```bash
VERSION=v0.1.0                          # see github.com/arthurlunkes/ferrix/releases
ARCH=x86_64                             # or aarch64
wget https://github.com/arthurlunkes/ferrix/releases/download/$VERSION/ferrix-$VERSION-$ARCH-unknown-linux-gnu.tar.gz
tar xzf ferrix-$VERSION-$ARCH-unknown-linux-gnu.tar.gz
sudo install -m 755 ferrix-$VERSION-$ARCH-unknown-linux-gnu/ferrix /usr/local/bin/
sudo setcap cap_net_bind_service=+ep /usr/local/bin/ferrix   # allow ports 80/443
ferrix run --config /etc/ferrix/ferrix.yaml
```

### Checking that it works

```bash
curl -I http://localhost/            # "Server: ferrix/..." and the welcome page
systemctl status ferrix
journalctl -u ferrix -f              # logs
ferrix test                          # validate the configuration
```

### Updating an existing install

An existing `/etc/ferrix/ferrix.yaml` is never overwritten. To start from the new documented template, move the old file away and restart: Ferrix writes a fresh one on start.

---

## 🚀 Execution

### 1. Basic Startup

```bash
./target/release/ferrix run
```

By default:
- The configuration is always `/etc/ferrix/ferrix.yaml` (like nginx's `/etc/nginx/nginx.conf`), whatever directory you run `ferrix` from; `--config <file>` uses another one. If the file does not exist, Ferrix writes the documented default there, with a random admin token. Every command (`run`, `reload`, `stop`, `status`, `test`) uses the same file, so they always find the running server.
- The control plane API and web dashboard listen at: `http://127.0.0.1:8081`.
- The proxy listens on `http://0.0.0.0:80` (and on 443 for hosts with `tls:`), like nginx. Ports below 1024 need root or `CAP_NET_BIND_SERVICE`; the systemd unit and the packages grant it. To run by hand as a normal user, either allow the binary once (`sudo setcap cap_net_bind_service=+ep $(which ferrix)`) or use another port: `ferrix run --http-listen 0.0.0.0:8080`.
- Open `http://localhost/` and you should see the **Ferrix is running** page.
- The generated `ferrix.yaml` lists every option, commented out: uncomment what you need.

### 2. CLI Commands & Options

Ferrix provides an intuitive command line interface:

```text
⚡ Ferrix — High-Performance Standalone Reverse Proxy & Web Server

Usage: ferrix [COMMAND] [OPTIONS]

Main commands:
  run       Start the server in foreground
  start     Start the server in background (daemon)
  stop      Stop the background server
  restart   Restart the background server
  reload    Reload configuration without dropping active connections
  status    Check if the server is currently running
  test      Test configuration file syntax (shortcut: ferrix -t)
  import    Import and convert existing nginx.conf configurations

Global options:
  -c, --config <FILE>      Configuration file path [default: /etc/ferrix/ferrix.yaml]
  -t, --test               Test configuration file syntax and exit
  -s, --signal <SIGNAL>    Send signal to Ferrix (stop, reload, restart, status)
  -h, --help               Display all detailed help options
  -V, --version            Display installed Ferrix version
```

#### Examples:

Run with a custom configuration file:

```bash
./target/release/ferrix run --config /etc/ferrix/production.json
```

Start Ferrix as a background daemon:

```bash
./target/release/ferrix start
```

Check runtime status and health:

```bash
./target/release/ferrix status
```

Test syntax without applying:

```bash
./target/release/ferrix -t
```

---

### Master process (automatic restarts)

Like nginx, `ferrix run` keeps a small **master process** that starts the actual server
as a child and restarts it within ~100 ms if it ever dies (with a growing delay if it
keeps failing). `ferrix reload` and `ferrix stop` signal the master, which forwards
them. Configuration errors are not retried: the server exits and so does the master.

Release builds use `panic = "abort"`: a bug that panics ends the server process instead
of unwinding, which saves ~1 MB of resident code; the master brings it back.

The master is a second process (~3–4 MB). Where something else already restarts Ferrix,
it's turned off with `FERRIX_SUPERVISOR=0`, and that's the default in the places Ferrix
ships with one: the Docker images (start them with `--restart unless-stopped`, as the
`docker-compose.yml` does) and the systemd unit from `install.sh` (`Restart=on-failure`).

## 🧪 Testing with Mock Backends

To experiment with load balancing, you can run the provided `test_backends.js` script:

```bash
# In terminal 1:
node test_backends.js

# Expected output:
# [Backend-1] listening on http://127.0.0.1:9001
# [Backend-2] listening on http://127.0.0.1:9002
```

In terminal 2, start Ferrix:

```bash
./target/release/ferrix run --config ferrix.yaml
```

Send requests to `http://localhost/api` and verify:
1. Responses alternating between `Backend-1` and `Backend-2`.
2. Live metrics and request logs appearing in real time on the web dashboard at `http://localhost:8081`.

---

## 🐳 Running with Docker (GitHub Packages)

Official multi-architecture images (`linux/amd64` and `linux/arm64`) are published to the GitHub Container Registry. You can choose between the **ultra-lean Core image** (~20 MB) or the **Full All-in-One image** with the Web Dashboard:

### Option A: Ultra-Lean Core (~20 MB, Rust Proxy Only)

Ideal for production, Kubernetes, and minimal resource footprints:

```bash
docker run -d \
  --name ferrix \
  -p 80:80 \
  -p 8081:8081 \
  -v $(pwd)/ferrix.yaml:/etc/ferrix/ferrix.yaml \
  ghcr.io/arthurlunkes/ferrix:latest
```

### Option B: Full, with the web dashboard on :8080

One Ferrix process serves the proxy (:80) and the admin API plus dashboard (:8080); the image has no Node.js. The first start writes `ferrix.yaml` into the mounted directory with a random admin token, shown in the log:

```bash
docker run -d \
  --name ferrix-full \
  -p 80:80 \
  -p 8080:8080 \
  -v $(pwd)/ferrix-data:/etc/ferrix \
  --restart unless-stopped \
  ghcr.io/arthurlunkes/ferrix:full
docker logs ferrix-full
```

### Using Docker Compose

```yaml
services:
  ferrix-core:
    image: ghcr.io/arthurlunkes/ferrix:latest
    ports:
      - "80:80"
      - "8081:8081"
    volumes:
      - ./ferrix.yaml:/etc/ferrix/ferrix.yaml
```

Run with:

```bash
docker compose up -d ferrix-core
```
