# syntax=docker/dockerfile:1
# The NRESE server as one image: the server binary with the user console built in.
#
#   docker build -t nrese .
#   docker run -p 8080:8080 -v nrese-data:/var/lib/nrese/data nrese
#
# For x86-64 and arm64 at once (the Rust build runs on the building machine's own
# architecture and cross-compiles, so no emulated compiler):
#
#   docker buildx build --platform linux/amd64,linux/arm64 -t nrese .
#
# Configuration is by environment (docs/ops/config-reference.md) or a mounted config file
# (`-v ./config.toml:/etc/nrese/config.toml` and `--config /etc/nrese/config.toml`).

# Must match rust-toolchain.toml (a test checks it).
ARG RUST_VERSION=1.98.1

# --- the console -----------------------------------------------------------------------
# JavaScript: built once, on the building machine.
FROM --platform=$BUILDPLATFORM node:22-bookworm-slim AS console
WORKDIR /src/apps/nrese-console
COPY apps/nrese-console/package.json apps/nrese-console/package-lock.json ./
RUN npm ci
COPY apps/nrese-console/ ./
RUN npm run build

# --- the server ------------------------------------------------------------------------
FROM --platform=$BUILDPLATFORM rust:${RUST_VERSION}-bookworm AS build
ARG TARGETARCH
ARG BUILDARCH
# The Rust target, and a cross linker and C compiler (mimalloc, ring) where it isn't the
# building machine's architecture.
RUN case "$TARGETARCH" in \
      amd64) echo x86_64-unknown-linux-gnu > /rust-target ;; \
      arm64) echo aarch64-unknown-linux-gnu > /rust-target ;; \
      *) echo "unsupported architecture: $TARGETARCH" >&2; exit 1 ;; \
    esac \
    && rustup target add "$(cat /rust-target)" \
    && if [ "$TARGETARCH" != "$BUILDARCH" ]; then \
         apt-get update \
         && case "$TARGETARCH" in \
              amd64) apt-get install -y --no-install-recommends gcc-x86-64-linux-gnu libc6-dev-amd64-cross ;; \
              arm64) apt-get install -y --no-install-recommends gcc-aarch64-linux-gnu libc6-dev-arm64-cross ;; \
            esac \
         && rm -rf /var/lib/apt/lists/*; \
       fi
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY .cargo .cargo
COPY crates crates
# The server's build script embeds the console it finds here.
COPY --from=console /src/apps/nrese-console/dist apps/nrese-console/dist
# `--build-arg CARGO_BUILD_JOBS=8` keeps the build from taking every core.
ARG CARGO_BUILD_JOBS=default
# The CPU level the code is generated for (scripts/lib/target-cpu.sh): by default
# x86-64-v3 (AVX2: server CPUs of the last decade) on amd64, and neoverse-n1 on arm64
# (AWS Graviton 2 and later, Ampere, Raspberry Pi 5, Apple silicon), so the image runs
# wherever it is pushed. `--build-arg NRESE_TARGET_CPU=native` for the building machine
# (benchmark and local images), `portable` for any CPU of the architecture. The server
# refuses, with a message, to start on a CPU that lacks what it was built for. The flags
# apply to the target only (`--target`), never to build scripts run here.
ARG NRESE_TARGET_CPU=default
RUN target="$(cat /rust-target)" \
    && cpu="$NRESE_TARGET_CPU" \
    && if [ "$cpu" = default ]; then \
         case "$TARGETARCH" in amd64) cpu=x86-64-v3 ;; arm64) cpu=neoverse-n1 ;; esac; \
       fi \
    && flags="" \
    && if [ "$cpu" != portable ]; then flags="-C target-cpu=$cpu"; fi \
    && upper="$(echo "$target" | tr 'a-z-' 'A-Z_')" \
    && lower="$(echo "$target" | tr '-' '_')" \
    && if [ "$TARGETARCH" != "$BUILDARCH" ]; then \
         cross="$(echo "$target" | sed 's/-unknown//')"; \
         export "CARGO_TARGET_${upper}_LINKER=${cross}-gcc" "CC_${lower}=${cross}-gcc" \
                "AR_${lower}=${cross}-ar"; \
       fi \
    && env "CARGO_TARGET_${upper}_RUSTFLAGS=$flags" \
         cargo build --release --locked -p nrese-server --target "$target" \
    && cp "target/$target/release/nrese-server" /nrese-server

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
COPY --from=build /nrese-server /usr/local/bin/nrese-server
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
