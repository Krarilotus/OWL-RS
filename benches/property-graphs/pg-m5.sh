#!/usr/bin/env bash
# M5, counts only (no timing): what a reifier's dictionary entry and a marking statement cost.
# Per variant of a mapped SNB scale, three loads and their counts (pg_hops m0):
#   X  the mapped data as it is (reifier IRIs);
#   Y  each reifier IRI replaced by its edge's subject: no reifier term is left, nothing new is
#      interned, so X's dictionary less Y's is what the reifiers' entries cost (Y's statements
#      differ and are not used);
#   M  X plus `r rdf:type <urn:x-nrese:EdgeOccurrence>` per reifier: the marking statement.
# X's store is left in the job directory for pg-m6.sh. Generated inputs live in the job
# directory only and are removed. Not an LDBC benchmark result.
#
#   bash benches/property-graphs/pg-m5.sh SF [a] [b]     # results to $FLEET_OUT
set -euo pipefail
A=${SNB_ASSETS:-$HOME/nrese-assets/ldbc-snb-20261030}
here=$(cd "$(dirname "$0")" && pwd)
root=$(cd "$here/../.." && pwd)
sf=$1; shift
variants=${*:-a b}
out=$FLEET_OUT
export CARGO_TARGET_DIR=${CARGO_TARGET_DIR:-$PWD/target-job}
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
for v in $variants; do
  y=$PWD/m5-sf$sf-$v-y.nt
  m=$PWD/m5-sf$sf-$v-m.nt
  LC_ALL=C awk -v Y="$y" -v M="$m" '
    BEGIN { e = "<http://ldbcouncil.org/snb/e/"
            reifies = "<http://www.w3.org/1999/02/22-rdf-syntax-ns#reifies>"
            mark = " <http://www.w3.org/1999/02/22-rdf-syntax-ns#type> <urn:x-nrese:EdgeOccurrence> ." }
    { print > M }
    index($0, e) == 1 {
      # The first line of a reifier reifies its edge (snb_to_rdf.py writes it first): r reifies <<( s p o )>> .
      if ($2 == reifies) { s = $4; print $1 mark > M; n++ }
      print s substr($0, length($1) + 1) > Y
      next
    }
    { print > Y }
    END { print n + 0 > "/dev/stderr" }' "$d/snb-$v.nt" 2> "$out/m5-sf$sf-$v-marked.txt"
  for kind in x y m; do
    case $kind in x) input=$d/snb-$v.nt ;; y) input=$y ;; m) input=$m ;; esac
    store=$PWD/store-sf$sf-$v
    [ "$kind" = x ] || store=$PWD/store-sf$sf-$v-$kind
    rm -rf "$store"
    "$bin" load "$input" "$store" --load-memory-gb "${LOAD_GB:-8}" > "$out/m5-load-sf$sf-$v-$kind.json"
    "$bin" m0 "$store" > "$out/m5-sf$sf-$v-$kind.json"
    [ "$kind" = x ] || rm -rf "$store"
  done
  rm -f "$y" "$m"
done
