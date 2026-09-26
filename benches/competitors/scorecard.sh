#!/usr/bin/env bash
# Pf0 scorecard: load, store size, peak memory, restart and query mix per system and
# dataset. Everything runs in Docker on the same host.
#
#   benches/competitors/scorecard.sh <dataset> [systems...]
#   LOAD_ONLY=1 benches/competitors/scorecard.sh <dataset> [systems...]
#
# <dataset> names /data/<dataset>.nt in the volume `nrese-bench-data`; see
# prepare-datasets.sh, or `entities-<triples>` from the harness `generate` command. The query
# mix is queries/<dataset>/*.rq (queries/entities/ for any entities-* dataset).
#
# For each system:
#   1. load into a fresh named volume, measuring wall time and peak container memory
#   2. store size = bytes in the volume
#   3. restart = start the server on the volume until it answers ASK {}
#   4. query mix = harness `query-mix` (1 warm-up, 5 measured runs per query)
#   5. stop the server and remove the volume
#
# Output:
#   - one CSV line per system on stdout
#   - query reports, logs and the CSV under benches/competitors/results/ (git-ignored:
#     GraphDB, RDFox and AnzoGraph results may not be published; see README.md)
#   - a cross-check of result counts across systems at the end
#
# Fairness settings that differ from vendor defaults (all documented in README.md):
#   - QLever's query-result cache is capped at 1 MB, so repeated runs measure evaluation
#     rather than cache hits
#   - Virtuoso's result-row cap and cost-based query rejection are disabled
#   - NRESE's rate limits are lifted
set -euo pipefail
export MSYS_NO_PATHCONV=1

DATASET=${1:?usage: $0 <dataset> [systems...]}
shift
native() { cygpath -m "$1" 2>/dev/null || echo "$1"; }
ROOT=$(native "$(cd "$(dirname "$0")/../.." && pwd)")
RESULTS=$(native "${RESULTS:-$ROOT/benches/competitors/results/$(date +%Y-%m-%d)}")
QUERY_TIMEOUT_S=${QUERY_TIMEOUT_S:-120}
QUERIES=$ROOT/benches/competitors/queries/${DATASET%%-[0-9]*}
HARNESS=$ROOT/benches/nrese-bench-harness/target/release/nrese-bench-harness
FILE=/data/$DATASET.nt
GRAPHDB_IMAGE=${GRAPHDB_IMAGE:-ontotext/graphdb:11.5.1}
QLEVER_IMAGE=${QLEVER_IMAGE:-adfreiburg/qlever:latest}
OXIGRAPH_IMAGE=${OXIGRAPH_IMAGE:-ghcr.io/oxigraph/oxigraph:0.5.11}
VIRTUOSO_IMAGE=${VIRTUOSO_IMAGE:-openlink/virtuoso-opensource-7:7.2.17}
JENA_IMAGE=${JENA_IMAGE:-nrese-bench/jena:6.2.0}
RUST_IMAGE=${RUST_IMAGE:-rust:1.91-bookworm}
JAVA_HEAP=${JAVA_HEAP:-16g}
PORT=${PORT:-18900}
SYSTEMS=${*:-nrese qlever oxigraph jena virtuoso graphdb}
mkdir -p "$RESULTS"

ms() { date +%s%N | cut -c1-13; }

# Parses docker's "1.23GiB / 31GiB" into MiB.
mem_mib() {
  awk '{ v=$1; u=v; gsub(/[0-9.]/, "", u); gsub(/[A-Za-z]/, "", v);
         f = (u=="GiB") ? 1024 : (u=="MiB") ? 1 : (u=="KiB") ? 1/1024 : (u=="B") ? 1/1048576 : 0;
         printf "%d", v * f }'
}

# Streams a container's memory usage (about once per second) into a file until killed.
watch_memory() { docker stats --format '{{.MemUsage}}' "$1" >"$2" 2>/dev/null & echo $!; }
# The peak of a watch_memory file, in MiB (strips docker's terminal control codes).
peak_mib() {
  sed $'s/\x1b\[[0-9;]*[A-Za-z]//g' "$1" | tr -d '\r' | grep -o '^[0-9.]*[KMG]\?i\?B' \
    | mem_mib_lines | sort -n | tail -1
}
mem_mib_lines() { while read -r v; do echo "$v" | mem_mib; echo; done; }

# Runs a detached container to completion. Prints "<wall_ms> <peak_mib>"; logs to $2.
# Usage: measured <name> <log> <docker run args...>
measured() {
  local name=$1 log=$2 start rc watcher
  shift 2
  docker rm -f "$name" >/dev/null 2>&1 || true
  start=$(ms)
  docker run -d --name "$name" "$@" >/dev/null
  watcher=$(watch_memory "$name" "$log.mem")
  docker wait "$name" >/dev/null
  local wall=$(( $(ms) - start ))
  kill "$watcher" 2>/dev/null || true
  local peak
  peak=$(peak_mib "$log.mem")
  rc=$(docker inspect -f '{{.State.ExitCode}}' "$name")
  docker logs "$name" >"$log" 2>&1
  docker rm "$name" >/dev/null
  [ "$rc" = 0 ] || { echo "load failed ($rc), see $log" >&2; return 1; }
  echo "$wall ${peak:-0}"
}

# Starts a server and waits until its endpoint answers ASK {}; sets RESTART_MS.
# Usage: serve <name> <endpoint> <docker run args...>
serve() {
  local name=$1 endpoint=$2 start
  shift 2
  docker rm -f "$name" >/dev/null 2>&1 || true
  start=$(ms)
  docker run -d --name "$name" "$@" >/dev/null
  until [ "$(curl -s -o /dev/null -w '%{http_code}' --data-urlencode 'query=ASK {}' "$endpoint")" = 200 ]; do
    if [ "$(docker inspect -f '{{.State.Running}}' "$name" 2>/dev/null)" != true ]; then
      docker logs "$name" >&2 2>&1 | tail -20
      return 1
    fi
    [ $(( $(ms) - start )) -gt 1800000 ] && { echo "server did not start" >&2; return 1; }
    sleep 0.2
  done
  RESTART_MS=$(( $(ms) - start ))
}

server_mib() { docker stats --no-stream --format '{{.MemUsage}}' "$1" | mem_mib; }
volume_bytes() { docker run --rm -v "$1:/v" alpine du -sb /v | cut -f1; }

# --- systems: load_<s> <volume> <log> prints "wall_ms peak_mib"; serve_<s> <volume> starts
# the server and sets ENDPOINT and RESTART_MS (call it directly, not in a subshell). ---

load_nrese() {
  measured nrese-sc-load "$2" -v nrese-target:/target:ro -v nrese-bench-data:/data:ro -v "$1:/store" \
    -e NRESE_STORE_MODE=on-disk -e NRESE_DATA_DIR=/store -e RUST_LOG=info "$RUST_IMAGE" \
    /target/release/nrese-server load "$FILE"
}
serve_nrese() {
  ENDPOINT=http://localhost:$PORT/dataset/query
  serve nrese-sc "$ENDPOINT" -p "$PORT:8080" -v nrese-target:/target:ro -v "$1:/store" \
    -e NRESE_STORE_MODE=on-disk -e NRESE_DATA_DIR=/store -e NRESE_BIND_ADDR=0.0.0.0:8080 \
    -e NRESE_QUERY_TIMEOUT_MS=$(( QUERY_TIMEOUT_S * 1000 )) -e NRESE_READ_REQUESTS_PER_WINDOW=100000000 \
    "$RUST_IMAGE" /target/release/nrese-server
}

load_qlever() {
  measured qlever-sc-load "$2" -u root -v nrese-bench-data:/data:ro -v "$1:/index" -w /index \
    --entrypoint /qlever/qlever-index "$QLEVER_IMAGE" \
    -i /index/idx -f "$FILE" -F nt -p true --stxxl-memory 8G
}
serve_qlever() {
  ENDPOINT=http://localhost:$PORT/
  serve qlever-sc "$ENDPOINT" -u root -p "$PORT:7001" -v "$1:/index" -w /index \
    --entrypoint /qlever/qlever-server "$QLEVER_IMAGE" \
    -i /index/idx -p 7001 -n -j 8 -m 16G -c 1MB -e 1MB -s "${QUERY_TIMEOUT_S}s"
}

load_oxigraph() {
  measured oxigraph-sc-load "$2" -v nrese-bench-data:/data:ro -v "$1:/store" "$OXIGRAPH_IMAGE" \
    load --location /store --file "$FILE"
}
serve_oxigraph() {
  ENDPOINT=http://localhost:$PORT/query
  serve oxigraph-sc "$ENDPOINT" -p "$PORT:7878" -v "$1:/store" "$OXIGRAPH_IMAGE" \
    serve-read-only --location /store --bind 0.0.0.0:7878
}

load_jena() {
  measured jena-sc-load "$2" -e JVM_ARGS="-Xmx$JAVA_HEAP" -v nrese-bench-data:/data:ro -v "$1:/tdb2" \
    "$JENA_IMAGE" tdb2.xloader --loc /tdb2/db --tmpdir /tdb2/tmp --threads 8 "$FILE"
}
serve_jena() {
  ENDPOINT=http://localhost:$PORT/ds/query
  serve jena-sc "$ENDPOINT" -p "$PORT:3030" -e JVM_ARGS="-Xmx$JAVA_HEAP" -v "$1:/tdb2" \
    "$JENA_IMAGE" fuseki-server --tdb2 --loc=/tdb2/db --port 3030 /ds
}

VIRTUOSO_ENV=(-e DBA_PASSWORD=bench
  -e "VIRT_PARAMETERS_DIRSALLOWED=., /data, ../vad, /usr/share/proj"
  -e VIRT_PARAMETERS_NUMBEROFBUFFERS=1360000 -e VIRT_PARAMETERS_MAXDIRTYBUFFERS=1000000
  -e VIRT_SPARQL_RESULTSETMAXROWS=1000000000 -e VIRT_SPARQL_MAXQUERYEXECUTIONTIME=0
  -e VIRT_SPARQL_MAXQUERYCOSTESTIMATIONTIME=0)
load_virtuoso() {
  # Virtuoso loads through its running server: time ld_dir + 6 loaders + checkpoint.
  local name=virtuoso-sc-load start peak pid
  docker rm -f "$name" >/dev/null 2>&1 || true
  docker run -d --name "$name" "${VIRTUOSO_ENV[@]}" -v nrese-bench-data:/data:ro -v "$1:/database" \
    "$VIRTUOSO_IMAGE" >/dev/null
  until docker logs "$name" 2>&1 | grep -q "Server online at 1111"; do sleep 0.2; done
  isql() { docker exec "$name" isql 1111 dba bench exec="$1"; }
  start=$(ms)
  (
    isql "ld_dir('/data', '$(basename "$FILE")', 'http://bench');"
    for _ in 1 2 3 4 5 6; do isql "rdf_loader_run();" & done
    wait
    isql "checkpoint;"
  ) >"$2" 2>&1 &
  pid=$!
  local watcher
  watcher=$(watch_memory "$name" "$2.mem")
  wait "$pid"
  local wall=$(( $(ms) - start ))
  kill "$watcher" 2>/dev/null || true
  peak=$(peak_mib "$2.mem")
  docker stop -t 60 "$name" >/dev/null && docker rm "$name" >/dev/null
  echo "$wall $peak"
}
serve_virtuoso() {
  ENDPOINT=http://localhost:$PORT/sparql
  serve virtuoso-sc "$ENDPOINT" -p "$PORT:8890" "${VIRTUOSO_ENV[@]}" -v "$1:/database" "$VIRTUOSO_IMAGE"
}

load_graphdb() {
  measured graphdb-sc-load "$2" -e GDB_HEAP_SIZE="$JAVA_HEAP" -v nrese-bench-data:/data:ro \
    -v "$(native "$ROOT/benches/competitors/graphdb"):/config:ro" -v "$1:/opt/graphdb/home" \
    --entrypoint /opt/graphdb/dist/bin/importrdf "$GRAPHDB_IMAGE" \
    preload -f -c /config/repo-empty.ttl "$FILE"
}
serve_graphdb() {
  ENDPOINT=http://localhost:$PORT/repositories/bench
  serve graphdb-sc "$ENDPOINT" -p "$PORT:7200" -e GDB_HEAP_SIZE="$JAVA_HEAP" \
    -v "$1:/opt/graphdb/home" "$GRAPHDB_IMAGE"
}

# --- run -----------------------------------------------------------------------------------

docker run --rm -v nrese-bench-data:/data alpine test -s "$FILE" || {
  echo "missing $FILE: run prepare-datasets.sh or the harness generate command" >&2
  exit 1
}
TRIPLES=$(docker run --rm -v nrese-bench-data:/data alpine sh -c "wc -l < $FILE")
if [ -z "${SKIP_BUILD:-}" ]; then
  docker volume create nrese-target >/dev/null
  docker run --rm -v "$ROOT:/src:ro" -v nrese-target:/target -w /src "$RUST_IMAGE" \
    cargo build --release --locked -p nrese-server --target-dir /target >"$RESULTS/nrese-build.log" 2>&1
  docker build -q -t "$JENA_IMAGE" "$ROOT/benches/competitors/jena" >/dev/null
  cargo build --release --quiet --manifest-path "$ROOT/benches/nrese-bench-harness/Cargo.toml"
fi

CSV=$RESULTS/scorecard-$DATASET.csv
[ -s "$CSV" ] || echo "system,dataset,triples,load_ms,load_peak_mib,store_bytes,bytes_per_triple,restart_ms,serve_mib,queries_ok,queries_total,sum_p50_ms" >"$CSV"
head -1 "$CSV"
for system in $SYSTEMS; do
  volume=sc-$system-$DATASET
  # Leftovers of an interrupted run would hold the volume (and its lock).
  docker rm -f "$system-sc" "$system-sc-load" >/dev/null 2>&1 || true
  docker volume rm -f "$volume" >/dev/null 2>&1 || true
  docker volume create "$volume" >/dev/null
  prefix=$RESULTS/$system-$DATASET
  if ! read -r load_ms peak < <("load_$system" "$volume" "$prefix-load.log"); then
    echo "$system,$DATASET,$TRIPLES,FAILED" | tee -a "$CSV"
    docker volume rm -f "$volume" >/dev/null
    continue
  fi
  bytes=$(volume_bytes "$volume")
  line="$system,$DATASET,$TRIPLES,$load_ms,$peak,$bytes,$(( bytes / (TRIPLES > 0 ? TRIPLES : 1) ))"
  if [ -n "${LOAD_ONLY:-}" ]; then
    echo "$line,,,,," | tee -a "$CSV"
  else
    if ! "serve_$system" "$volume"; then
      echo "$line,FAILED" | tee -a "$CSV"
      docker rm -f "$system-sc" >/dev/null 2>&1 || true
      docker volume rm -f "$volume" >/dev/null
      continue
    fi
    restart=$RESTART_MS
    "$(native "$HARNESS")" query-mix --endpoint "$ENDPOINT" --queries "$QUERIES" --label "$system" \
      --warmup 1 --runs 5 --timeout-s "$QUERY_TIMEOUT_S" --report-json "$prefix-queries.json"       >"$prefix-queries.log" 2>&1 || true
    rss=$(server_mib "$system-sc")
    summary=$(python -c "
import json, sys
try:
    r = json.load(open(sys.argv[1]))
except OSError:
    print('0,0,')
    sys.exit()
ok = [q for q in r['queries'] if not q['error']]
print(f\"{len(ok)},{len(r['queries'])},{sum(q['p50_ms'] for q in ok):.1f}\")" "$prefix-queries.json")
    docker rm -f "$system-sc" >/dev/null
    echo "$line,$restart,$rss,$summary" | tee -a "$CSV"
  fi
  docker volume rm -f "$volume" >/dev/null
done

# Cross-check: every system that answered a query should report the same number of rows.
[ -n "${LOAD_ONLY:-}" ] || python - "$(native "$RESULTS")" "$DATASET" <<'EOF'
import glob, json, os, sys
results, dataset = sys.argv[1], sys.argv[2]
rows = {}
for path in glob.glob(os.path.join(results, f"*-{dataset}-queries.json")):
    report = json.load(open(path))
    for q in report["queries"]:
        if not q["error"]:
            rows.setdefault(q["id"], {})[report["label"]] = q["rows"]
disagreements = {q: r for q, r in rows.items() if len(set(r.values())) > 1}
print("result-count cross-check:", "all systems agree" if not disagreements else "")
for q, r in sorted(disagreements.items()):
    print(f"  {q}: {r}")
EOF
