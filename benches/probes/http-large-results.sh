#!/usr/bin/env bash
# Times large results over HTTP: LUBM-100 with OWL 2 RL in an on-disk store, the server
# without result cache, q06 and q14 as JSON and TSV with curl (transfer only, no parsing).
#
# Usage: benches/probes/http-large-results.sh   (Linux; needs $SCRATCH/lubm/{univ-bench,lubm-100}.nt)
set -u
source "$(dirname "$0")/common.sh"
cd "$REPO"
scripts/cargo-guarded.sh build --release -p nrese-server 2>&1 | grep -E "^error|Finished"
bin=target/release/nrese-server
store=$SCRATCH/store-http
rm -rf "$store"
export NRESE_STORE_MODE=on-disk NRESE_DATA_DIR="$store" NRESE_REASONING_MODE=owl2-rl RUST_LOG=warn
/usr/bin/time -f "load %e s, peak %M KiB" "$bin" load $SCRATCH/lubm/univ-bench.nt $SCRATCH/lubm/lubm-100.nt
NRESE_BIND_ADDR=127.0.0.1:18080 NRESE_QUERY_CACHE_BYTES=0 NRESE_READ_REQUESTS_PER_WINDOW=100000000 "$bin" &
pid=$!
until curl -sf http://127.0.0.1:18080/readyz >/dev/null; do sleep 0.5; done
for q in q06 q14; do
  for accept in "application/sparql-results+json" "text/tab-separated-values"; do
    for run in 1 2 3; do
      curl -s -o /dev/null -w "$q $accept run $run: %{time_total} s, %{size_download} bytes, first byte %{time_starttransfer} s\n" \
        -H "Accept: $accept" --data-urlencode "query@benches/reasoning/queries/lubm/$q.rq" \
        http://127.0.0.1:18080/dataset/query
    done
  done
done
kill $pid; wait $pid 2>/dev/null
rm -rf "$store"
