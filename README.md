# Ferrix — High-Performance Nginx Clone in Rust

**Ferrix** é um servidor web e reverse proxy assíncrono de alto desempenho construído em **Rust** (usando Tokio, Hyper e Axum), acompanhado de uma interface web administrativa moderna em **React + Tailwind CSS** e **API REST** para configuração dinâmica sem reinicialização (_zero-downtime hot-reload_), métricas em tempo real e inspeção de requisições ao vivo via Server-Sent Events (SSE).

---

## 🚀 Principais Funcionalidades

1. **Reverse Proxy com Suporte a WebSockets**:
   - Túnel assíncrono full-duplex transparente para conexões `Upgrade: websocket`.
   - Repasse de cabeçalhos de proxy (`Host`, `X-Real-IP`, `X-Forwarded-For`, `X-Forwarded-Proto`, cabeçalhos customizados).
   - Suporte a streaming de dados e Server-Sent Events sem bufferizar tudo em memória.

2. **Load Balancing & Alta Disponibilidade**:
   - **Algoritmos Suportados**:
     - `round_robin`: Distribuição cíclica uniforme.
     - `weighted_round_robin`: Distribuição proporcional por pesos definidos por servidor.
     - `least_conn`: Encaminhamento para o servidor com menor número de conexões ativas.
     - `ip_hash`: Sessão persistente baseada no IP de origem do cliente.
     - `random`: Seleção aleatória entre nós saudáveis.
   - **Health Checks Ativos & Passivos**:
     - Monitoramento periódico de saúde dos backends em background (path, intervalo e status esperado).
     - Failover automático (_proxy_next_upstream_): se um nó retornar 502/504 ou timeout, o Ferrix retenta instantaneamente no próximo servidor do pool.

3. **Arquivos Estáticos & Roteamento SPA**:
   - Entrega rápida de arquivos estáticos com detecção de MIME types via `mime_guess`.
   - Diretiva **`try_files`** (estilo Nginx) para suporte nativo a aplicações SPA (Single Page Applications como React, Vue, Angular).
   - Suporte a headers de cache (`Cache-Control`) e proteção contra directory traversal (`../`).

4. **Regras de Tráfego, Segurança & Headers**:
   - **Rate Limiting**: Algoritmo Token Bucket com suporte a taxa por segundo e burst por IP.
   - **Controle de Acesso**: Bloqueio e liberação de IPs (Allow/Deny).
   - **Autenticação Básica**: Proteção HTTP Basic Auth per route ou vhost.
   - **Redirecionamentos / Rewrites**: Suporte nativo a 301, 302, 307 e 308.

5. **Hot-Reload em Tempo Real (Zero Downtime)**:
   - Configurações alteradas pela Web UI ou pela API REST aplicam instantaneamente na memória usando ponteiros atômicos (`ArcSwap`), persistindo no arquivo `ferrix.json` no disco sem fechar sockets nem derrubar conexões ativas.

6. **Dashboard Web React & API de Telemetria**:
   - Painel de métricas com RPS, latência média, p95, p99 e distribuição de status HTTP (2xx, 3xx, 4xx, 5xx).
   - Tabela de inspeção de requests ao vivo via **SSE** com detalhes profundos de cada requisição.
   - Gestor visual de Virtual Hosts, Rotas e Pools de Upstream.
   - Editor de configuração com validação sintática e botão "Validate & Hot-Reload".

---

## 🛠️ Arquitetura do Projeto

O Ferrix é estruturado em 3 camadas independentes e desacopladas:

```
ferrix/
├── core/                  # 🦀 Ferramenta Core em Rust (Nginx-style Proxy Engine, standalone e ultra-leve)
│   ├── Cargo.toml         # Pacote Rust sem dependência de assets web
│   ├── src/               # Engine de Proxy, Load Balancing, WebSockets, Cache, CLI
│   ├── tests/             # Testes unitários e de integração
│   └── scripts/           # Scripts de instalação: install.sh (Linux/systemd), install.ps1 (Windows)
│
├── backend/               # 🟢 Backend de Gestão em Node.js + Express (TypeScript)
│   ├── package.json       # Servidor Node.js
│   ├── tsconfig.json
│   └── src/server.ts      # API REST, proxy para o Core, SSE stream e servidor do painel web
│
├── frontend/              # ⚛️ Painel Dashboard em React + Vite + Tailwind CSS
│   ├── package.json
│   ├── src/               # Componentes, telas de métricas e modal de autenticação
│   └── dist/              # Assets compilados entregues pelo backend Node.js
│
├── static_demo/           # Pasta de demonstração de arquivos estáticos servidos pelo Core
├── ferrix.json            # Arquivo de configuração persistente
└── test_backends.js       # Servidores mock para teste de balanceamento
```

---

## 🏁 Como Executar e Instalar

### 1. Requisitos

- **Rust & Cargo** (1.75+)
- **Node.js** (v18+) & **npm**

### 2. Executando em Desenvolvimento

```bash
# 1. Iniciar o Core em Rust (Proxy em :8000 e Control Plane em :8081):
cargo build --release --workspace
./target/release/ferrix --config ferrix.json

# 2. Iniciar o Backend em Node.js (API e Painel em :8080):
cd backend
npm install
npm run build
npm start

# 3. (Opcional) Iniciar os servidores de teste para validação de balanceamento:
node test_backends.js
```

Em seguida:

1. Abra no navegador: **`http://localhost:8080`** para acessar o Dashboard.
2. Insira o token de administração configurado no `ferrix.json` (`9f5c43dad9c341e6ac88e452d953f27b`).
3. Dispare requisições para `http://localhost:8000/api` e veja o tráfego balanceado em tempo real no Dashboard!

---

### 📦 3. Instalando o Ferrix Core no Sistema Operacional

O **Ferrix Core** pode ser instalado como um utilitário do sistema (`/usr/local/bin/ferrix` ou `C:\Program Files\Ferrix\ferrix.exe`) independente de Node.js:

#### No Linux (com serviço Systemd automático):

```bash
sudo ./core/scripts/install.sh
```

#### No Windows (PowerShell como Administrador):

```powershell
.\core\scripts\install.ps1
```

---

## 📡 API REST de Gestão

| Método   | Endpoint               | Descrição                                                  |
| -------- | ---------------------- | ---------------------------------------------------------- |
| `GET`    | `/api/status`          | Informações de uptime, portas e contadores                 |
| `GET`    | `/api/metrics`         | Snapshot de métricas (RPS, latência p95, status codes)     |
| `GET`    | `/api/logs`            | Histórico paginado de logs com busca e filtros             |
| `GET`    | `/api/logs/stream`     | Stream ao vivo de requisições via Server-Sent Events (SSE) |
| `GET`    | `/api/config`          | Configuração JSON ativa                                    |
| `POST`   | `/api/config`          | Atualiza a configuração global com hot-reload atômico      |
| `GET`    | `/api/vhosts`          | Lista de hosts virtuais                                    |
| `POST`   | `/api/vhosts`          | Cria ou atualiza um virtual host                           |
| `DELETE` | `/api/vhosts/:id`      | Remove um virtual host                                     |
| `GET`    | `/api/upstreams`       | Lista de pools de upstreams e status de saúde              |
| `POST`   | `/api/upstreams`       | Cria ou atualiza um pool de upstreams                      |
| `DELETE` | `/api/upstreams/:name` | Remove um pool de upstreams                                |
