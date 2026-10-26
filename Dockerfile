# ==============================================================================
# Stage 1: Rust Builder (compiles native static binary)
# ==============================================================================
FROM rust:alpine AS rust-builder
WORKDIR /src

RUN apk add --no-cache musl-dev

COPY Cargo.toml Cargo.lock ./
COPY .cargo ./.cargo
COPY core ./core

RUN cargo build --release --workspace

# ==============================================================================
# Stage 2: Web Builder (builds Frontend UI & Backend Gateway for Full variant)
# ==============================================================================
FROM node:22-alpine AS web-builder
WORKDIR /src

COPY frontend/package*.json ./frontend/
RUN cd frontend && npm ci

COPY frontend ./frontend
RUN cd frontend && npm run build

COPY backend/package*.json ./backend/
RUN cd backend && npm ci

COPY backend ./backend
RUN cd backend && npm run build && npm prune --production

# ==============================================================================
# Target: full (All-in-one with React Web Dashboard & Node.js API Gateway)
# ==============================================================================
FROM node:22-alpine AS full

RUN apk add --no-cache ca-certificates tzdata

COPY --from=rust-builder /src/target/release/ferrix /usr/local/bin/ferrix
COPY ferrix.example.json /etc/ferrix/ferrix.example.json
COPY docker/entrypoint-full.sh /usr/local/bin/entrypoint-full.sh
RUN chmod +x /usr/local/bin/entrypoint-full.sh

WORKDIR /app
COPY --from=web-builder /src/backend /app/backend
COPY --from=web-builder /src/frontend/dist /app/frontend/dist

VOLUME ["/etc/ferrix"]

ENV PORT=8080 \
    FERRIX_CORE_URL=http://127.0.0.1:8081 \
    FERRIX_CONFIG=/etc/ferrix/ferrix.json

EXPOSE 8000 8081 8080

ENTRYPOINT ["/usr/local/bin/entrypoint-full.sh"]

# ==============================================================================
# Target: core (Default - Ultra-enxuta, ~19MB, pure Rust proxy & control plane)
# Placed as the last stage so standard `docker build .` defaults to core.
# ==============================================================================
FROM alpine:3.21 AS core

RUN apk add --no-cache ca-certificates tzdata && \
    mkdir -p /etc/ferrix && \
    addgroup -g 10001 -S ferrix && \
    adduser -u 10001 -S -G ferrix -h /etc/ferrix ferrix && \
    chown -R ferrix:ferrix /etc/ferrix

COPY --from=rust-builder /src/target/release/ferrix /usr/local/bin/ferrix
COPY ferrix.example.json /etc/ferrix/ferrix.example.json

USER ferrix:ferrix
WORKDIR /etc/ferrix

VOLUME ["/etc/ferrix"]

EXPOSE 8000 8081

ENTRYPOINT ["/usr/local/bin/ferrix"]
CMD ["--config", "/etc/ferrix/ferrix.json"]
