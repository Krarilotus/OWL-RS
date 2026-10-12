#!/usr/bin/env bash
# The breadth benchmarks' batches, one after another on one host (benches/breadth/README.md):
# each step waits until the host's owners are away (~/owlrs/RETREAT absent), then runs inside
# the quiet slot (scripts/quiet-slot.sh), so it is the only heavy job and no build times
# beside it. A step that fails is logged and the queue goes on.
#
#   benches/breadth/queue.sh STEP...      steps below; `all` for the first-priority order
#
# Results go to $NRESE_BREADTH/results/<step>-<date>; the log to $NRESE_BREADTH/queue.log.
set -u
ROOT=$(cd "$(dirname "$0")/../.." && pwd)
WORK=${NRESE_BREADTH:-$HOME/nrese-breadth}
PY=${BENCH_PYTHON:-python3}
LOG=$WORK/queue.log
STAMP=$(date +%Y%m%d)
RETREAT=${NRESE_RETREAT_FILE:-$HOME/owlrs/RETREAT}
cd "$ROOT"

step() {
  local name=$1; shift
  while [ -e "$RETREAT" ]; do sleep 60; done
  echo "$(date '+%F %T') start $name: $*" >> "$LOG"
  NRESE_QUIET_DRAIN_S=1800 scripts/quiet-slot.sh "$@" >> "$WORK/$name-$STAMP.log" 2>&1
  echo "$(date '+%F %T') end $name: exit $?" >> "$LOG"
}

run_step() {
  case $1 in
    images)  # every image the batches below use (builds wait for the quiet slot themselves)
      "$PY" benches/breadth/era-shacl/run.py images >> "$WORK/images-$STAMP.log" 2>&1
      "$PY" benches/breadth/vectors/ann.py setup >> "$WORK/images-$STAMP.log" 2>&1
      "$PY" benches/breadth/vectors/ann.py images --algorithms nrese,qdrant,pgvector,hnswlib >> "$WORK/images-$STAMP.log" 2>&1 ;;
    era-qualify)  # one repetition of every engine on the two smaller subsets, then the report check
      step era-qualify "$PY" benches/breadth/era-shacl/run.py run --subsets ES,FR --runs 1 \
        --results "$WORK/results/era-qualify-$STAMP"
      "$PY" benches/breadth/era-shacl/run.py check "$WORK/results/era-qualify-$STAMP" >> "$LOG" 2>&1 ;;
    era)  # the timed batch: three interleaved repetitions of all nine combinations
      step era "$PY" benches/breadth/era-shacl/run.py run --runs 3 --results "$WORK/results/era-$STAMP"
      "$PY" benches/breadth/era-shacl/run.py check "$WORK/results/era-$STAMP" >> "$LOG" 2>&1 ;;
    ann-sift|ann-glove)
      dataset=$([ "$1" = ann-sift ] && echo sift-128-euclidean || echo glove-100-angular)
      step "$1" "$PY" benches/breadth/vectors/ann.py run --dataset "$dataset" \
        --algorithms nrese,qdrant,pgvector,hnswlib ;;
    geo-qualify)
      step geo-qualify "$PY" benches/suite/suite.py run --data "$WORK/data" \
        --results "$WORK/results/geo-qualify-$STAMP" --workloads geographica2 --tier geographica2=real \
        --systems nrese,jena,rdf4j,graphdb,qlever,virtuoso --runs 1 --query-runs 1 --cache off \
        --query-timeout-s 600 --answers full --answers-reference jena ;;
    geo)  # the paper's limit: an hour per query
      step geo "$PY" benches/suite/suite.py run --data "$WORK/data" --results "$WORK/results/geo-$STAMP" \
        --workloads geographica2 --systems nrese,jena,rdf4j,graphdb,qlever,virtuoso --runs 3 \
        --query-timeout-s 3600 ;;
    *) echo "unknown step $1" >&2; return 2 ;;
  esac
}

steps=("$@")
[ "${steps[0]:-}" = all ] && steps=(images era-qualify ann-sift geo-qualify ann-glove era geo)
for s in "${steps[@]}"; do run_step "$s"; done
