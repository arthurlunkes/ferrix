# 05. REST API Reference

Ferrix exposes a modern REST API on its administrative port (`admin.listen`, default `127.0.0.1:8081`; `8080` in the Docker "full" image) for telemetry metrics, historical request audits, real-time logs, and dynamic zero-downtime configuration management.

---

## 📡 Endpoints Summary

| Method   | Path                   | Description                                                |
| -------- | ---------------------- | ---------------------------------------------------------- |
| `GET`    | `/api/status`          | Health, process info, version, and uptime                  |
| `GET`    | `/api/metrics`         | Real-time telemetry snapshot (RPS, latencies, counters)    |
| `GET`    | `/api/logs`            | Historical request logs with filters and pagination        |
| `GET`    | `/api/logs/stream`     | Live event stream via Server-Sent Events (SSE)             |
| `GET`    | `/api/config`          | Returns active global configuration                        |
| `POST`   | `/api/config`          | Applies new configuration with zero-downtime hot-reload    |
| `GET`    | `/api/config/yaml`     | Active configuration as YAML text (what `ferrix.yaml` holds) |
| `POST`   | `/api/config/yaml`     | Applies YAML text: parsed, validated, saved, hot-reloaded  |
| `POST`   | `/api/config/yaml/validate` | Validates YAML text without applying it               |
| `GET`    | `/api/vhosts`          | Lists all Virtual Hosts                                    |
| `POST`   | `/api/vhosts`          | Creates or updates a Virtual Host                          |
| `DELETE` | `/api/vhosts/:id`      | Deletes a Virtual Host                                     |
| `GET`    | `/api/upstreams`       | Lists all Upstreams and health status of targets           |
| `POST`   | `/api/upstreams`       | Creates or updates an Upstream pool                        |
| `DELETE` | `/api/upstreams/:name` | Removes an Upstream pool                                   |

---

## 1. Server Status

### `GET /api/status`

Returns process metadata, configuration summary, and active uptime.

**Example Response:**

```json
{
  "name": "Ferrix",
  "version": "0.1.0",
  "uptime_seconds": 1245,
  "start_time_utc": "2026-09-08T16:38:54.332504764Z",
  "admin_listen": "127.0.0.1:8081",
  "proxy_listen": "0.0.0.0:80",
  "total_vhosts": 2,
  "total_upstreams": 1
}
```

---

## 2. Telemetry & Metrics

### `GET /api/metrics`

Returns throughput, active connection metrics, status code aggregates, and latency percentiles.

**Example Response:**

```json
{
  "uptime_seconds": 1245,
  "total_requests": 84210,
  "active_connections": 14,
  "requests_per_second": 320.5,
  "status_2xx": 81500,
  "status_3xx": 2100,
  "status_4xx": 580,
  "status_5xx": 30,
  "total_bytes_sent": 584920194,
  "avg_latency_ms": 1.45,
  "p95_latency_ms": 3.2,
  "p99_latency_ms": 6.8
}
```

---

## 3. Request Logs

### `GET /api/logs`

Query the recent ring-buffer of processed requests.

**Query Parameters:**

- `limit` (optional, default 100, max 500): Number of entries to return.
- `status` (optional): Exact status code or range prefix (e.g., `200` or `400` for 4xx errors).
- `q` (optional): Text search matching URI, Client IP, or Host header.

**Example Request:**

```bash
curl "http://localhost:8081/api/logs?limit=10&status=400&q=api"
```

**Example Response:**

```json
[
  {
    "id": "601fa7db-3b13-4709-b4d4-1a202426a4bf",
    "timestamp": "2026-09-08T17:12:16.230268210Z",
    "client_ip": "192.168.1.50",
    "method": "POST",
    "uri": "/api/users",
    "host": "mydomain.com",
    "status": 200,
    "latency_ms": 2.14,
    "bytes_sent": 420,
    "upstream": "http://127.0.0.1:9001",
    "user_agent": "Mozilla/5.0 ...",
    "is_websocket": false,
    "error": null
  }
]
```

### `GET /api/logs/stream` (SSE)

Real-time push channel streaming `request` events whenever a proxy transaction finishes.

**JavaScript EventSource Example:**

```javascript
const es = new EventSource("http://localhost:8081/api/logs/stream");
es.addEventListener("request", (event) => {
  const log = JSON.parse(event.data);
  console.log(
    `[${log.method}] ${log.uri} -> ${log.status} (${log.latency_ms}ms)`,
  );
});
```

---

## 4. Configuration Management & Hot-Reload

### `GET` / `POST /api/config/yaml`

The same configuration as `/api/config`, but as the YAML text stored in `ferrix.yaml` (`Content-Type: application/yaml`). The dashboard's config editor uses these. `POST` answers with the same JSON result as `POST /api/config`, or `400` with the YAML error.

```bash
curl -H "Authorization: Bearer $TOKEN" http://localhost:8081/api/config/yaml > ferrix.yaml
# edit ferrix.yaml, then:
curl -X POST -H "Authorization: Bearer $TOKEN" -H "Content-Type: application/yaml" \
     --data-binary @ferrix.yaml http://localhost:8081/api/config/yaml
```

### `GET /api/config`

Retrieves the currently active JSON configuration.

### `POST /api/config`

Overwrites global configuration, persists to disk, and executes atomic hot-reload.

**curl Example:**

```bash
curl -X POST http://localhost:8081/api/config \
  -H "Content-Type: application/json" \
  -d @new_config.json
```

---

## 5. Virtual Hosts CRUD

### `POST /api/vhosts`

Creates or updates a Virtual Host. If `id` already exists, it is updated in place; otherwise, a new host is appended.

```bash
curl -X POST http://localhost:8081/api/vhosts \
  -H "Content-Type: application/json" \
  -d '{
    "id": "new-vhost",
    "name": "Auth Gateway Service",
    "server_names": ["auth.mydomain.com"],
    "listen_port": 80,
    "headers": {
      "X-Service": "Auth-Gateway"
    },
    "allowed_ips": [],
    "denied_ips": [],
    "routes": [
      {
        "id": "auth-route",
        "path_prefix": "/",
        "strip_prefix": false,
        "target_type": "proxy_upstream",
        "upstream_name": "default_backend",
        "websocket_enabled": true,
        "proxy_set_headers": { "Host": "$host" },
        "add_headers": {}
      }
    ]
  }'
```

### `DELETE /api/vhosts/:id`

Deletes a Virtual Host and reloads routing tables.

```bash
curl -X DELETE http://localhost:8081/api/vhosts/new-vhost
```
