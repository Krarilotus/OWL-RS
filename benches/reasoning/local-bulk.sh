#!/usr/bin/env bash
# Local end-of-batch reasoning numbers (development machine, not published figures):
# batch materialisation, end-to-end load + reasoning + queries + commits, and delta
# latency. Inputs: DATA (default ~/nrese-bench/reasoning) with univ-bench.nt,
# lubm-{1,10,100}.nt and owl2bench/owl2bench-{el,ql,rl,dl}-1.nt.
#
#   benches/reasoning/local-bulk.sh > results.txt
set -euo pipefail
DATA=${DATA:-$HOME/nrese-bench/reasoning}
cd "$(dirname "$0")/../.."
cargo build -q --release -p nrese-reasoner --example v2_closure --example v2_delta
cargo build -q --release -p nrese-store --example reason_query
out=$(mktemp)
echo "== batch materialisation (v2_closure)"
for n in 1 10 100; do
  ./target/release/examples/v2_closure --out "$out" "$DATA/univ-bench.nt" "$DATA/lubm-$n.nt" 2>&1 |
    sed -n "s/^owl2-rl batch: /lubm-$n: /p"
done
for p in el ql rl dl; do
  ./target/release/examples/v2_closure --out "$out" "$DATA/owl2bench/owl2bench-$p-1.nt" 2>&1 |
    sed -n "s/^owl2-rl batch: /owl2bench-$p-1: /p"
done
echo "== end to end (reason_query: bulk load, materialise, 14 LUBM queries, commits)"
for n in 1 10 100; do
  echo "lubm-$n:"
  ./target/release/examples/reason_query --commits 200 --queries benches/reasoning/queries/lubm \
    "$DATA/univ-bench.nt" "$DATA/lubm-$n.nt" 2>&1 | grep -E '^owl2-rl|^commits|^queries' | sed 's/^/  /'
done
echo "== delta executor over in-memory facts (v2_delta)"
./target/release/examples/v2_delta --changes 50 --check "$DATA/univ-bench.nt" "$DATA/lubm-1.nt" | sed 's/^/lubm-1 (checked): /'
./target/release/examples/v2_delta --changes 200 "$DATA/univ-bench.nt" "$DATA/lubm-10.nt" | sed 's/^/lubm-10: /'
rm -f "$out"
