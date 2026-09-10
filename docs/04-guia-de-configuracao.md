# 04. Guia de Recursos e Configuração

O arquivo de configuração do Ferrix é armazenado em formato **JSON** (por padrão `ferrix.json`), permitindo fácil manipulação tanto visualmente quanto por scripts ou ferramentas de infraestrutura como código.

---

## 📄 Estrutura Geral do `ferrix.json`

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

Cada Virtual Host representa um bloco de servidor (equivalente a um bloco `server { ... }` no Nginx).

```json
{
  "id": "vhost-principal",
  "name": "Meu Aplicativo Web",
  "server_names": ["meudominio.com", "www.meudominio.com", "localhost"],
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
    "password": "senha_segura_aqui"
  },
  "rate_limit": {
    "requests_per_second": 50,
    "burst": 100
  },
  "routes": [ ... ]
}
```

### Propriedades do Virtual Host:

- `id`: Identificador único (usado nas URLs da API REST).
- `name`: Nome legível amigável para exibição no dashboard.
- `server_names`: Lista de domínios ou IPs aceitos no cabeçalho `Host`. Suporta `*` para capturar todos os domínios.
- `listen_port`: Porta de escuta do tráfego.
- `headers`: Cabeçalhos HTTP adicionados a todas as respostas servidas por este host virtual.
- `allowed_ips`: Lista de IPs autorizados. Se vazia, permite todos (salvo os negados).
- `denied_ips`: Lista de IPs bloqueados (retorna `403 Forbidden`).
- `basic_auth` (opcional): Protege todas as rotas com autenticação básica HTTP.
- `rate_limit` (opcional): Limite global do host por IP cliente.

---

## 🛣️ Regras de Roteamento (Routes)

Dentro de cada Virtual Host, você define regras de caminho (`routes`), equivalentes aos blocos `location` do Nginx:

### 1. Rota de Proxy Reverso (Reverse Proxy)

Encaminha o tráfego para um pool de servidores de backend:

```json
{
  "id": "rota-api",
  "path_prefix": "/api",
  "strip_prefix": false,
  "target_type": "proxy_upstream",
  "upstream_name": "meu_backend_cluster",
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

- `strip_prefix`: Se `true`, remove `/api` da URL antes de enviar ao backend.
- `websocket_enabled`: Permite conexões WebSocket de longa duração com tunneling bidirecional.
- `proxy_set_headers`: Mapeamento de cabeçalhos enviados ao backend com variáveis especiais (`$host`, `$remote_addr`, `$proxy_add_x_forwarded_for`, `$scheme`).

### 2. Rota de Arquivos Estáticos com Fallback SPA

Serve arquivos locais e redireciona rotas virtuais para `index.html`:

```json
{
  "id": "rota-frontend",
  "path_prefix": "/",
  "strip_prefix": false,
  "target_type": "static_files",
  "static_root": "./frontend/dist",
  "try_files": "$uri $uri/ /index.html",
  "websocket_enabled": false
}
```

- `static_root`: Diretório local no disco onde os arquivos estão localizados.
- `try_files`: Tenta primeiro o arquivo exato (`$uri`), depois o diretório com index (`$uri/`) e finalmente o fallback (`/index.html`). Ideal para React Router, Vue Router, etc.

### 3. Rota de Redirecionamento HTTP

Redireciona o navegador para outra URL:

```json
{
  "id": "rota-docs",
  "path_prefix": "/docs",
  "strip_prefix": false,
  "target_type": "redirect",
  "redirect_url": "https://github.com/meu-projeto",
  "redirect_code": 301
}
```

---

## ⚖️ Upstreams e Balanceamento de Carga

Os upstreams representam conjuntos de nós de backend que dividem a carga de trabalho.

```json
{
  "upstreams": {
    "meu_backend_cluster": {
      "name": "meu_backend_cluster",
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

### Algoritmos Suportados (`algorithm`):

1. **`round_robin`**: Distribui cada requisição sequencialmente entre os nós saudáveis.
2. **`weighted_round_robin`**: Distribui proporcionalmente ao peso (`weight`) de cada servidor (ex: peso 3 recebe 3x mais requisições que peso 1).
3. **`least_conn`**: Direciona para o nó que possui a menor quantidade de conexões ativas no instante da requisição.
4. **`ip_hash`**: Gera um hash do IP do cliente, garantindo que o mesmo cliente caia sempre no mesmo servidor (sessão persistente).
5. **`random`**: Distribuição aleatória uniforme.

### Failover Automático (_proxy_next_upstream_):

Se um nó do pool falhar (erro de conexão de rede, timeout, ou código HTTP 502/504):

1. O Ferrix incrementa o contador de falhas do nó.
2. Tenta imediatamente o próximo nó disponível no pool de candidatos.
3. Se o número de falhas consecutivas atingir `max_fails`, o nó é marcado temporariamente como **UNHEALTHY** até que o health check ativo confirme seu retorno.
