#!/usr/bin/env bash
# BSBM (10 M) on NRESE over HTTP: load into an on-disk store, then the explore and the
# business-intelligence mixes with the BSBM test driver (one client; then 8 for explore).
#
# Usage: benches/probes/bsbm-http.sh   (Linux; $SCRATCH/bsbm with bsbm-10m.nt, td_data and bsbmtools-0.2; RUNS, BI_RUNS, NO_BUILD=1)
set -u
source "$(dirname "$0")/common.sh"
cd "$REPO"
[ "${NO_BUILD:-}" = 1 ] || scripts/cargo-guarded.sh build --release -p nrese-server 2>&1 | grep -E "^error|Finished"
bin=$PWD/target/release/nrese-server
bsbm=$SCRATCH/bsbm
store=$bsbm/store
rm -rf "$store"
export NRESE_STORE_MODE=on-disk NRESE_DATA_DIR="$store" RUST_LOG=warn
/usr/bin/time -f "load %e s, peak %M KiB" "$bin" load "$bsbm/bsbm-10m.nt"
NRESE_BIND_ADDR=127.0.0.1:18085 NRESE_QUERY_CACHE_BYTES=0 NRESE_READ_REQUESTS_PER_WINDOW=100000000 "$bin" > $bsbm/server.log 2>&1 &
pid=$!
until curl -sf http://127.0.0.1:18085/readyz >/dev/null; do sleep 0.5; done
cd $bsbm/bsbmtools-0.2
endpoint=http://127.0.0.1:18085/dataset/query
run() {
  label=$1; shift
  echo "=== $label"
  ./testdriver -idir ../td_data -t 60000 -o ../$label.xml "$@" $endpoint 2>&1 | grep -vE "^Thread|^[0-9]+: [0-9.]+ms" | tail -60
}
run explore-1 -runs ${RUNS:-500} -w 50 -ucf usecases/explore/sparql.txt
run bi-1 -runs ${BI_RUNS:-25} -w 5 -ucf usecases/businessIntelligence/sparql.txt
run explore-8 -runs ${RUNS:-500} -w 50 -mt 8 -ucf usecases/explore/sparql.txt
echo "server rss: $(awk '/VmRSS/ {print int($2/1024)}' /proc/$pid/status) MiB"
kill $pid; wait $pid 2>/dev/null
rm -rf "$store"
echo ALL-DONE
