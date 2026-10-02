#!/usr/bin/env bash
# A dataset in the perf lab on the office PC: load into an on-disk store, queries, then a
# restart from the checkpoint (open time and resident memory).
#
#   office-perflab.sh LABEL [DATASET-FILE QUERY-DIR [perf_lab args...]]
#
# Defaults to DBpedia core. Builds the perf lab first unless NO_BUILD=1.
#
# Usage: scripts/office/perflab.sh LABEL [DATASET-FILE QUERY-DIR [perf_lab args...]]   (NO_BUILD=1 to skip the build)
set -u
source ~/.cargo/env
cd "$(dirname "$0")/../.."
label=${1:-run}
data=${2:-$HOME/nrese-bench/scratch/dbpedia/dbpedia-core.nt}
queries=${3:-benches/competitors/queries/dbpedia-core}
shift $(( $# < 3 ? $# : 3 ))
store=~/nrese-bench/scratch/store-$label
lab=~/nrese-bench/scratch/perf_lab-$label
if [ "${NO_BUILD:-}" != 1 ]; then
  scripts/cargo-guarded.sh build --release -p nrese-store --example perf_lab || exit 1
  cp target/release/examples/perf_lab "$lab"
fi
rm -rf "$store"
echo "=== load + queries ($label)"
"$lab" --store "$store" --load "$data" --queries "$queries" --runs 3 --label "$label" "$@"
echo "=== restart ($label)"
RUST_LOG=nrese_engine=debug "$lab" --store "$store" --queries "$queries" --runs 3 --label "$label-restart" "$@"
du -sh "$store"
rm -rf "$store"
echo ALL-DONE
