# syntax=docker/dockerfile:1
# The NRESE server as one image: the server binary with the user console built in.
#
#   docker build -t nrese .
#   docker run -p 8080:8080 -v nrese-data:/var/lib/nrese/data nrese
#
# Configuration is by environment (docs/ops/config-reference.md) or a mounted config file
# (`-v ./config.toml:/etc/nrese/config.toml` and `--config /etc/nrese/config.toml`).

# Must match rust-toolchain.toml (a test checks it).
ARG RUST_VERSION=1.98.1

# --- the console -----------------------------------------------------------------------
FROM node:22-bookworm-slim AS console
WORKDIR /src/apps/nrese-console
COPY apps/nrese-console/package.json apps/nrese-console/package-lock.json ./
RUN npm ci
COPY apps/nrese-console/ ./
RUN npm run build

# --- the server ------------------------------------------------------------------------
FROM rust:${RUST_VERSION}-bookworm AS build
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY .cargo .cargo
COPY crates crates
# The server's build script embeds the console it finds here.
COPY --from=console /src/apps/nrese-console/dist apps/nrese-console/dist
# `--build-arg CARGO_BUILD_JOBS=8` keeps the build from taking every core.
ARG CARGO_BUILD_JOBS=default
# Code for x86-64-v3 (AVX2: server CPUs of the last decade), so the image runs wherever it
# is pushed. For the building machine's own CPU (benchmark and local images):
# `--build-arg NRESE_TARGET_CPU=native`; for any x86-64: `portable`
# (scripts/lib/target-cpu.sh). The server refuses, with a message, to start on a CPU that
# lacks what it was built for.
ARG NRESE_TARGET_CPU=x86-64-v3
RUN if [ "$NRESE_TARGET_CPU" != portable ]; then export RUSTFLAGS="-C target-cpu=$NRESE_TARGET_CPU"; fi \
    && cargo build --release --locked -p nrese-server

# --- the image -------------------------------------------------------------------------
FROM debian:bookworm-slim
# The binary needs no system libraries. The CA store is for outgoing HTTPS (an identity
# provider's token introspection); for an internal certificate authority, mount your CA
# bundle and point SSL_CERT_FILE at it.
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --system --uid 10001 --home-dir /var/lib/nrese --shell /usr/sbin/nologin nrese \
    && mkdir -p /var/lib/nrese/data \
    && chown -R nrese:nrese /var/lib/nrese
COPY --from=build /src/target/release/nrese-server /usr/local/bin/nrese-server
USER nrese
WORKDIR /var/lib/nrese
# Durable storage in the volume, reachable from outside the container. Everything else
# keeps the server's defaults.
ENV NRESE_BIND_ADDR=0.0.0.0:8080 \
    NRESE_STORE_MODE=on-disk \
    NRESE_DATA_DIR=/var/lib/nrese/data
VOLUME /var/lib/nrese/data
EXPOSE 8080
ENTRYPOINT ["nrese-server"]
