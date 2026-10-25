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

## 🚀 Execution

### 1. Basic Startup

```bash
./target/release/ferrix run
```

By default:
- If `ferrix.json` does not exist, Ferrix automatically generates a default initial configuration with a secure random token.
- The control plane API and web dashboard listen at: `http://127.0.0.1:8081`.
- The HTTP reverse proxy traffic listener binds to: `http://0.0.0.0:8000`.

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
  -c, --config <FILE>      Configuration file path [default: ferrix.json]
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
./target/release/ferrix run --config ferrix.json
```

Send requests to `http://localhost:8000/api` and verify:
1. Responses alternating between `Backend-1` and `Backend-2`.
2. Live metrics and request logs appearing in real time on the web dashboard at `http://localhost:8081`.

---

## 🐳 Docker Deployment (Optional)

Multi-stage `Dockerfile` example to produce a minimal container image:

```dockerfile
# Stage 1: Build React UI
FROM node:22-alpine AS ui-builder
WORKDIR /app/frontend
COPY frontend/package*.json ./
RUN npm install
COPY frontend/ ./
RUN npm run build

# Stage 2: Build Rust Binary
FROM rust:1.80-alpine AS rust-builder
RUN apk add --no-cache musl-dev
WORKDIR /app
COPY Cargo.* ./
COPY core/ core/
COPY --from=ui-builder /app/frontend/dist frontend/dist
RUN cargo build --release

# Stage 3: Runtime
FROM alpine:3.20
WORKDIR /app
COPY --from=rust-builder /app/target/release/ferrix /usr/local/bin/ferrix
COPY ferrix.json ./
EXPOSE 8000 8081
CMD ["ferrix", "run", "--config", "ferrix.json"]
```
