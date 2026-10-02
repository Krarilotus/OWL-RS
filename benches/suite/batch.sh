#!/usr/bin/env bash
# A batch of suite runs from a worktree of the newest commit, so edits in the main tree
# don't reach the build; then every benchmark leftover goes (datasets, volumes, images, the
# worktree) and the results stay. Each argument group after the results name is one
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
echo "=== cleanup"
bash scripts/bench-cleanup.sh; echo "=== exit $?"
git worktree remove --force "$tree"; echo "=== worktree removed $?"
echo "BATCH-$name-DONE"
