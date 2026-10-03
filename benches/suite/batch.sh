#!/usr/bin/env bash
# A batch of suite runs from a worktree of the newest commit, so edits in the main tree
# don't reach the build; then the leftovers go (containers, stores, scratch, the worktree)
# and the results stay. The datasets stay until the results are concluded, unless the drive
# has less than 100 GB free (NRESE_BENCH_MIN_FREE_GB). Each argument group after the results name is one
# `suite.py run` invocation, separated by `--`. GraphDB's results stay local.
#
# Usage: benches/suite/batch.sh RESULTS-NAME [--wait-for LOG MARKER] -- RUN-ARGS... [-- RUN-ARGS...]
#   e.g. benches/suite/batch.sh 2026-10-02-lubm -- --systems nrese,qlever --workloads lubm --tier lubm=1
set -u
REPO=$(cd "$(dirname "$0")/../.." && pwd)
name=$1; shift
if [ "${1:-}" = --wait-for ]; then
  until grep -q "$3" "$2" 2>/dev/null; do sleep 120; done
  shift 3
fi
[ "${1:-}" = -- ] && shift
results="$REPO/benches/suite/results/$name"
tree="$REPO/tmp/batch-$name-tree"
cd "$REPO"
git worktree remove --force "$tree" 2>/dev/null
git worktree add --detach "$tree" "$(git rev-parse HEAD)" || exit 1
cd "$tree"
run=()
flush() {
  [ ${#run[@]} -eq 0 ] && return
  echo "=== suite ${run[*]}"
  python benches/suite/suite.py run --keep --results "$results" "${run[@]}"
  echo "=== exit $?"
  run=()
}
for arg in "$@"; do
  if [ "$arg" = -- ]; then flush; else run+=("$arg"); fi
done
flush
cd "$REPO"
# The datasets stay until the run's results are concluded (its record in benches/runs/ has
# its findings): then `scripts/bench-cleanup.sh` removes them. Below 100 GB free on the
# drive they go now. Containers, stores and scratch always go.
free_gb=$(( $(df -Pk "$REPO" | awk 'NR==2 {print $4}') / 1048576 ))
echo "=== cleanup (${free_gb} GB free)"
if [ "$free_gb" -lt "${NRESE_BENCH_MIN_FREE_GB:-100}" ]; then
  bash scripts/bench-cleanup.sh; echo "=== exit $?"
else
  NRESE_BENCH_KEEP=1 bash scripts/bench-cleanup.sh; echo "=== exit $? (datasets kept until the results are concluded)"
fi
git worktree remove --force "$tree"; echo "=== worktree removed $?"
echo "BATCH-$name-DONE"
