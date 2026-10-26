FROM rust:1.96-bookworm AS build
RUN apt-get update && apt-get install -y --no-install-recommends cmake clang && rm -rf /var/lib/apt/lists/*
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY src ./src
RUN cargo build --release --locked

FROM debian:bookworm-slim
COPY --from=build /src/target/release/pingora-bench /usr/local/bin/pingora-bench
COPY conf.yaml /etc/pingora/conf.yaml
ENTRYPOINT ["/usr/local/bin/pingora-bench", "-c", "/etc/pingora/conf.yaml"]
