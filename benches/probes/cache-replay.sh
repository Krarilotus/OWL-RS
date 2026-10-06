#!/usr/bin/env bash
# The result cache against an earlier build's on replayed query logs (merge checklist §2
# item 7): each build's `cache_replay` example (crates/nrese-store/examples/cache_replay.rs,
# copied into the earlier build's tree) replays the same seeded logs, interleaved, with
# the cache on and, for the newer build, off.
#
#   benches/probes/cache-replay.sh NEW_BIN OLD_BIN DATA_DIR OUT_DIR [REPS] [CACHE_BYTES]
#
# NEW_BIN, OLD_BIN: the two builds' `cache_replay` binaries. DATA_DIR holds olympics.nt and
# yago-tiny.nt; each build loads its own stores under OUT_DIR once. One line per run goes
# to OUT_DIR/runs.txt (the tool's report). Run from the repository root.
set -euo pipefail
new=$1 old=$2 data=$3 out=$4 reps=${5:-3} cache=${6:-1342177280}
queries=benches/competitors/queries
mkdir -p "$out"
declare -A sets=(
  [olympics]="--queries $queries/olympics"
  [yago-tiny]="--queries $queries/yago-tiny --queries $queries/yago-tiny-paths"
)
for build in new old; do
  bin=${!build}
  for dataset in "${!sets[@]}"; do
    store="$out/$build-$dataset"
    [ -d "$store" ] || "$bin" --store "$store" --load "$data/$dataset.nt"
  done
done
for rep in $(seq 1 "$reps"); do
  for dataset in "${!sets[@]}"; do
    # shellcheck disable=SC2086
    for run in "old $cache today" "new $cache parts" "new 0 off"; do
      set -- $run
      bin=${!1}
      "$bin" --store "$out/$1-$dataset" ${sets[$dataset]} --cache "$2" --seed "$rep" \
        --label "$dataset $3 rep$rep" | tee -a "$out/runs.txt"
    done
  done
done
