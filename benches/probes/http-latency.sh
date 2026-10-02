#!/usr/bin/env bash
# Per-request latency of the server for tiny queries (LUBM-1 with OWL 2 RL): curl in a loop
# (connection reuse off: what one-shot clients see) and with keep-alive (one curl, many URLs).
#
# Usage: benches/probes/http-latency.sh   (Linux; needs $SCRATCH/lubm/{univ-bench,lubm-1}.nt and a release nrese-server)
set -u
source "$(dirname "$0")/common.sh"
cd "$REPO"
bin=target/release/nrese-server
store=$SCRATCH/store-lat
rm -rf "$store"
export NRESE_STORE_MODE=on-disk NRESE_DATA_DIR="$store" NRESE_REASONING_MODE=owl2-rl RUST_LOG=warn
"$bin" load $SCRATCH/lubm/univ-bench.nt $SCRATCH/lubm/lubm-1.nt
NRESE_BIND_ADDR=127.0.0.1:18081 NRESE_QUERY_CACHE_BYTES=0 NRESE_READ_REQUESTS_PER_WINDOW=100000000 "$bin" &
pid=$!
until curl -sf http://127.0.0.1:18081/readyz >/dev/null; do sleep 0.3; done
q='ASK { ?s ?p ?o }'
enc=$(python3 -c "import urllib.parse,sys; print(urllib.parse.quote(sys.argv[1]))" "$q")
url="http://127.0.0.1:18081/dataset/query?query=$enc"
# Warm up, then 500 one-shot requests.
for i in $(seq 50); do curl -s -o /dev/null "$url"; done
start=$(date +%s.%N)
for i in $(seq 500); do curl -s -o /dev/null "$url"; done
end=$(date +%s.%N)
echo "one-shot curl: $(echo "($end - $start) * 1000 / 500" | bc -l | cut -c1-6) ms per request (curl process start included)"
# Keep-alive: one curl, 2000 URLs.
args=()
for i in $(seq 2000); do args+=("$url" -o /dev/null); done
start=$(date +%s.%N)
curl -s "${args[@]}"
end=$(date +%s.%N)
echo "keep-alive: $(echo "($end - $start) * 1000 / 2000" | bc -l | cut -c1-6) ms per request"
# LUBM q1 (4 rows) the same way.
q1=$(cat benches/reasoning/queries/lubm/q01.rq | tr '\n' ' ')
enc=$(python3 -c "import urllib.parse,sys; print(urllib.parse.quote(sys.argv[1]))" "$q1")
url="http://127.0.0.1:18081/dataset/query?query=$enc"
args=()
for i in $(seq 2000); do args+=("$url" -o /dev/null); done
start=$(date +%s.%N)
curl -s -H "Accept: application/sparql-results+json" "${args[@]}"
end=$(date +%s.%N)
echo "keep-alive LUBM q1: $(echo "($end - $start) * 1000 / 2000" | bc -l | cut -c1-6) ms per request"
kill $pid; wait $pid 2>/dev/null
rm -rf "$store"
