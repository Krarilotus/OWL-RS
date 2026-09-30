#!/usr/bin/env bash
# Pf0-R reasoning scorecard, first stage: RT1 (materialisation) and RT3 (queries under
# entailment) on LUBM. See docs/design/reasoning-benchmark.md.
#
#   benches/reasoning/reasoning-scorecard.sh [datasets...]        (default: lubm-1 lubm-10)
#   SYSTEMS="nrese-v2 jena-owl-micro" benches/reasoning/reasoning-scorecard.sh lubm-100
#
# Input, from the volume `nrese-bench-data`:
#   lubm-<N>             /data/univ-bench.nt plus /data/lubm-<N>.nt (prepare-lubm.sh)
#   owl2bench-<p>-<N>    /data/owl2bench-<p>-<N>.nt, TBox included (prepare-owl2bench.sh)
# Queries: queries/<family>/*.rq, where the family is the name up to the first "-".
#
# Systems, each in Docker on the same host:
#   nrese-v2        reasoner v2's batch executor (crates/nrese-reasoner/examples/v2_closure.rs).
#                   Until its inferences are installed in the store (R4), Oxigraph answers
#                   the queries over its closure, as for Nemo.
#   jena-<profile>  Jena's rule reasoner, profile rdfs | owl-micro | owl-mini | owl
#   nemo            Nemo, a Rust datalog engine, running nemo/owl2rl.rls (the OWL 2 RL/RDF rules)
#   oracle          owlrl OWL 2 RL, the correctness reference. It runs only up to
#                   ORACLE_MAX_TRIPLES input triples and ORACLE_TIMEOUT_S, and its output is
#                   cached across runs. Where it doesn't finish, Nemo is the reference: its
#                   closure matches owlrl's exactly wherever both finish. The CSV names the
#                   reference used for every row.
#
# Per system and dataset:
#   - materialise: time as the system reports it (after parsing), inferred-triple count,
#     peak container memory (cgroup memory.peak)
#   - correctness against the oracle's inferred set (compare_inferred.py documents the
#     normalisation): precision and recall of the instance-level facts, which queries see;
#     "explained" = extra facts verified as entailed outside OWL 2 RL (counted as correct);
#     precision and recall of the schema-level facts, which differ legitimately by profile
#   - queries: the answer count of each query. Where queries/<family>/expected-<dataset>.tsv
#     exists (published LUBM(1) answers), they're checked against it; otherwise the systems
#     are cross-checked at the end.
#
# Output: one CSV line per system on stdout. Logs, inferred sets and reports go under
# benches/reasoning/results/ (git-ignored, because licensed systems write there too).
# The body is one block, so bash parses all of it before running: editing this file during a
# (long) run can't change the running script.
{
set -euo pipefail

# Datasets, store files and images take tens of GB: don't start on a nearly full disk.
. "$(dirname "$0")/../../scripts/lib/disk.sh"
require_free_gb "${NRESE_MIN_FREE_GB:-40}" "$(basename "$0")" "$(dirname "$0")"
# A run cleans up after itself, also when it fails or is interrupted: containers, store
# volumes, datasets and images go, results stay. NRESE_BENCH_KEEP=1 keeps datasets and
# images for the next run of a batch (see scripts/bench-cleanup.sh).
CLEANUP="$(cd "$(dirname "$0")/../.." && pwd)/scripts/bench-cleanup.sh"
trap '"$CLEANUP" >&2' EXIT
export MSYS_NO_PATHCONV=1
native() { cygpath -m "$1" 2>/dev/null || echo "$1"; }
HERE=$(native "$(cd "$(dirname "$0")" && pwd)")
ROOT=$(native "$(cd "$HERE/../.." && pwd)")
RESULTS=$(native "${RESULTS:-$HERE/results/$(date +%Y-%m-%d)}")
ORACLE_CACHE=$(native "${ORACLE_CACHE:-$HERE/results/oracle}")
SYSTEMS=${SYSTEMS:-oracle nrese-v2 jena-owl-micro nemo}
ORACLE_MAX_TRIPLES=${ORACLE_MAX_TRIPLES:-200000}
ORACLE_TIMEOUT_S=${ORACLE_TIMEOUT_S:-900}  # owlrl is pure Python; past this, Nemo is the reference
TIMEOUT_S=${TIMEOUT_S:-3600}
JAVA_HEAP=${JAVA_HEAP:-16g}
# Optional hard memory cap per container (e.g. 18g on a shared machine): a system that needs
# more is killed inside its container (exit 137) instead of pressuring the host.
DOCKER_MEMORY=${DOCKER_MEMORY:-}
# The Rust image follows rust-toolchain.toml, so benchmarks build with the pinned compiler.
RUST_IMAGE=${RUST_IMAGE:-rust:$(sed -n 's/^channel = "\(.*\)"/\1/p' "$ROOT/rust-toolchain.toml")-bookworm}
DATASETS=${*:-lubm-1 lubm-10}
mkdir -p "$RESULTS" "$ORACLE_CACHE"

# Runs a command in a container: output to <log>, the container's peak memory appended as
# `peak_bytes N`. Returns the command's exit code; 124 means TIMEOUT_S ran out.
measured() {
  local log=$1 image=$2 name=rsc-$$-$RANDOM rc=0
  shift 2
  # --init: tini as PID 1 passes the timeout's SIGTERM on (a shell as PID 1 ignores it, and
  # the container then ran past its limit); -k: KILL if it still hasn't stopped.
  timeout -k 30 "$TIMEOUT_S" docker run --init --name "$name" \
    ${DOCKER_MEMORY:+--memory "$DOCKER_MEMORY" --memory-swap "$DOCKER_MEMORY"} \
    -v nrese-bench-data:/data:ro \
    -v "$RESULTS:/out" -v "$ORACLE_CACHE:/cache" -v "$HERE/queries:/queries:ro" \
    -v nrese-target:/target:ro -e JAVA_TOOL_OPTIONS="-Xmx$JAVA_HEAP" --entrypoint sh "$image" \
    -c '"$@"; rc=$?; echo "peak_bytes $(cat /sys/fs/cgroup/memory.peak 2>/dev/null || echo 0)"; exit $rc' \
    sh "$@" >"$log" 2>&1 || rc=$?
  docker rm -f -v "$name" >/dev/null 2>&1 || true
  return $rc
}
field() { sed -nE "s/.*$1.*/\1/p" "$2" | head -1; } # <ERE with one capture group> <file>
peak_mib() { echo $(( $(field 'peak_bytes ([0-9]+)' "$1") / 1048576 )); }

# answers <log> <out.tsv>: the "qNN<TAB>count" lines of a run
answers() { awk -F'\t' '/^q[0-9]+\t[0-9]+/ { print $1 "\t" $2 }' "$1" >"$2"; }

# inputs <dataset>: the input files inside the containers
inputs() {
  case $1 in
    lubm-*) echo "/data/univ-bench.nt /data/$1.nt" ;;
    *) echo "/data/$1.nt" ;;
  esac
}

# queries_correct <dataset> <counts.tsv>: "k/n" against the published answers, else "-"
queries_correct() {
  local expected=$HERE/queries/${1%%-*}/expected-$1.tsv
  [ -f "$expected" ] && [ -s "$2" ] || { echo "-"; return; }
  echo "$(grep -v '^#' "$expected" | grep -cxFf "$2")/$(grep -vc '^#' "$expected")"
}

# reference <dataset>: the system whose inferred set is the correctness reference. The owlrl
# oracle where it finished; otherwise Nemo, whose OWL 2 RL closure matches the oracle's exactly
# wherever both finish (LUBM(1), OWL2Bench QL(1)). Empty if neither ran.
reference() {
  if [ -s "$RESULTS/oracle-$1.inferred.nt" ]; then echo oracle
  elif [ -s "$RESULTS/nemo-$1.inferred.nt" ]; then echo nemo
  fi
}

# compare <dataset> <system> <reference>: "precision,recall,explained,schema_precision,
# schema_recall" of the system's inferred set against the reference's (compare_inferred.py)
compare() {
  local log=$RESULTS/$2-$1.compare.log
  # shellcheck disable=SC2046
  docker run --rm -v nrese-bench-data:/data:ro -v "$RESULTS:/out" -v "$HERE:/kit:ro" \
    --entrypoint python nrese-bench/owlrl-oracle /kit/compare_inferred.py \
    "/out/$3-$1.inferred.nt" "/out/$2-$1.inferred.nt" --input $(inputs "$1") --label "$2" \
    --json "/out/$2-$1.compare.json" >"$log" 2>&1 || { echo "error,error,error,error,error"; return; }
  echo "$(field '\| precision ([0-9.]+)' "$log"),$(field '\| recall ([0-9.]+)' "$log"),$(field 'explained ([0-9]+)' "$log"),$(field 'schema precision ([0-9.]+)' "$log"),$(field 'schema precision [0-9.]+ recall ([0-9.]+)' "$log")"
}

run_oracle() { # <dataset> <log>
  local cached=$ORACLE_CACHE/oracle-owl2-rl-$1.nt triples
  triples=$(docker run --rm -v nrese-bench-data:/data:ro alpine sh -c "cat $(inputs "$1") | sort -u | wc -l")
  if [ "$triples" -gt "$ORACLE_MAX_TRIPLES" ]; then
    echo "skipped: $triples input triples > ORACLE_MAX_TRIPLES=$ORACLE_MAX_TRIPLES" >"$2"
    return 1
  fi
  if [ -s "$cached" ] && [ -s "$cached.log" ]; then
    cp "$cached.log" "$2"
  else
    local queries=
    [ ! -d "$HERE/queries/${1%%-*}" ] || queries="--queries /queries/${1%%-*}"
    # shellcheck disable=SC2086
    TIMEOUT_S=$ORACLE_TIMEOUT_S measured "$2" nrese-bench/owlrl-oracle python /oracle/owlrl_materialise.py \
      --profile owl2-rl $queries --out "/cache/oracle-owl2-rl-$1.nt" $(inputs "$1") || return 1
    cp "$2" "$cached.log"
  fi
  cp "$cached" "$RESULTS/oracle-$1.inferred.nt"
  echo "$(field 'asserted: ([0-9]+)' "$2"),$(field 'triples in ([0-9.]+) s' "$2"),$(field 'closure \(owl2-rl\): ([0-9]+)' "$2"),$(peak_mib "$2")"
}

run_nrese_v2() { # <dataset> <log>
  measured "$2" "$RUST_IMAGE" /target/release/examples/v2_closure \
    --out "/out/nrese-v2-$1.inferred.nt" $(inputs "$1") || return 1
  if [ -d "$HERE/queries/${1%%-*}" ]; then
    # shellcheck disable=SC2046
    docker run --rm -v nrese-bench-data:/data:ro -v "$RESULTS:/out:ro" -v "$HERE/queries:/queries:ro" \
      --entrypoint python nrese-bench/owlrl-oracle /oracle/answer_queries.py --queries "/queries/${1%%-*}" \
      $(inputs "$1") "/out/nrese-v2-$1.inferred.nt" >>"$2" 2>&1 || true
  fi
  echo "$(field 'asserted ([0-9]+)' "$2"),$(field 'closure ([0-9.]+) s' "$2"),$(field 'derived ([0-9]+)' "$2"),$(peak_mib "$2")"
}

run_nemo() { # <dataset> <log>
  # Nemo reads one import file, so the inputs are concatenated first (outside the timing).
  # shellcheck disable=SC2046
  measured "$2" nrese-bench/nemo bash -c 'mkdir -p /in /tmp/nemo && cat "$@" > /in/input.nt &&
    nmo -I /in -D /tmp/nemo -o --report short /nemo/owl2rl.rls &&
    cp /tmp/nemo/inferred.nt "/out/nemo-$0.inferred.nt" && cp /tmp/nemo/violations.csv "/out/nemo-$0.violations.csv"' \
    "$1" $(inputs "$1") || return 1
  # Nemo has no SPARQL engine: Oxigraph answers the queries over its closure (counts only).
  if [ -d "$HERE/queries/${1%%-*}" ]; then
    # shellcheck disable=SC2046
    docker run --rm -v nrese-bench-data:/data:ro -v "$RESULTS:/out:ro" -v "$HERE/queries:/queries:ro" \
      --entrypoint python nrese-bench/owlrl-oracle /oracle/answer_queries.py --queries "/queries/${1%%-*}" \
      $(inputs "$1") "/out/nemo-$1.inferred.nt" >>"$2" 2>&1 || true
  fi
  local asserted
  asserted=$(docker run --rm -v nrese-bench-data:/data:ro alpine sh -c "cat $(inputs "$1") | sort -u | wc -l")
  echo "$asserted,$(awk "BEGIN { print $(field 'Reasoning: +([0-9]+)ms' "$2") / 1000 }"),$(wc -l <"$RESULTS/nemo-$1.inferred.nt"),$(peak_mib "$2")"
}

run_jena() { # <dataset> <log> <profile>
  local queries=-
  [ ! -d "$HERE/queries/${1%%-*}" ] || queries=/queries/${1%%-*}
  measured "$2" nrese-bench/jena-reasoner java -cp '/runner:/opt/jena/lib/*' Materialise "$3" \
    "/out/jena-$3-$1.inferred.nt" "$queries" $(inputs "$1") || return 1
  echo "$(field 'asserted ([0-9]+)' "$2"),$(field 'closure ([0-9.]+)' "$2"),$(field 'inferred ([0-9]+)' "$2"),$(peak_mib "$2")"
}

# Images and the NRESE build
docker build -q -t nrese-bench/owlrl-oracle "$HERE/oracle" >/dev/null
docker image inspect nrese-bench/jena:6.2.0 >/dev/null 2>&1 ||
  docker build -q -t nrese-bench/jena:6.2.0 "$HERE/../competitors/jena" >/dev/null
docker build -q -t nrese-bench/jena-reasoner "$HERE/jena" >/dev/null
docker build -q -t nrese-bench/nemo "$HERE/nemo" >/dev/null
if [[ " $SYSTEMS " == *" nrese-v"* ]] && [ -z "${SKIP_BUILD:-}" ]; then
  docker volume create nrese-target >/dev/null
  docker run --rm -v "$ROOT:/src:ro" -v nrese-target:/target -v nrese-cargo:/usr/local/cargo/registry \
    -w /src "$RUST_IMAGE" cargo build --release --locked -p nrese-reasoner --example v2_closure \
    --target-dir /target >"$RESULTS/nrese-build.log" 2>&1
fi

CSV=$RESULTS/reasoning-scorecard.csv
header="dataset,system,asserted,materialise_s,inferred,peak_mib,reference,precision,recall,explained,schema_precision,schema_recall,queries_correct"
[ -s "$CSV" ] || echo "$header" >"$CSV"
echo "$header"
for dataset in $DATASETS; do
  # 1. Run every system; a failed run leaves no inferred set, so it can't become the reference.
  declare -A stats=()
  for system in $SYSTEMS; do
    log=$RESULTS/$system-$dataset.log
    rm -f "$RESULTS/$system-$dataset.inferred.nt" "$RESULTS/$system-$dataset.answers.tsv"
    case $system in
      oracle) result=$(run_oracle "$dataset" "$log") || { echo "$dataset,$system: $(tail -1 "$log")" >&2; continue; } ;;
      nrese-v2) result=$(run_nrese_v2 "$dataset" "$log") || { echo "$dataset,$system failed, see $log" >&2; continue; } ;;
      nemo) result=$(run_nemo "$dataset" "$log") || { echo "$dataset,$system failed, see $log" >&2; continue; } ;;
      jena-*) result=$(run_jena "$dataset" "$log" "${system#jena-}") || { echo "$dataset,$system failed, see $log" >&2; continue; } ;;
      *) echo "unknown system $system" >&2; continue ;;
    esac
    stats[$system]=$result
    answers "$log" "$RESULTS/$system-$dataset.answers.tsv"
  done
  # 2. Correctness of each against the reference
  ref=$(reference "$dataset")
  for system in $SYSTEMS; do
    [ -n "${stats[$system]:-}" ] || continue
    if [ -z "$ref" ]; then quality="-,-,-,-,-,-"
    elif [ "$system" = "$ref" ]; then quality="$ref,1.0000,1.0000,0,1.0000,1.0000"
    else quality="$ref,$(compare "$dataset" "$system" "$ref")"
    fi
    line="$dataset,$system,${stats[$system]},$quality,$(queries_correct "$dataset" "$RESULTS/$system-$dataset.answers.tsv")"
    echo "$line" | tee -a "$CSV"
  done
  unset stats
  # Cross-check: answer counts side by side, for systems that answered queries
  tables=$(ls "$RESULTS"/*-"$dataset".answers.tsv 2>/dev/null | while read -r f; do [ ! -s "$f" ] || echo "$f"; done)
  if [ -n "$tables" ]; then
    echo "# answers on $dataset: $(for f in $tables; do basename "$f" "-$dataset.answers.tsv"; done | tr '\n' ' ')"
    # shellcheck disable=SC2086
    paste $tables | awk -F'\t' '{ line = $1; for (i = 2; i <= NF; i += 2) line = line "\t" $i; print line }'
  fi
done
exit
}
