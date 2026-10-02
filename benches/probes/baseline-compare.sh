#!/usr/bin/env bash
# Engine end to end: the baseline branch (NRESE on the Oxigraph libraries) against today's
# branch, the same binaries' examples on the same files, alternating so drift hits both.
#
# Usage: benches/probes/baseline-compare.sh   (BASE=<baseline worktree>, DATA=<dir with univ-bench.nt, lubm-10.nt, olympics.nt>)
set -u
source "$(dirname "$0")/common.sh"
DATA=${DATA:-$SCRATCH}
HEAD=$REPO
BASE=${BASE:-$BENCH/OWL-RS-baseline}
OUT=$HEAD/tmp/engine-bench
exe=""; [ -f "$HEAD/target/release/examples/perf_lab$exe" ] && exe=.exe
mkdir -p "$OUT"
for round in 1 2; do
  for build in baseline head; do
    root=$HEAD; [ "$build" = baseline ] && root=$BASE
    echo "=== round $round $build: LUBM(10) load + OWL 2 RL + 14 queries + 200 commits"
    "$root/target/release/examples/reason_query$exe" --commits 200 --queries "$HEAD/benches/reasoning/queries/lubm" \
      "$DATA/univ-bench.nt" "$DATA/lubm-10.nt" 2>&1 | tail -40
    echo "=== round $round $build: Olympics perf lab"
    "$root/target/release/examples/perf_lab$exe" --load "$DATA/olympics.nt" --queries "$HEAD/benches/competitors/queries/olympics" \
      --runs 5 --warmup 1 --label "$build-$round" --json "$OUT/olympics-$build-$round.json" 2>&1 | tail -40
  done
done
echo "=== done"
