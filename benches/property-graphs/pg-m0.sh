#!/usr/bin/env bash
# M0, counts only (no timing): loads each variant of a mapped SNB scale (snb-map.sh) into a
# fresh store, counts it (pg_hops m0) and checks the counts against the mapping's
# (snb_check.py). Records the build, the inputs' hashes, peak memory and the store's size.
#
#   bash benches/property-graphs/pg-m0.sh SF [a] [b]     # results to $FLEET_OUT; exit 1 on a difference
set -euo pipefail
A=${SNB_ASSETS:-$HOME/nrese-assets/ldbc-snb-20261030}
here=$(cd "$(dirname "$0")" && pwd)
root=$(cd "$here/../.." && pwd)
sf=$1; shift
variants=${*:-a b}
out=$FLEET_OUT
python=${PYTHON:-python3}
export CARGO_TARGET_DIR=${CARGO_TARGET_DIR:-$PWD/target-job}
timed() { # peak memory and times of a command, where GNU time is installed
  local record=$1; shift
  if [ -x /usr/bin/time ]; then /usr/bin/time -v -o "$record" "$@"; else "$@"; fi
}
{
  echo "host $(hostname) $(uname -r) $(nproc) cpus"
  grep -m1 'model name' /proc/cpuinfo || true
  rustc -V; cargo -V
  echo "rev $(git -C "$root" rev-parse HEAD 2>/dev/null || echo unknown)"
  echo "dirty $(git -C "$root" status --porcelain 2>/dev/null | wc -l)"
  echo "RUSTFLAGS=${RUSTFLAGS:-} profile=release"
} > "$out/env.txt"
cargo build --release --locked -p nrese-engine --example pg_hops > "$out/build.log" 2>&1
bin=$CARGO_TARGET_DIR/release/examples/pg_hops
sha256sum "$bin" >> "$out/env.txt"
d=$A/rdf/sf$sf
(cd "$d" && sha256sum --check SHA256SUMS) > "$out/inputs-sf$sf.txt"
cp "$d/SHA256SUMS" "$out/SHA256SUMS-sf$sf"
mapfile -t probes < <("$python" -I "$here/snb_check.py" probes "$d/counts.json")
args=()
for p in "${probes[@]}"; do args+=(--probe "$p"); done
status=0
for v in $variants; do
  store=$PWD/store-sf$sf-$v
  rm -rf "$store"
  timed "$out/load-sf$sf-$v.time" "$bin" load "$d/snb-$v.nt" "$store" --load-memory-gb "${LOAD_GB:-8}" \
    > "$out/load-sf$sf-$v.json"
  timed "$out/m0-sf$sf-$v.time" "$bin" m0 "$store" "${args[@]}" > "$out/m0-sf$sf-$v.json"
  "$python" -I "$here/snb_check.py" check "$d/counts.json" "$out/m0-sf$sf-$v.json" "${v^^}" \
    > "$out/check-sf$sf-$v.txt" || status=1
  rm -rf "$store"
done
exit $status
