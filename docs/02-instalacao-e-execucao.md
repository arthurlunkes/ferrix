# 02. Instalação e Execução

Este guia orienta como compilar, rodar e operar o **Ferrix** em ambientes de desenvolvimento e produção.

---

## 📋 Pré-requisitos

- **Sistema Operacional**: Linux, macOS ou Windows.
- **Rust Toolchain**: Versão estável (1.75 ou superior).
  ```bash
  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
  ```
- **Node.js & npm** (necessário apenas caso queira customizar ou recompilar o frontend em React):
  - Node.js 18+ (recomendado Node.js 20+).

---

## 🔨 Compilação do Projeto

O Ferrix usa o crate `rust-embed` para incluir todos os assets compilados do React (HTML, CSS e JavaScript) diretamente dentro do binário executável final. Dessa forma, você distribui **um único arquivo binário** sem depender de arquivos soltos.

### Passo 1: Compilar o Frontend React

```bash
cd frontend
npm install
npm run build
cd ..
```

Isso gerará os arquivos estáticos na pasta `frontend/dist/`.

### Passo 2: Compilar o Backend em Rust

Para ambiente de desenvolvimento:

```bash
cargo build
```

O executável será gerado em: `target/debug/ferrix`.

Para ambiente de produção (máxima otimização e velocidade):

```bash
cargo build --release
```

O executável otimizado será gerado em: `target/release/ferrix`.

---

## 🚀 Execução

### 1. Inicialização Básica

```bash
./target/release/ferrix
```

Por padrão:

- Se o arquivo `ferrix.json` não existir, o Ferrix cria uma configuração inicial padrão automaticamente.
- O painel administrativo fica disponível em: `http://localhost:8080`.
- O tráfego de proxy reverso e servidor web escuta em: `http://localhost:8000`.

### 2. Flags e Parâmetros de Linha de Comando (CLI)

O Ferrix aceita os seguintes argumentos:

```text
Usage: ferrix [OPTIONS]

Options:
  -c, --config <CONFIG>              Caminho para o arquivo de configuração [padrão: ferrix.json]
      --admin-listen <ADMIN_LISTEN>  Sobrescreve o IP e porta do painel web/API (ex: 0.0.0.0:8080)
      --http-listen <HTTP_LISTEN>    Sobrescreve o IP e porta do proxy de tráfego (ex: 0.0.0.0:80)
  -h, --help                         Exibe o menu de ajuda
  -V, --version                      Exibe a versão instalada
```

#### Exemplos:

Rodar com arquivo de configuração específico:

```bash
./target/release/ferrix --config /etc/ferrix/production.json
```

Rodar em portas de produção (80 para tráfego e 8080 para admin):

```bash
sudo ./target/release/ferrix --http-listen 0.0.0.0:80 --admin-listen 127.0.0.1:8080
```

---

## 🧪 Ambiente de Teste com Backends Mock

Para experimentar o balanceamento de carga, você pode usar o arquivo `test_backends.js` fornecido na raiz:

```bash
# Em um terminal:
node test_backends.js

# Saída esperada:
# [Backend-1] listening on http://127.0.0.1:9001
# [Backend-2] listening on http://127.0.0.1:9002
```

Em outro terminal, inicie o Ferrix:

```bash
./target/debug/ferrix --config ferrix.json
```

Agora envie requisições para `http://localhost:8000/api` e observe:

1. O retorno alternando entre `Backend-1` e `Backend-2`.
2. As métricas e os logs aparecendo instantaneamente no painel em `http://localhost:8080`.

---

## 🐳 Execução via Docker (Opcional)

Exemplo de `Dockerfile` multi-stage para gerar uma imagem minúscula:

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
COPY src/ src/
COPY --from=ui-builder /app/frontend/dist frontend/dist
RUN cargo build --release

# Stage 3: Runtime
FROM alpine:3.20
WORKDIR /app
COPY --from=rust-builder /app/target/release/ferrix /usr/local/bin/ferrix
COPY ferrix.json ./
EXPOSE 8000 8080
CMD ["ferrix", "--config", "ferrix.json"]
```
