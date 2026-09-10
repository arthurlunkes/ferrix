# 03. Arquitetura Interna

O **Ferrix** foi desenhado segundo princípios modernos de sistemas distribuídos e servidores de rede assíncronos. Esta seção detalha os componentes internos e como eles colaboram para entregar alta vazão e zero downtime.

---

## 🏛️ Visão Estrutural

```
                          ┌────────────────────────────────────────────────────────┐
                          │                      FERRIX PROCESS                    │
                          │                                                        │
Incoming HTTP / WS  ─────►│  Proxy Listener (Port 8000 / HTTP Config)              │
                          │  ├── Client Address Extractor (ConnectInfo)            │
                          │  ├── Host Header Matcher (VirtualHost Finder)          │
                          │  ├── Middleware Pipeline (IP ACL, RateLimit, Auth)     │
                          │  ├── Route Rule Prefix Matcher                         │
                          │  │   ├── Target: Static File Server (Zero-Copy)        │
                          │  │   ├── Target: HTTP Redirect (301/302)               │
                          │  │   └── Target: Upstream Pool (Load Balancer)         │
                          │  │       ├── WS Upgrade: Tokio Bidirectional Tunnel   │
                          │  │       └── HTTP: Reqwest/Hyper Pool with Failover    │
                          │  │                                                     │
                          │  └── Telemetry Interceptor                             │
                          │      ├── Atomic Counters (2xx, 3xx, 4xx, 5xx, Bytes)   │
                          │      ├── Ring Buffer (Recent 1000 logs)                │
                          │      └── Broadcast Channel (tokio::sync::broadcast)    │
                          │                     │                                  │
                          │                     ▼                                  │
Browser / REST API  ─────►│  Admin Listener (Port 8080)                            │
                          │  ├── REST API Handlers (/api/status, /api/config, etc) │
                          │  ├── Server-Sent Events (SSE Stream)                   │
                          │  └── Embedded React SPA (rust-embed Assets)            │
                          └────────────────────────────────────────────────────────┘
```

---

## ⚡ Zero-Downtime Hot Reload via `ArcSwap`

No Nginx tradicional, para recarregar a configuração sem parar o servidor (`nginx -s reload`), o processo pai cria novos processos filhos e aguarda os antigos terminarem as conexões existentes (_graceful worker restart_).

No Ferrix, como o Rust possui primitivas atômicas de ponteiros seguros, usamos o crate **`arc-swap`** (`ArcSwap<FerrixConfig>`):

1. O estado de configuração do servidor é empacotado em um ponteiro atômico:
   ```rust
   pub shared_config: Arc<ArcSwap<FerrixConfig>>
   ```
2. Durante o processamento de tráfego, as threads de trabalho do Tokio fazem apenas um empréstimo rápido (`shared_config.load()`), que tem custo de apenas uma instrução atômica de CPU (sem locks e sem contenção de mutex).
3. Quando uma nova configuração é enviada pela interface web ou pela API REST:
   - A configuração é validada;
   - É gravada com segurança no arquivo `ferrix.json` no disco;
   - É armazenada no ponteiro atômico via `shared_config.store(Arc::new(nova_config))`.
4. Todas as novas conexões passam a enxergar imediatamente a nova tabela de rotas e backends, enquanto requisições que já estavam no meio do caminho completam normalmente.
5. **Resultado**: Zero downtime, zero perda de conexões e tempo de recarga menor que 1 milissegundo.

---

## 🔄 Túnel WebSocket Bidirecional Full-Duplex

Quando um cliente envia uma requisição com os headers `Upgrade: websocket` e `Connection: Upgrade`, o Ferrix detecta automaticamente a intenção de upgrade:

1. O handshake inicial é processado pelo Axum através de `WebSocketUpgrade::from_request_parts`.
2. Em paralelo, o Ferrix conecta-se assincronamente ao nó de backend selecionado pelo algoritmo de balanceamento via `tokio_tungstenite::connect_async`.
3. Ao completar a conexão com o backend, as duas pontas (Cliente e Servidor Upstream) são conectadas usando tarefas assíncronas concorrentes:
   - Tarefa A: lê frames do cliente e encaminha para o backend.
   - Tarefa B: lê frames do backend e devolve para o cliente.
4. Quando uma das pontas envia um frame de `Close`, o túnel é finalizado de forma limpa.

---

## 📊 Telemetria, Métricas e Server-Sent Events (SSE)

O subsistema de telemetria foi projetado para não causar gargalo no caminho crítico das requisições:

- **Contadores Atômicos**: `total_requests`, `active_connections`, `status_2xx`, `status_3xx`, `status_4xx`, `status_5xx` e `total_bytes_sent` usam `AtomicU64` com ordenação `Ordering::Relaxed` (sem travar threads).
- **Cálculo de Percentis de Latência (p50, p95, p99)**: Mantido em uma fila circular (`VecDeque<f64>`) com as últimas 1.000 requisições.
- **Transmissão em Tempo Real**: Cada requisição finalizada emite um `RequestLog` para um canal de broadcast assíncrono (`tokio::sync::broadcast::Sender`).
- **Endpoint SSE (`/api/logs/stream`)**: A interface web consome esse stream via `EventSource`, atualizando a tabela de requisições ao vivo sem precisar fazer polling repetitivo.

---

## 📁 Estrutura de Arquivos do Backend

| Arquivo                     | Responsabilidade                                                                           |
| --------------------------- | ------------------------------------------------------------------------------------------ |
| `src/main.rs`               | Inicialização do runtime Tokio, parsing de CLI e spawn dos listeners (Proxy e Admin).      |
| `src/config.rs`             | Estruturas de dados de configuração (VHost, Route, Upstream, etc.) e serialização JSON.    |
| `src/proxy/mod.rs`          | Engine principal de roteamento, correspondência de domínio, failover HTTP e orquestração.  |
| `src/proxy/upstream.rs`     | Gerenciamento de pools de upstream e implementação dos 5 algoritmos de balanceamento.      |
| `src/proxy/websocket.rs`    | Túnel bidirecional full-duplex de WebSocket.                                               |
| `src/proxy/static_files.rs` | Servidor de arquivos estáticos com detecção de MIME e diretiva `try_files`.                |
| `src/proxy/middleware.rs`   | Rate Limiter (Token Bucket), IP Access Control e HTTP Basic Auth.                          |
| `src/admin/mod.rs`          | Servidor HTTP administrativo, suporte a CORS e entrega de assets embutidos (`rust-embed`). |
| `src/admin/handlers.rs`     | Handlers REST da API de gestão (`/status`, `/metrics`, `/config`, etc.).                   |
| `src/admin/sse.rs`          | Adaptador do canal broadcast para Server-Sent Events (SSE).                                |
| `src/metrics/mod.rs`        | Coletor de métricas estatísticas e percentis de latência.                                  |
| `src/metrics/logger.rs`     | Estrutura de dados `RequestLog`.                                                           |
| `src/health/checker.rs`     | Background worker que pinga periodicamente os servidores upstream.                         |
