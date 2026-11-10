#!/usr/bin/env bash
# Run inside a Linux fleet job; no build or server runs on the owner's workstation.
set -euo pipefail
export PATH="$HOME/.cargo/bin:$HOME/.local/bin:$PATH"
export CARGO_TARGET_DIR="$PWD/../target-capability-demo" CARGO_BUILD_JOBS=4
# A disposable sentinel above every server's directory: the server loads the first .env from
# its working directory upwards (dotenvy), and one that loaded this one would refuse to start.
printf 'NRESE_READER_WALK_SLICE=0\n' > "$FLEET_OUT/.env"
{
    git rev-parse HEAD HEAD^{tree}
    git status --short
    rustc --version
    cargo --version
    sha256sum scripts/demo/capabilities/contract.nt
    hostname
} | tee "$FLEET_OUT/identity.log"
cargo build --locked -p nrese-server --bin nrese-server 2>&1 | tee "$FLEET_OUT/build-server.log"
cargo build --locked -p nrese-engine --example held_snapshot 2>&1 | tee "$FLEET_OUT/build-snapshot.log"
uv run --no-project --python 3.12 scripts/demo/capabilities/run.py \
    --server "$CARGO_TARGET_DIR/debug/nrese-server" \
    --snapshot "$CARGO_TARGET_DIR/debug/examples/held_snapshot" \
    --out "$FLEET_OUT/demo-state" 2>&1 | tee "$FLEET_OUT/demos.log"
# verify starts each server in a fresh directory inside its scratch; bar the search there too.
mkdir "$FLEET_OUT/verify-scratch"
: > "$FLEET_OUT/verify-scratch/.env"
uv run --no-project --python 3.12 scripts/verify/verify.py run \
    --server "$CARGO_TARGET_DIR/debug/nrese-server" --start-timeout 30 \
    --scratch "$FLEET_OUT/verify-scratch" \
    scripts/verify/scenarios/repository-lifecycle.toml 2>&1 | tee "$FLEET_OUT/verify.log"
