# 04. Configuration & Features Guide

Ferrix's configuration file is stored in **JSON** format (defaulting to `ferrix.json`), enabling straightforward manipulation visually, via scripts, or with infrastructure-as-code tools.

---

## 📄 General Structure of `ferrix.json`

```json
{
  "admin": {
    "listen": "127.0.0.1:8081"
  },
  "http": {
    "listen": "0.0.0.0:8000",
    "worker_threads": 4
  },
  "upstreams": { ... },
  "vhosts": [ ... ]
}
```

---

## 🌐 Virtual Hosts (Sites)

Each Virtual Host represents a server block (equivalent to an Nginx `server { ... }` block).

```json
{
  "id": "main-vhost",
  "name": "My Web Application",
  "server_names": ["mydomain.com", "www.mydomain.com", "localhost"],
  "listen_port": 8000,
  "headers": {
    "X-Powered-By": "Ferrix/0.1.0",
    "X-Frame-Options": "SAMEORIGIN",
    "X-Content-Type-Options": "nosniff"
  },
  "allowed_ips": [],
  "denied_ips": ["192.168.1.100"],
  "basic_auth": {
    "username": "admin",
    "password": "secure_password_here"
  },
  "rate_limit": {
    "requests_per_second": 50,
    "burst": 100
  },
  "routes": [ ... ]
}
```

### Virtual Host Properties:

- `id`: Unique identifier (used in REST API URLs).
- `name`: Friendly human-readable name displayed on the dashboard.
- `server_names`: List of hostnames or IPs accepted in the `Host` header. Supports `*` as a catch-all for all domains.
- `listen_port`: Inbound traffic listening port.
- `headers`: HTTP headers injected into all responses served by this virtual host.
- `allowed_ips`: List of authorized IPs. If empty, all IPs are allowed (except explicitly denied ones).
- `denied_ips`: Blocked IP addresses (returns `403 Forbidden`).
- `basic_auth` (optional): Protects all routes with HTTP Basic Authentication.
- `rate_limit` (optional): Global host rate limit applied per client IP.

---

## 🛣️ Routing Rules (Routes)

Inside each Virtual Host, you configure path routing rules (`routes`), equivalent to Nginx `location` blocks:

### 1. Reverse Proxy Route

Forwards traffic to a backend server pool:

```json
{
  "id": "api-route",
  "path_prefix": "/api",
  "strip_prefix": false,
  "target_type": "proxy_upstream",
  "upstream_name": "my_backend_cluster",
  "websocket_enabled": true,
  "proxy_set_headers": {
    "Host": "$host",
    "X-Real-IP": "$remote_addr",
    "X-Forwarded-For": "$proxy_add_x_forwarded_for",
    "X-Forwarded-Proto": "$scheme"
  },
  "add_headers": {
    "Access-Control-Allow-Origin": "*"
  }
}
```

- `strip_prefix`: If `true`, strips `/api` from the URI path before forwarding to the backend.
- `websocket_enabled`: Enables long-lived bidirectional WebSocket tunneling.
- `proxy_set_headers`: Custom upstream headers with special variable substitution (`$host`, `$remote_addr`, `$proxy_add_x_forwarded_for`, `$scheme`).

### 2. Static Files Route with SPA Fallback

Serves local disk files and routes missing paths to `index.html`:

```json
{
  "id": "frontend-route",
  "path_prefix": "/",
  "strip_prefix": false,
  "target_type": "static_files",
  "static_root": "./frontend/dist",
  "try_files": "$uri $uri/ /index.html",
  "websocket_enabled": false
}
```

- `static_root`: Local disk path where files are hosted.
- `try_files`: Checks for the exact file (`$uri`), then directory index (`$uri/`), falling back to (`/index.html`). Tailor-made for React Router, Vue Router, etc.

### 3. HTTP Redirect Route

Redirects the browser to an alternate URL:

```json
{
  "id": "docs-route",
  "path_prefix": "/docs",
  "strip_prefix": false,
  "target_type": "redirect",
  "redirect_url": "https://github.com/my-project",
  "redirect_code": 301
}
```

---

## ⚖️ Upstreams and Load Balancing

Upstreams define pools of backend servers that share incoming workload.

```json
{
  "upstreams": {
    "my_backend_cluster": {
      "name": "my_backend_cluster",
      "algorithm": "round_robin",
      "targets": [
        {
          "url": "http://10.0.0.1:3000",
          "weight": 3,
          "max_fails": 3,
          "fail_timeout_secs": 10
        },
        {
          "url": "http://10.0.0.2:3000",
          "weight": 1,
          "max_fails": 3,
          "fail_timeout_secs": 10
        }
      ],
      "health_check": {
        "path": "/health",
        "interval_secs": 5,
        "timeout_secs": 2,
        "expected_status": 200
      }
    }
  }
}
```

### Supported Balancing Algorithms (`algorithm`):

1. **`round_robin`**: Cycles sequentially through healthy targets.
2. **`weighted_round_robin`**: Distributes proportionally to each server's configured `weight` (e.g., weight 3 receives 3x more requests than weight 1).
3. **`least_conn`**: Routes to the target with the fewest active concurrent connections at request time.
4. **`ip_hash`**: Hashes client IP to ensure continuous affinity with the same backend (sticky session).
5. **`random`**: Uniform random distribution among healthy nodes.

### Automatic Failover (_proxy_next_upstream_):

If a pool node fails (connection error, timeout, or HTTP 502/504 status code):

1. Ferrix increments the node's consecutive failure counter.
2. Immediately tries the next available target in the pool without dropping the client request.
3. Once consecutive failures reach `max_fails`, the node is temporarily marked **UNHEALTHY** until active background health checks confirm recovery.
