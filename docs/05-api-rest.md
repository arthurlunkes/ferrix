# 05. Referência da API REST

O Ferrix expõe uma API REST moderna na porta administrativa (padrão `8080`) para consulta de métricas, histórico de requisições, logs ao vivo e gestão dinâmica de configurações.

---

## 📡 Resumo dos Endpoints

| Método   | Caminho                | Descrição                                                       |
| -------- | ---------------------- | --------------------------------------------------------------- |
| `GET`    | `/api/status`          | Informações de integridade, versão e uptime                     |
| `GET`    | `/api/metrics`         | Snapshot de métricas em tempo real (RPS, latências, contadores) |
| `GET`    | `/api/logs`            | Consulta histórica de logs com filtros e paginação              |
| `GET`    | `/api/logs/stream`     | Stream ao vivo via Server-Sent Events (SSE)                     |
| `GET`    | `/api/config`          | Retorna a configuração global ativa                             |
| `POST`   | `/api/config`          | Aplica uma nova configuração global com hot-reload              |
| `GET`    | `/api/vhosts`          | Lista todos os Virtual Hosts                                    |
| `POST`   | `/api/vhosts`          | Cria ou atualiza um Virtual Host                                |
| `DELETE` | `/api/vhosts/:id`      | Remove um Virtual Host                                          |
| `GET`    | `/api/upstreams`       | Lista todos os Upstreams e status dos nós                       |
| `POST`   | `/api/upstreams`       | Cria ou atualiza um pool de Upstream                            |
| `DELETE` | `/api/upstreams/:name` | Remove um pool de Upstream                                      |

---

## 1. Status do Servidor

### `GET /api/status`

Retorna dados gerais de operação e uptime do processo.

**Exemplo de Resposta:**

```json
{
  "name": "Ferrix",
  "version": "0.1.0",
  "uptime_seconds": 1245,
  "start_time_utc": "2026-09-08T16:38:54.332504764Z",
  "admin_listen": "127.0.0.1:8081",
  "proxy_listen": "0.0.0.0:8000",
  "total_vhosts": 2,
  "total_upstreams": 1
}
```

---

## 2. Métricas de Desempenho

### `GET /api/metrics`

Retorna estatísticas acumuladas e instantâneas de vazão e latência.

**Exemplo de Resposta:**

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

## 3. Logs de Requisições

### `GET /api/logs`

Permite filtrar o buffer recente de requisições.

**Parâmetros de Query:**

- `limit` (opcional, padrão 100, máx 500): Quantidade de itens retornados.
- `status` (opcional): Código exato ou faixa (ex: `200` ou `400` para 4xx).
- `q` (opcional): Termo de busca textual em URL, IP ou Host.

**Exemplo de Chamada:**

```bash
curl "http://localhost:8080/api/logs?limit=10&status=400&q=api"
```

**Exemplo de Resposta:**

```json
[
  {
    "id": "601fa7db-3b13-4709-b4d4-1a202426a4bf",
    "timestamp": "2026-09-08T17:12:16.230268210Z",
    "client_ip": "192.168.1.50",
    "method": "POST",
    "uri": "/api/users",
    "host": "meudominio.com",
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

Canal em tempo real que emite eventos do tipo `request` sempre que uma requisição é concluída pelo proxy.

**Exemplo de Consumo via JavaScript:**

```javascript
const es = new EventSource("http://localhost:8080/api/logs/stream");
es.addEventListener("request", (event) => {
  const log = JSON.parse(event.data);
  console.log(
    `[${log.method}] ${log.uri} -> ${log.status} (${log.latency_ms}ms)`,
  );
});
```

---

## 4. Gestão de Configuração e Hot-Reload

### `GET /api/config`

Retorna a configuração completa ativa no formato JSON.

### `POST /api/config`

Substitui a configuração global, salva em disco e aplica o hot-reload imediatamente.

**Exemplo via curl:**

```bash
curl -X POST http://localhost:8080/api/config \
  -H "Content-Type: application/json" \
  -d @nova_config.json
```

---

## 5. CRUD de Virtual Hosts

### `POST /api/vhosts`

Cria ou atualiza um virtual host. Se o `id` já existir, ele é atualizado; caso contrário, é adicionado.

```bash
curl -X POST http://localhost:8080/api/vhosts \
  -H "Content-Type: application/json" \
  -d '{
    "id": "vhost-novo",
    "name": "Serviço de Autenticação",
    "server_names": ["auth.meudominio.com"],
    "listen_port": 8000,
    "headers": {
      "X-Service": "Auth-Gateway"
    },
    "allowed_ips": [],
    "denied_ips": [],
    "routes": [
      {
        "id": "rota-auth",
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

Remove um host virtual e recarrega a tabela de roteamento.

```bash
curl -X DELETE http://localhost:8080/api/vhosts/vhost-novo
```
