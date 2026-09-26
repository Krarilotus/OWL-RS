#!/usr/bin/env bash
# Bulk-load comparison on the same file, all in Docker on the same host, so CPU, memory,
# storage and container overhead are identical.
#
#   benches/competitors/run-load-comparison.sh <triples> [runs] [systems...]
#
# - The input is the harness's entity dataset (`generate`), staged once into the Docker
#   volume `nrese-bench-data`. NRESE is built for Linux in Docker (volume `nrese-target`).
# - Every system writes its store to a fresh anonymous volume.
# - Each system times its own load step: from the start of the load to a durable,
#   queryable store. Server start-up (only Virtuoso has one) is excluded.
# - Output: `system,triples,run,wall_ms` lines on stdout; logs in $BENCH_DIR.
#
# Systems and their bulk paths (the closest equivalents; no reasoning anywhere):
#   nrese          nrese-server load (parallel parse, bulk index build, checkpoint)
#   qlever         qlever-index -p true (parallel parse, all permutations)
#   graphdb        GraphDB Free, importrdf preload, ruleset "empty"
#   jena           Apache Jena TDB2 (Fuseki's storage), tdb2.tdbloader --loader=parallel
#   jena-xloader   Apache Jena TDB2, tdb2.xloader (Jena's loader for large data)
#   oxigraph       oxigraph load (bulk loader, strict parsing)
#   virtuoso       Virtuoso Open Source 7: ld_dir + 6 parallel rdf_loader_run + checkpoint
#   rdfox          RDFox sandbox import (only with RDFOX_LICENSE=/path/RDFox.lic)
set -euo pipefail
export MSYS_NO_PATHCONV=1

TRIPLES=${1:?usage: $0 <triples> [runs] [systems...]}
RUNS=${2:-1}
shift $(( $# >= 2 ? 2 : 1 ))
# Native paths: on Windows (Git Bash), cargo and docker need C:/... rather than /c/...
native() { cygpath -m "$1" 2>/dev/null || echo "$1"; }
ROOT=$(native "$(cd "$(dirname "$0")/../.." && pwd)")
BENCH_DIR=$(native "${BENCH_DIR:-$HOME/nrese-bench}")
FILE=entities-$TRIPLES.nt
GRAPHDB_IMAGE=${GRAPHDB_IMAGE:-ontotext/graphdb:11.5.1}
QLEVER_IMAGE=${QLEVER_IMAGE:-adfreiburg/qlever:latest}
OXIGRAPH_IMAGE=${OXIGRAPH_IMAGE:-ghcr.io/oxigraph/oxigraph:0.5.11}
VIRTUOSO_IMAGE=${VIRTUOSO_IMAGE:-openlink/virtuoso-opensource-7:7.2.17}
JENA_IMAGE=${JENA_IMAGE:-nrese-bench/jena:6.2.0}
RDFOX_IMAGE=${RDFOX_IMAGE:-oxfordsemantic/rdfox:7.6b}
RDFOX_LICENSE=${RDFOX_LICENSE:-}
RUST_IMAGE=${RUST_IMAGE:-rust:1.91-bookworm}
JAVA_HEAP=${JAVA_HEAP:-16g}
mkdir -p "$BENCH_DIR"

ms() { date +%s%N | cut -c1-13; }

# Runs a command and prints its wall time in ms on fd 3 (stdout goes to the log).
timed() {
  local start
  start=$(ms)
  "$@"
  echo $(( $(ms) - start )) >&3
}

stage_data() {
  docker volume create nrese-bench-data >/dev/null
  if docker run --rm -v nrese-bench-data:/data alpine test -f "/data/$FILE"; then
    return
  fi
  cargo run --release --quiet --manifest-path "$ROOT/benches/nrese-bench-harness/Cargo.toml" -- \
    generate --triples "$TRIPLES" --out "$BENCH_DIR/$FILE"
  docker run --rm -v "$BENCH_DIR:/src:ro" -v nrese-bench-data:/data alpine cp "/src/$FILE" /data/
}

build_images() {
  docker volume create nrese-target >/dev/null
  docker run --rm -v "$ROOT:/src:ro" -v nrese-target:/target -w /src "$RUST_IMAGE" \
    cargo build --release --locked -p nrese-server --target-dir /target >"$BENCH_DIR/nrese-build.log" 2>&1
  docker build -q -t "$JENA_IMAGE" "$ROOT/benches/competitors/jena" >/dev/null
}

run_nrese() {
  timed docker run --rm -v nrese-target:/target:ro -v nrese-bench-data:/data:ro -v /store \
    -e NRESE_STORE_MODE=on-disk -e NRESE_DATA_DIR=/store -e RUST_LOG=info "$RUST_IMAGE" \
    /target/release/nrese-server load "/data/$FILE"
}

run_qlever() {
  timed docker run --rm -u root -v nrese-bench-data:/data:ro -v /index -w /index \
    --entrypoint /qlever/qlever-index "$QLEVER_IMAGE" \
    -i /index/entities -f "/data/$FILE" -F nt -p true --stxxl-memory 8G
}

run_graphdb() {
  timed docker run --rm -e GDB_HEAP_SIZE="$JAVA_HEAP" -v nrese-bench-data:/data:ro \
    -v "$ROOT/benches/competitors/graphdb:/config:ro" -v /opt/graphdb/home \
    --entrypoint /opt/graphdb/dist/bin/importrdf "$GRAPHDB_IMAGE" \
    preload -f -c /config/repo-empty.ttl "/data/$FILE"
}

run_jena() {
  timed docker run --rm -e JVM_ARGS="-Xmx$JAVA_HEAP" -v nrese-bench-data:/data:ro -v /tdb2 \
    "$JENA_IMAGE" tdb2.tdbloader --loader=parallel --loc /tdb2 "/data/$FILE"
}

run_jena-xloader() {
  timed docker run --rm -e JVM_ARGS="-Xmx$JAVA_HEAP" -v nrese-bench-data:/data:ro -v /tdb2 \
    "$JENA_IMAGE" tdb2.xloader --loc /tdb2/db --tmpdir /tdb2/tmp --threads 8 "/data/$FILE"
}

run_oxigraph() {
  timed docker run --rm -v nrese-bench-data:/data:ro -v /store "$OXIGRAPH_IMAGE" \
    load --location /store --file "/data/$FILE"
}

run_virtuoso() {
  local name=nrese-bench-virtuoso
  docker rm -f "$name" >/dev/null 2>&1 || true
  # Buffers sized for ~16 GB, per Virtuoso's RDF performance tuning guide.
  docker run -d --name "$name" -e DBA_PASSWORD=bench \
    -e VIRT_PARAMETERS_DIRSALLOWED=". , /data, ../vad, /usr/share/proj" \
    -e VIRT_PARAMETERS_NUMBEROFBUFFERS=1360000 -e VIRT_PARAMETERS_MAXDIRTYBUFFERS=1000000 \
    -v nrese-bench-data:/data:ro "$VIRTUOSO_IMAGE" >/dev/null
  until docker logs "$name" 2>&1 | grep -q "Server online at 1111"; do sleep 0.2; done
  isql() { docker exec "$name" isql 1111 dba bench exec="$1"; }
  load() {
    isql "ld_dir('/data', '$FILE', 'http://bench');"
    for _ in 1 2 3 4 5 6; do isql "rdf_loader_run();" & done
    wait
    isql "checkpoint;"
  }
  timed load
  isql "SPARQL SELECT COUNT(*) FROM <http://bench> WHERE { ?s ?p ?o };"
  docker rm -f "$name" >/dev/null
}

run_rdfox() {
  timed docker run --rm --cap-drop ALL -v "$(native "$RDFOX_LICENSE"):/opt/RDFox/RDFox.lic:ro" \
    -v nrese-bench-data:/data:ro "$RDFOX_IMAGE" \
    sandbox /data "dstore create bench" "active bench" "import /data/$FILE" "quit"
}

SYSTEMS=${*:-nrese qlever graphdb jena jena-xloader oxigraph virtuoso}
if [ -n "$RDFOX_LICENSE" ] && [ $# -eq 0 ]; then SYSTEMS="$SYSTEMS rdfox"; fi

stage_data
build_images
echo "system,triples,run,wall_ms"
for run in $(seq 1 "$RUNS"); do
  for system in $SYSTEMS; do
    log="$BENCH_DIR/$system-$TRIPLES-$run.log"
    if wall=$( { "run_$system" >"$log" 2>&1; } 3>&1 ); then
      echo "$system,$TRIPLES,$run,$wall"
    else
      echo "$system,$TRIPLES,$run,FAILED (see $log)"
    fi
  done
done
