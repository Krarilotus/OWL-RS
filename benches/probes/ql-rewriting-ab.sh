#!/usr/bin/env bash
# The OWL 2 QL rewriting's answers gained and time cost (docs/design/ql-rewriting.md,
# performance.md §6): perf_lab with the rewriting off and on, interleaved (ABBA), PAIRS times.
#
#   benches/probes/ql-rewriting-ab.sh LABEL RULESET QUERIES_DIR FILE... 
#
# Needs a release perf_lab (scripts/cargo-guarded.sh build --release -p nrese-store
# --example perf_lab). Writes OUT/LABEL-{off,on}-N.json (OUT: NRESE_QL_AB_OUT, default
# target/ql-ab) and prints, per query, the rows off and on and the median of the p50s.
set -euo pipefail
label=$1 ruleset=$2 queries=$3
shift 3
loads=()
for file in "$@"; do loads+=(--load "$file"); done
ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
LAB=${PERF_LAB:-$ROOT/target/release/examples/perf_lab}
OUT=${NRESE_QL_AB_OUT:-$ROOT/target/ql-ab}
PAIRS=${PAIRS:-3}
mkdir -p "$OUT"
for n in $(seq 1 "$PAIRS"); do
  # ABBA: each mode runs first in every other pair.
  order="off on"
  [ $((n % 2)) = 0 ] && order="on off"
  for mode in $order; do
    setting=$([ "$mode" = on ] && echo auto || echo off)
    NRESE_REASONING_QL_REWRITING=$setting "$LAB" "${loads[@]}" --queries "$queries" \
      --reason "$ruleset" --runs "${RUNS:-5}" --warmup 1 --label "$label-$mode" \
      --json "$OUT/$label-$mode-$n.json" > "$OUT/$label-$mode-$n.log"
  done
done
${PYTHON:-python3} - "$OUT" "$label" "$PAIRS" <<'PY'
import json, statistics, sys
out, label, pairs = sys.argv[1], sys.argv[2], int(sys.argv[3])
runs = {m: [json.load(open(f"{out}/{label}-{m}-{n}.json")) for n in range(1, pairs + 1)] for m in ("off", "on")}
def per_query(m):
    q = {}
    for r in runs[m]:
        for e in r["queries"]:
            q.setdefault(e["name"], []).append(e)
    return q
off, on = per_query("off"), per_query("on")
print(f"{'query':<12}{'rows off':>10}{'rows on':>10}{'p50 off ms':>12}{'p50 on ms':>12}{'on/off':>8}")
tot_off = tot_on = 0.0
for name in sorted(off):
    a, b = off[name], on[name]
    rows_a, rows_b = {e.get("rows") for e in a}, {e.get("rows") for e in b}
    ma = statistics.median(e["p50_ms"] for e in a if "p50_ms" in e)
    mb = statistics.median(e["p50_ms"] for e in b if "p50_ms" in e)
    tot_off += ma; tot_on += mb
    print(f"{name:<12}{'/'.join(map(str, rows_a)):>10}{'/'.join(map(str, rows_b)):>10}{ma:>12.2f}{mb:>12.2f}{mb / ma if ma else 0:>8.2f}")
print(f"{'sum':<12}{'':>20}{tot_off:>12.2f}{tot_on:>12.2f}{tot_on / tot_off:>8.2f}")
for m in ("off", "on"):
    print(m, "load+reason s:", [round(r["load_s"], 2) for r in runs[m]])
PY
