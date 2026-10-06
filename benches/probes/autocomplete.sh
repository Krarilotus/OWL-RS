#!/usr/bin/env bash
# Autocompletion latency on DBpedia core over HTTP: the first request builds the indexes.
#
# Usage: STORE=<on-disk store dir> benches/probes/autocomplete.sh   (Linux; DBpedia core in the store)
set -u
source "$(dirname "$0")/common.sh"
cd "$REPO"
nice scripts/cargo-guarded.sh build --release -p nrese-server 2>&1 | tail -1
bin=target/release/nrese-server
export NRESE_STORE_MODE=on-disk NRESE_DATA_DIR="${STORE:-$SCRATCH/store-d}" RUST_LOG=warn
NRESE_BIND_ADDR=127.0.0.1:18083 "$bin" > $BENCH/autocomplete-server.log 2>&1 &
pid=$!
until curl -sf http://127.0.0.1:18083/readyz >/dev/null; do sleep 0.2; done
for q in "albert ein" "berl" "einstein" "quantum mech" "zzzq"; do
  for i in 1 2 3; do
    curl -s -o /tmp/ac.json -w "q=\"$q\" run $i: %{time_total} s\n" -G --data-urlencode "q=$q" \
      http://127.0.0.1:18083/dataset/autocomplete
  done
  head -c 300 /tmp/ac.json; echo
done
echo "server rss: $(awk '/VmRSS/ {print int($2/1024)}' /proc/$pid/status) MiB"
kill $pid; wait $pid 2>/dev/null
rm -f /tmp/ac.json $BENCH/autocomplete-server.log
