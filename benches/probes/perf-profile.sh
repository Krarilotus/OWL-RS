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
# NRESE_PROFILE_EXAMPLE=crate:example profiles that example instead of nrese-server (the
# repository is at /src, read-only; /out is writable), e.g. the EL classifier:
#   NRESE_PROFILE_EXAMPLE=nrese-reasoner:classify benches/probes/perf-profile.sh el -- \
#     --out /out/hierarchy.tsv /src/tmp/el-lab/nt/ore_ont_15929.nt
#
# Remove the volume afterwards: scripts/bench-cleanup.sh --owner probe (this job's alone)
set -euo pipefail
# This job's names (scripts/lib/ns.sh): its containers and volumes; the shared datasets' volume.
NS_ROOT=$(cd "$(dirname "$0")/../.." && pwd)
. "$NS_ROOT/scripts/lib/ns.sh"
DATA_VOLUME=$(ns_shared data)
TARGET_VOLUME=$(ns_name prof-target "$NS_ROOT")
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
ns_volume create "$TARGET_VOLUME" "$NS_ROOT" probe >/dev/null  # labelled: its own, kept between profiles
MSYS_NO_PATHCONV=1 docker run --rm --privileged \
  -v "$src:/src:ro" -v "$TARGET_VOLUME":/target -v "$(ns_shared cargo)":/usr/local/cargo/registry \
  -v "$DATA_VOLUME":/data:ro -v "$(cd "$out" && pwd -W 2>/dev/null || pwd):/out" \
  -e RUSTFLAGS="-C target-cpu=native -C force-frame-pointers=yes" \
  -e CARGO_PROFILE_RELEASE_DEBUG=line-tables-only -e EXAMPLE="${NRESE_PROFILE_EXAMPLE:-}" "${envs[@]}" \
  -w /src "$image" bash -c '
    set -e
    apt-get update -qq >/dev/null 2>&1 && apt-get install -y -qq linux-perf >/dev/null 2>&1
    if [ -n "$EXAMPLE" ]; then
      cargo build --release --locked -q -p "${EXAMPLE%%:*}" --example "${EXAMPLE#*:}" --target-dir /target 2>&1 | tail -3
      binary="/target/release/examples/${EXAMPLE#*:}"
    else
      cargo build --release --locked -q -p nrese-server --target-dir /target 2>&1 | tail -3
      binary=/target/release/nrese-server
    fi
    rm -rf /tmp/store
    export NRESE_STORE_MODE=on-disk NRESE_DATA_DIR=/tmp/store RUST_LOG=info,nrese_engine=debug NO_COLOR=1
    perf record -F 999 -g --call-graph fp -o /tmp/perf.data "$binary" "$@" > /out/command.log 2>&1
    flat() { grep -E "^ +[0-9]+\.[0-9]+%" | sed "s/  */ /g" | cut -c1-200; }
    perf report -i /tmp/perf.data --no-children --sort symbol --stdio -g none --percent-limit 0.3 2>/dev/null | flat > /out/flat.txt
    perf report -i /tmp/perf.data --children --sort symbol --stdio -g none --percent-limit 1.5 2>/dev/null | flat > /out/children.txt
    perf report -i /tmp/perf.data --no-children --sort symbol --stdio -g caller,0.5,callee,function,percent --percent-limit 3 2>/dev/null \
      | grep -v "^$" | cut -c1-220 | head -300 > /out/callers.txt
    cp /tmp/perf.data /out/perf.data
    echo done
  ' bash "$@"
echo "report: $out"
