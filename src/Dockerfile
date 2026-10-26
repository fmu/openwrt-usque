FROM rust:slim AS build

WORKDIR /app

RUN --mount=type=cache,target=/var/cache/apt \
    apt-get update && \
    apt-get install -y --no-install-recommends \
    build-essential \
    cmake \
    clang \
    git \
    perl \
    pkg-config \
    ca-certificates && \
    rm -rf /var/lib/apt/lists/*

COPY Cargo.toml Cargo.lock ./
COPY crates ./crates

RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/app/target \
    cargo build --release -p usque-cli --locked && \
    cp target/release/usque /app/usque

FROM debian:stable-slim

RUN --mount=type=cache,target=/var/cache/apt \
    apt-get update && \
    apt-get install -y --no-install-recommends \
    ca-certificates && \
    rm -rf /var/lib/apt/lists/*

WORKDIR /app

COPY --from=build /app/usque /app/usque

ENTRYPOINT ["/app/usque"]
