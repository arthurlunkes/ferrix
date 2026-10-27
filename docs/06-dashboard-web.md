# 06. Web Dashboard Guide (React UI)

**Ferrix** comes equipped with a comprehensive graphical dashboard built with **React 19**, **Vite**, and styled with **Tailwind CSS** in a modern dark theme.

The frontend is compiled and directly embedded inside the single Rust binary executable, meaning you do not need Node.js installed in production environments to run the web UI.

Access the dashboard by navigating to:
👉 **[http://localhost:8080](http://localhost:8080)**

---

## 🧭 Top Navigation Bar (Navbar)

At the top of every view, the navigation bar displays:

- **Logo and Version**: Visual Ferrix branding and current running binary version.
- **Navigation Tabs**:
  - `Dashboard`: High-level operational overview and telemetry graphs.
  - `Virtual Hosts`: Manage site configs, domains, and route rules.
  - `Live Requests`: Live streaming and inspection of HTTP/WebSocket traffic.
  - `Upstreams`: Backend pool health checks and load balancing.
  - `Config Editor`: JSON configuration editor with validation and instant hot-reload.
  - `Import Nginx`: Convert Nginx `.conf` files directly into Ferrix configurations.
- **SSE Connection Status**:
  - A pulsing green indicator confirms that Server-Sent Events are active and streaming real-time metrics.
- **Uptime & Ports**:
  - Displays the active proxy port and process uptime.

---

## 📈 1. Dashboard View

The primary dashboard provides immediate operational visibility into proxy infrastructure:

1. **KPI Metric Cards**:
   - **Total Requests**: Cumulative requests served and active concurrent open connections.
   - **Throughput (RPS)**: Rolling requests per second.
   - **Avg Latency**: Average response time in milliseconds along with p95 latency.
   - **Error Rate**: Percentage of 4xx/5xx responses relative to total volume.
2. **Real-time Area Chart**:
   - Renders second-by-second throughput curves (RPS in green) and response latencies (in blue).
3. **HTTP Status Distribution**:
   - Donut chart categorizing responses into 2xx (Success), 3xx (Redirect), 4xx (Client Error), and 5xx (Server Error).
4. **Recent Activity Feed**:
   - Compact table listing the last 6 completed requests.

---

## ⚡ 2. Live Requests (Real-Time Inspection)

The Live Requests screen provides deep observability and debugging capabilities for incoming traffic:

- **Instant Event Stream**: Every request handled by Ferrix streams live into the view without reloading.
- **Status Code Filters**:
  - Quick filters for `2xx` (Success), `3xx` (Redirects), `4xx` (Client errors), or `5xx` (Server errors).
- **HTTP Method Filters**:
  - Filter by `GET`, `POST`, `PUT`, `DELETE`, or `WS` (WebSocket).
- **Live Search**:
  - Search by request path, client IP, host, or upstream backend.
- **Stream Controls**:
  - **"Pause Stream"**: Freezes scrolling to inspect a specific request without incoming events displacing the table.
  - **"Resume Stream"**: Resumes real-time streaming.
  - **"Clear"**: Flushes the local view history.

### 🔍 Request Details Modal

Clicking on any row opens a comprehensive inspection modal displaying:

- HTTP status code and method;
- Exact response time in milliseconds;
- Transferred payload size (bytes);
- Connection type (HTTP/1.1 or WebSocket);
- Client IP address and `Host` header;
- Upstream target address that processed the request;
- Full `User-Agent` string;
- Detailed error messages (for responses >= 400).

---

## 🌐 3. Virtual Hosts View

Manage domains and routing topologies through an intuitive graphical editor:

- **Site List**: View all configured domains, listen ports, and route definitions.
- **Create / Edit Virtual Host**:
  - Configure hostnames (e.g. `app.mydomain.com`, `api.mydomain.com`, or wildcard `*`).
  - Attach multiple routes with prefix patterns (e.g. `/`, `/api`, `/assets`).
  - Select route target types:
    - **Proxy Upstream**: Select load-balanced backend pools and toggle WebSocket tunneling.
    - **Static Files**: Serve local disk folders and configure `try_files` SPA fallbacks.
    - **Redirect**: Set up permanent (301) or temporary (302) redirections.
  - Changes take effect immediately via zero-downtime **Hot-Reload**.

---

## 🖥️ 4. Upstreams View

Monitor availability and performance across backend server targets:

- **Upstream Pools Overview**: Inspect cluster nodes, healthy counts, and active balancing algorithms.
- **Per-Target Health Monitoring**:
  - Visual indicators for healthy (**Healthy**) vs failing (**Unhealthy**) nodes.
  - Response latency measured by background health checks in milliseconds.
  - Consecutive failure counters and configured error thresholds.
- **Health Check Configuration**:
  - Configure probe endpoints (e.g. `/health`), probe intervals, and expected HTTP status codes.

---

## 📝 5. Config Editor View

For engineers who prefer editing configurations in code:

- **Syntax-Highlighted Editor**: Full `ferrix.yaml` with line numbers and linting.
- **"Validate & Hot-Reload"**: Validates JSON syntax and atomically updates the running server.
- **Export & Import**:
  - Download active configuration backups (`Export JSON`).
  - Upload previously saved configurations (`Import JSON`).
- **"Reset"**: Reverts uncommitted changes back to current active memory state.
