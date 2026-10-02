#!/usr/bin/env bash
# The suite's count task over HTTP, first request after a restart: Wikidata lexemes store.
#
# Usage: STORE=<on-disk store dir> benches/probes/count-after-restart.sh   (Linux)
set -u
source "$(dirname "$0")/common.sh"
cd "$REPO"
nice scripts/cargo-guarded.sh build --release -p nrese-server 2>&1 | tail -1
bin=target/release/nrese-server
export NRESE_STORE_MODE=on-disk NRESE_DATA_DIR="${STORE:-$SCRATCH/store-w4}" RUST_LOG=info
NRESE_BIND_ADDR=127.0.0.1:18082 NRESE_QUERY_CACHE_BYTES=0 "$bin" > $BENCH/count-probe-server.log 2>&1 &
pid=$!
until curl -sf http://127.0.0.1:18082/readyz >/dev/null; do sleep 0.2; done
q='SELECT (COUNT(*) AS ?n) WHERE { ?s ?p ?o }'
for i in 1 2 3; do
  curl -s -o /dev/null -w "count $i: %{time_total} s\n" --data-urlencode "query=$q" \
    -H "Accept: application/sparql-results+json" http://127.0.0.1:18082/dataset/query
done
kill $pid; wait $pid 2>/dev/null
grep -iE "warn|error|took|ms" $BENCH/count-probe-server.log | tail -15
