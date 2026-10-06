#!/usr/bin/env bash
# Plain against FSST-compressed vocabularies, both checkpoint format 11 and one binary:
# each store rewritten as two images, then queried after a restart in the perf lab
# (5 runs): checkpoint size, resident memory, query times.
#
# Usage: benches/probes/vocabulary-compare.sh   (stores $SCRATCH/store-d etc. from a perf lab load)
set -u
source "$(dirname "$0")/common.sh"
cd "$REPO"
s=$SCRATCH
nice scripts/cargo-guarded.sh build --release -p nrese-store --example perf_lab 2>&1 | grep -E "^error|Finished"
nice scripts/cargo-guarded.sh build --release -p nrese-engine --example image 2>&1 | grep -E "^error|Finished"
lab=target/release/examples/perf_lab
run() { # store queries label
  rm -rf $s/img-$1-plain $s/img-$1-fsst
  NRESE_VOCABULARY=plain target/release/examples/image $s/store-$1 $s/img-$1-plain
  NRESE_VOCABULARY=fsst target/release/examples/image $s/store-$1 $s/img-$1-fsst
  $lab --store $s/img-$1-plain --queries $2 --runs 5 --label $3-plain --json $s/cmp-$3-plain.json 2>&1 | grep -vE "^open"
  $lab --store $s/img-$1-fsst --queries $2 --runs 5 --label $3-fsst --json $s/cmp-$3-fsst.json --baseline $s/cmp-$3-plain.json 2>&1 | grep -vE "^open"
  rm -rf $s/img-$1-plain $s/img-$1-fsst
}
run d benches/competitors/queries/dbpedia-core dbpedia
#run w benches/competitors/queries/wikidata-lexemes wikidata
rm -f $s/cmp-*.json
echo ALL-DONE
