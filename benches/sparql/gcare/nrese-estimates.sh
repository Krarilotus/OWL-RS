#!/usr/bin/env bash
# NRESE's estimates on G-CARE's queries: for each graph, a store loaded from gcare-NAME.nt, a
# server on it, estimates.py asking the planner for every query (explain=plan, never run), the
# server stopped and the store removed; then summarise.py beside G-CARE's estimators.
#
#   benches/sparql/gcare/nrese-estimates.sh OUT-DIR [GCARE-RESULTS-DIR] [DATASET...]
#
# NRESE_BIN names the server binary; SPARQL_DATA the data directory (prepare.sh gcare);
# NRESE_BENCH_SCRATCH the stores' place. The estimation times are timings: run it inside
# scripts/quiet-slot.sh.
set -euo pipefail
ROOT=$(cd "$(dirname "$0")/../../.." && pwd)
OUT=$(realpath -m "${1:?usage: nrese-estimates.sh OUT-DIR [GCARE-RESULTS-DIR] [DATASET...]}"); shift
GCARE=${1:-}; [ $# -gt 0 ] && shift
datasets=("$@"); [ ${#datasets[@]} -gt 0 ] || datasets=(aids human lubm80 yago)
DATA=${SPARQL_DATA:-$HOME/owlrs/sparql-data}
BIN=${NRESE_BIN:?NRESE_BIN names the nrese-server binary}
SCRATCH=${NRESE_BENCH_SCRATCH:-$HOME/owlrs/sparql-scratch}
PORT=${GCARE_PORT:-18962}
mkdir -p "$OUT" "$SCRATCH"
sha256sum "$BIN" > "$OUT/nrese-binary.sha256"
for d in "${datasets[@]}"; do
  store=$SCRATCH/gcare-$d-store
  rm -rf "$store"
  echo "== $d"
  NRESE_STORE_MODE=on-disk NRESE_DATA_DIR=$store RUST_LOG=warn "$BIN" load "$DATA/gcare-$d.nt" > "$OUT/$d-load.log" 2>&1
  NRESE_STORE_MODE=on-disk NRESE_DATA_DIR=$store RUST_LOG=warn NRESE_BIND_ADDR=127.0.0.1:$PORT \
    NRESE_READ_REQUESTS_PER_WINDOW=100000000 "$BIN" > "$OUT/$d-server.log" 2>&1 &
  pid=$!
  for _ in $(seq 120); do curl -sf "http://127.0.0.1:$PORT/readyz" >/dev/null && break; sleep 1; done
  python3 -I "$ROOT/benches/sparql/gcare/estimates.py" --endpoint "http://127.0.0.1:$PORT/dataset/query" \
    --queries "$ROOT/benches/competitors/queries/gcare-$d" --out "$OUT/$d-nrese.tsv" || echo "$d: estimates failed"
  kill "$pid"; wait "$pid" 2>/dev/null || true
  rm -rf "$store"
  if [ -n "$GCARE" ]; then
    python3 -I "$ROOT/benches/sparql/gcare/summarise.py" --truth "$ROOT/benches/competitors/queries/gcare-$d" \
      --nrese "$OUT/$d-nrese.tsv" --gcare "$GCARE" --json "$OUT/$d-summary.json" | tee "$OUT/$d-summary.txt"
  fi
done
