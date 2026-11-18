#!/usr/bin/env bash
# The BSBM BI queries in the perf lab on a BSBM 10 M store (loaded once, kept for reruns).
#
# Usage: benches/probes/bsbm-bi-lab.sh [perf_lab args...]   (BSBM 10 M in $SCRATCH/bsbm; RUNS, NO_BUILD=1)
set -u
source "$(dirname "$0")/common.sh"
cd "$REPO"
[ "${NO_BUILD:-}" = 1 ] || { scripts/cargo-guarded.sh build --release -p nrese-store --example perf_lab 2>&1 | grep -E "^error|Finished"; cp target/release/examples/perf_lab $SCRATCH/perf_lab-bsbm; }
bsbm=$SCRATCH/bsbm
python3 "$REPO/benches/probes/bsbm-bi-queries.py" "$bsbm"
if [ ! -d $bsbm/store-lab ]; then
  $SCRATCH/perf_lab-bsbm --store $bsbm/store-lab --load $bsbm/bsbm-10m.nt --queries $bsbm/biq --runs 1 --timeout-s 60 --label load "$@" | tail -3
fi
$SCRATCH/perf_lab-bsbm --store $bsbm/store-lab --queries $bsbm/biq --runs ${RUNS:-3} --timeout-s 60 --label bi "$@"
echo ALL-DONE
