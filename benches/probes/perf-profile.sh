#!/usr/bin/env bash
# CPU profile of an NRESE command with Linux perf, in Docker (Windows hosts without an
# elevated session have no ETW sampling; this works there too). Builds nrese-server from
# the working tree with frame pointers and line tables into its own target volume
# (nrese-prof-target), runs the command against the datasets volume, and writes under
# tmp/profile-<label>/: perf.data, flat.txt (self time by symbol), children.txt (time
# including callees), callers.txt (the top symbols with their callers), command.log.
#
#   benches/probes/perf-profile.sh LABEL -- load /data/yago-tiny.nt
#   NRESE_PROFILE_ENV="NRESE_REASONING_MODE=owl2-rl" benches/probes/perf-profile.sh lubm -- load /data/univ-bench.nt /data/lubm-100.nt
#
# Remove the volume afterwards: docker volume rm nrese-prof-target
set -euo pipefail
REPO=$(cd "$(dirname "$0")/../.." && pwd)
label=$1; shift
[ "${1:-}" = -- ] && shift
out="$REPO/tmp/profile-$label"
mkdir -p "$out"
toolchain=$(sed -n 's/^channel = "\(.*\)"/\1/p' "$REPO/rust-toolchain.toml")
image="rust:${toolchain}-bookworm"
src=$(cd "$REPO" && pwd -W 2>/dev/null || pwd)
envs=()
for e in ${NRESE_PROFILE_ENV:-}; do envs+=(-e "$e"); done
MSYS_NO_PATHCONV=1 docker run --rm --privileged \
  -v "$src:/src:ro" -v nrese-prof-target:/target -v nrese-cargo:/usr/local/cargo/registry \
  -v nrese-bench-data:/data:ro -v "$(cd "$out" && pwd -W 2>/dev/null || pwd):/out" \
  -e RUSTFLAGS="-C target-cpu=native -C force-frame-pointers=yes" \
  -e CARGO_PROFILE_RELEASE_DEBUG=line-tables-only "${envs[@]}" \
  -w /src "$image" bash -c '
    set -e
    apt-get update -qq >/dev/null 2>&1 && apt-get install -y -qq linux-perf >/dev/null 2>&1
    cargo build --release --locked -q -p nrese-server --target-dir /target 2>&1 | tail -3
    rm -rf /tmp/store
    export NRESE_STORE_MODE=on-disk NRESE_DATA_DIR=/tmp/store RUST_LOG=info,nrese_engine=debug NO_COLOR=1
    perf record -F 999 -g --call-graph fp -o /tmp/perf.data /target/release/nrese-server "$@" > /out/command.log 2>&1
    flat() { grep -E "^ +[0-9]+\.[0-9]+%" | sed "s/  */ /g" | cut -c1-200; }
    perf report -i /tmp/perf.data --no-children --sort symbol --stdio -g none --percent-limit 0.3 2>/dev/null | flat > /out/flat.txt
    perf report -i /tmp/perf.data --children --sort symbol --stdio -g none --percent-limit 1.5 2>/dev/null | flat > /out/children.txt
    perf report -i /tmp/perf.data --no-children --sort symbol --stdio -g caller,0.5,callee,function,percent --percent-limit 3 2>/dev/null \
      | grep -v "^$" | cut -c1-220 | head -300 > /out/callers.txt
    cp /tmp/perf.data /out/perf.data
    echo done
  ' bash "$@"
echo "report: $out"
