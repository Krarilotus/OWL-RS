#!/usr/bin/env bash
# The RG/GS/GND integration workload (README.md) on one system, without containers, so it
# runs the same on a workstation and on a cluster node: load and reason, serve, then the
# project's competency questions through one client (the harness's `query-mix`).
#
#   RGGS_REPO=~/rgonline-gs-data-integration benches/integration/run.sh <system> <tier>
#
#   system  nrese          NRESE, OWL 2 RL materialised at load
#           nrese-plain    NRESE without reasoning: what the workload costs without it
#           fuseki-owl     Fuseki as the project configures it: Jena's OWL rule reasoner
#                          over an in-memory model (the project's fuseki-config.ttl)
#   tier    example        the linked example persons committed in the project's repository
#           current        data/harmonized/statements.ttl as the project's pipeline last
#                          built it (`just use-cohort` or `just use-full`, then
#                          `just harmonize`); name it with TIER_NAME=cohort|full
#           <file>         any file of harmonized statements (Turtle or N-Triples)
#
#   ONTOLOGY         project (default): the axioms the project's merge contains
#                    (mappings/harmonize.ttl); gndo: also the GND ontology and the RG
#                    vocabulary (mappings/gndo.ttl, mappings/rgo/tbox.ttl)
#   QUERY_TIMEOUT_S  per query run (default 300, the project's QLever setting)
#   RUNS             measured runs per query after one warm-up run (default 3)
#   QUERY_MEMORY_MIB NRESE's memory budget per query (default 4096; 0 = unlimited)
#   FUSEKI_HOME      a directory with fuseki-server.jar (default: what
#                    benches/suite/install-tools.sh installed, else the project's fuseki/)
#   FUSEKI_HEAP      JVM heap (default 64g, the project's setting)
#   JAVA             the java command (default java); on a cluster without Java:
#                    JAVA="apptainer exec temurin-21.sif java"
#   RESULTS          where results go (default benches/integration/results/<date>)
#   NRESE_BENCH_SCRATCH  where the store and temporary files go (default tmp/ in this
#                    repository); removed when the run ends
#
# Output: one line per run in $RESULTS/integration.csv, and per run a directory with the
# logs and the query report (queries.json). The store and everything temporary is removed
# when the run ends, also when it fails.
set -euo pipefail

HERE=$(cd "$(dirname "$0")" && pwd)
ROOT=$(cd "$HERE/../.." && pwd)
# shellcheck source=../../scripts/lib/disk.sh
. "$ROOT/scripts/lib/disk.sh"

usage() { sed -n '2,/^# when the run ends, also/p' "$0" | sed 's/^# \{0,1\}//'; exit 2; }
[ $# -eq 2 ] || usage
SYSTEM=$1
TIER=$2
REPO=${RGGS_REPO:?set RGGS_REPO to a checkout of https://github.com/HisQu/rgonline-gs-data-integration}
REPO=$(cd "$REPO" && pwd)
[ -d "$REPO/queries/cq" ] || { echo "no queries/cq in $REPO" >&2; exit 2; }
ONTOLOGY=${ONTOLOGY:-project}
QUERY_TIMEOUT_S=${QUERY_TIMEOUT_S:-300}
RUNS=${RUNS:-3}
QUERY_MEMORY_MIB=${QUERY_MEMORY_MIB:-4096}
PORT=${PORT:-10230}
RESULTS=${RESULTS:-$HERE/results/$(date +%Y-%m-%d)}
SCRATCH=${NRESE_BENCH_SCRATCH:-$ROOT/tmp}
WORK=$SCRATCH/bench-run-integration-$$
EXE=
case "$(uname -s)" in MINGW* | MSYS* | CYGWIN*) EXE=.exe ;; esac

# --- inputs ---------------------------------------------------------------------------
INPUTS=()
case $TIER in
  example)
    # The focused exports of the linked persons (their HermiT-reasoned copies are a
    # reference, not an input), the three sources' example extracts, and the alignment.
    for file in "$REPO"/data/examples/harmonized/*.ttl; do
      case $file in *.reasoned.ttl) ;; *) INPUTS+=("$file") ;; esac
    done
    INPUTS+=("$REPO"/data/raw/dnb/example_min.ttl "$REPO"/data/raw/gs/example_min.ttl
      "$REPO"/data/raw/rgo/example_min.ttl "$REPO"/mappings/harmonize.ttl)
    ;;
  current) INPUTS=("$REPO/data/harmonized/statements.ttl") ;;
  *) INPUTS=("$(cd "$(dirname "$TIER")" && pwd)/$(basename "$TIER")") ;;
esac
TIER_NAME=${TIER_NAME:-$(basename "${TIER%.*}")}
case $ONTOLOGY in
  project) ;;
  gndo) INPUTS+=("$REPO/mappings/gndo.ttl" "$REPO/mappings/rgo/tbox.ttl") ;;
  *) echo "unknown ONTOLOGY $ONTOLOGY (project | gndo)" >&2; exit 2 ;;
esac
for file in "${INPUTS[@]}"; do
  [ -s "$file" ] || { echo "input missing or empty: $file" >&2; exit 2; }
done

# --- helpers --------------------------------------------------------------------------
SERVER_PID=
cleanup() {
  [ -z "$SERVER_PID" ] || kill "$SERVER_PID" 2>/dev/null || true
  [ -z "$SERVER_PID" ] || wait "$SERVER_PID" 2>/dev/null || true
  rm -rf "$WORK"
}
trap cleanup EXIT
require_free_gb "${NRESE_MIN_FREE_GB:-20}" "$(basename "$0")" "$SCRATCH"
mkdir -p "$WORK"
RUN=$RESULTS/$SYSTEM-$TIER_NAME-$ONTOLOGY
mkdir -p "$RUN"

now() { date +%s.%N; }
since() { awk -v a="$1" -v b="$(now)" 'BEGIN { printf "%.2f", b - a }'; }
# The peak resident memory of a running process in MiB (Linux; "-" elsewhere).
peak_mib() {
  awk '/^VmHWM:/ { printf "%d", $2 / 1024; found = 1 } END { if (!found) printf "-" }' \
    "/proc/$1/status" 2>/dev/null || printf -- "-"
}
# timed <log> <command...>: runs the command; sets ELAPSED_S and PEAK_MIB (GNU time's
# maximum resident set size where /usr/bin/time exists).
timed() {
  local log=$1 started
  shift
  started=$(now)
  if [ -x /usr/bin/time ]; then
    /usr/bin/time -v -o "$WORK/time" "$@" >"$log" 2>&1
    PEAK_MIB=$(awk '/Maximum resident set size/ { printf "%d", $NF / 1024 }' "$WORK/time")
  else
    "$@" >"$log" 2>&1
    PEAK_MIB=-
  fi
  ELAPSED_S=$(since "$started")
}
wait_for() { # <url> <seconds>
  local deadline=$(($(date +%s) + $2))
  until curl -sf -o /dev/null "$1"; do
    kill -0 "$SERVER_PID" 2>/dev/null || { echo "the server stopped; see $RUN/server.log" >&2; return 1; }
    [ "$(date +%s)" -lt "$deadline" ] || { echo "no answer from $1 after $2 s" >&2; return 1; }
    sleep 1
  done
}
built() { # <binary name> <cargo arguments...>: the path of a release binary, built if missing
  local binary=${CARGO_TARGET_DIR:-$ROOT/target}/release/$1$EXE
  shift
  # From the repository root: that is where cargo finds the shared build directory.
  [ -x "$binary" ] || (cd "$ROOT" && scripts/cargo-guarded.sh build --release --locked --quiet "$@" >&2)
  printf '%s' "$binary"
}
# count_all <endpoint>: every statement the endpoint answers with (for a reasoning store:
# asserted and inferred). Sets STATEMENTS and FIRST_ANSWER_S; a lazy reasoner does its
# work here.
count_all() {
  local started
  started=$(now)
  STATEMENTS=$(curl -sS --max-time "${PREPARE_TIMEOUT_S:-86400}" -H 'Accept: text/csv' \
    --data-urlencode 'query=SELECT (COUNT(*) AS ?n) WHERE { ?s ?p ?o }' "$1" | tr -d '\r' | sed -n 2p) ||
    STATEMENTS=
  FIRST_ANSWER_S=$(since "$started")
}

# --- systems --------------------------------------------------------------------------
LOAD_S=- LOAD_PEAK_MIB=- ASSERTED=- INFERRED=- VERSION=-
case $SYSTEM in
  nrese | nrese-plain)
    NRESE=${NRESE_BIN:-$(built nrese-server -p nrese-server)}
    reasoning=owl2-rl
    [ "$SYSTEM" = nrese ] || reasoning=disabled
    export NRESE_STORE_MODE=on-disk NRESE_DATA_DIR=$WORK/store NRESE_REASONING_MODE=$reasoning
    export NRESE_BIND_ADDR=127.0.0.1:$PORT RUST_LOG=info
    export NRESE_QUERY_TIMEOUT_MS=$((QUERY_TIMEOUT_S * 1000))
    export NRESE_MAX_QUERY_MEMORY_BYTES=$((QUERY_MEMORY_MIB * 1024 * 1024))
    # No rate limits and no result cache: repeated runs measure evaluation.
    export NRESE_READ_REQUESTS_PER_WINDOW=100000000 NRESE_QUERY_CACHE_BYTES=0
    timed "$RUN/load.log" "$NRESE" load "${INPUTS[@]}"
    LOAD_S=$ELAPSED_S LOAD_PEAK_MIB=$PEAK_MIB
    plain() { sed 's/\x1b\[[0-9;]*m//g' "$RUN/load.log"; } # the log without colour codes
    ASSERTED=$(plain | sed -n 's/.*bulk load complete.* inserted=\([0-9]*\).*/\1/p' | tail -1)
    INFERRED=$(plain | sed -n 's/.*inferred stack rematerialised.* inferred=\([0-9]*\).*/\1/p' | tail -1)
    "$NRESE" >"$RUN/server.log" 2>&1 &
    SERVER_PID=$!
    wait_for "http://127.0.0.1:$PORT/readyz" 600
    VERSION=$(curl -s "http://127.0.0.1:$PORT/version" | sed -n 's/.*"version" *: *"\([^"]*\)".*/\1/p' | head -1)
    ENDPOINT=http://127.0.0.1:$PORT/dataset/sparql
    ;;
  fuseki-owl)
    JAVA=${JAVA:-java}
    FUSEKI_HOME=${FUSEKI_HOME:-$(ls -d "$ROOT"/.cache/tools/apache-jena-fuseki-* "$REPO"/fuseki/apache-jena-fuseki-* 2>/dev/null | head -1)}
    [ -f "${FUSEKI_HOME:-/nonexistent}/fuseki-server.jar" ] || {
      echo "no fuseki-server.jar: run benches/suite/install-tools.sh, or set FUSEKI_HOME" >&2
      exit 2
    }
    VERSION=$(basename "$FUSEKI_HOME")
    # The project's configuration, with its data file replaced by this run's inputs.
    content=
    for file in "${INPUTS[@]}"; do content="$content${content:+ , }<file://$file>"; done
    sed "s|<file:data/harmonized/statements.ttl>|$content|" "$REPO/fuseki-config.ttl" >"$WORK/fuseki-config.ttl"
    grep -q "file://" "$WORK/fuseki-config.ttl" || { echo "the project's fuseki-config.ttl no longer names data/harmonized/statements.ttl" >&2; exit 2; }
    cp "$WORK/fuseki-config.ttl" "$RUN/fuseki-config.ttl"
    started=$(now)
    # shellcheck disable=SC2086
    (cd "$WORK" && exec $JAVA "-Xmx${FUSEKI_HEAP:-64g}" -jar "$FUSEKI_HOME/fuseki-server.jar" \
      --config="$WORK/fuseki-config.ttl" --port="$PORT") >"$RUN/server.log" 2>&1 &
    SERVER_PID=$!
    wait_for "http://127.0.0.1:$PORT/\$/ping" 3600
    LOAD_S=$(since "$started")
    ENDPOINT=http://127.0.0.1:$PORT/integration/sparql
    ;;
  *) echo "unknown system $SYSTEM" >&2; usage ;;
esac

# --- measure --------------------------------------------------------------------------
count_all "$ENDPOINT"
HARNESS=${HARNESS:-$(built nrese-bench-harness --manifest-path "$ROOT/benches/nrese-bench-harness/Cargo.toml")}
"$HARNESS" query-mix --endpoint "$ENDPOINT" --queries "$REPO/queries/cq" --label "$SYSTEM" \
  --warmup 1 --runs "$RUNS" --timeout-s "$QUERY_TIMEOUT_S" \
  --report-json "$RUN/queries.json" | tee "$RUN/queries.txt"
SERVE_PEAK_MIB=$(peak_mib "$SERVER_PID")
answered=$(grep -c ' rows ' "$RUN/queries.txt" || true)
failed=$(grep -c ' ERROR ' "$RUN/queries.txt" || true)
sum_p50=$(awk '/ rows / { for (i = 1; i <= NF; i++) if ($i == "p50") sum += $(i + 1) } END { printf "%.1f", sum }' "$RUN/queries.txt")

CSV=$RESULTS/integration.csv
[ -s "$CSV" ] || echo "date,host,tier,ontology,system,version,inputs,load_s,load_peak_mib,asserted,inferred,statements_answered,first_answer_s,serve_peak_mib,queries_answered,queries_failed,sum_p50_ms" >"$CSV"
echo "$(date +%Y-%m-%dT%H:%M),$(hostname),$TIER_NAME,$ONTOLOGY,$SYSTEM,$VERSION,${#INPUTS[@]},$LOAD_S,$LOAD_PEAK_MIB,${ASSERTED:--},${INFERRED:--},${STATEMENTS:--},$FIRST_ANSWER_S,$SERVE_PEAK_MIB,$answered,$failed,$sum_p50" | tee -a "$CSV"
