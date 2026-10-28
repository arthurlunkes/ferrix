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
# Stage 2: Web Builder (builds the React dashboard for the full variant)
# ==============================================================================
FROM node:22-alpine AS web-builder
WORKDIR /src

COPY frontend/package*.json ./frontend/
RUN cd frontend && npm ci

COPY frontend ./frontend
RUN cd frontend && npm run build

# ==============================================================================
# Target: full (proxy + web dashboard, served by the same Ferrix process)
# ==============================================================================
FROM alpine:3.21 AS full

RUN apk add --no-cache ca-certificates tzdata && mkdir -p /etc/ferrix

COPY --from=rust-builder /src/target/release/ferrix /usr/local/bin/ferrix
COPY --from=web-builder /src/frontend/dist /usr/share/ferrix/dashboard
COPY docker/ferrix.full.yaml /usr/share/ferrix/ferrix.full.yaml
COPY docker/entrypoint-full.sh /usr/local/bin/entrypoint-full.sh
RUN chmod +x /usr/local/bin/entrypoint-full.sh

VOLUME ["/etc/ferrix"]

# The container runtime restarts Ferrix (`--restart` / compose `restart:`), so the
# built-in master process isn't needed: one process, less memory.
ENV FERRIX_CONFIG=/etc/ferrix/ferrix.yaml \
    FERRIX_SUPERVISOR=0

# 80: proxy traffic · 8080: admin API and web dashboard
EXPOSE 80 8080

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
COPY ferrix.example.yaml /etc/ferrix/ferrix.example.yaml

USER ferrix:ferrix
WORKDIR /etc/ferrix

VOLUME ["/etc/ferrix"]

EXPOSE 80 8081

ENV FERRIX_SUPERVISOR=0

ENTRYPOINT ["/usr/local/bin/ferrix"]
CMD ["--config", "/etc/ferrix/ferrix.yaml"]
