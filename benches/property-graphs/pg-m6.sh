#!/usr/bin/env bash
# M6, counts only (no timing): placement of the SNB store over 4, 16 and 64 ranges under the
# anchor rule (contiguous and hashed) and a hash per term; the edge-property hops that cross a
# range, forward and backward, and what an in-edge mirror would hold (pg_hops m6). Uses the
# store pg-m5.sh left in the job directory, else loads one. Not an LDBC benchmark result.
#
#   bash benches/property-graphs/pg-m6.sh SF [a] [b]     # results to $FLEET_OUT
set -euo pipefail
A=${SNB_ASSETS:-$HOME/nrese-assets/ldbc-snb-20261030}
here=$(cd "$(dirname "$0")" && pwd)
root=$(cd "$here/../.." && pwd)
sf=$1; shift
variants=${*:-a b}
out=$FLEET_OUT
export CARGO_TARGET_DIR=${CARGO_TARGET_DIR:-$PWD/target-job}
bin=$CARGO_TARGET_DIR/release/examples/pg_hops
d=$A/rdf/sf$sf
if [ ! -f "$out/env.txt" ]; then  # not after pg-m5.sh in this job: its own build and records
  {
    echo "host $(hostname) $(uname -r) $(nproc) cpus"
    grep -m1 'model name' /proc/cpuinfo || true
    rustc -V; cargo -V
    echo "rev $(git -C "$root" rev-parse HEAD 2>/dev/null || echo unknown)"
    echo "dirty $(git -C "$root" status --porcelain 2>/dev/null | wc -l)"
    echo "RUSTFLAGS=${RUSTFLAGS:-} profile=release"
  } > "$out/env.txt"
  cargo build --release --locked -p nrese-engine --example pg_hops > "$out/build.log" 2>&1
  sha256sum "$bin" >> "$out/env.txt"
fi
(cd "$d" && sha256sum --check SHA256SUMS) > "$out/m6-inputs-sf$sf.txt"
for v in $variants; do
  store=$PWD/store-sf$sf-$v
  if [ ! -d "$store" ]; then
    "$bin" load "$d/snb-$v.nt" "$store" --load-memory-gb "${LOAD_GB:-8}" > "$out/m6-load-sf$sf-$v.json"
  fi
  "$bin" m6 "$store" > "$out/m6-sf$sf-$v.json"
  rm -rf "$store"
done
