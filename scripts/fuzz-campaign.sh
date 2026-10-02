#!/usr/bin/env bash
# A bug hunt over many seeds: the random differential and property tests (SPARQL against
# the reference evaluator, reasoner closures, index models, the inferred stack, compact
# equality, incremental SHACL) run again and again, each run with another
# NRESE_FUZZ_SEED, until the time budget is spent. A failing run's output is kept with
# its seed, which replays it:
#
#   NRESE_FUZZ_SEED=<seed> cargo test --release -p <crate> --test <target> -- <test>
#
#   scripts/fuzz-campaign.sh [HOURS] [OUT_DIR]     (default 2 hours, ./fuzz-out)
#
# Runs niced; the release test binaries are built once through cargo-guarded.sh.
set -u
cd "$(dirname "$0")/.."
hours="${1:-2}"
out="${2:-fuzz-out}"
mkdir -p "$out"
deadline=$(( $(date +%s) + ${hours%.*} * 3600 ))

# The test binaries (crate:target) and the tests in them that draw random cases.
targets=(
  "nrese-sparql:native_differential_tests:"
  "nrese-store:equality_compact_tests:"
  "nrese-shacl:incremental_tests:"
  "nrese-engine:inferred_stack_tests:"
  "nrese-engine:lib:index::model_tests"
  "nrese-reasoner:lib:v2::"
)

declare -A binary
for entry in "${targets[@]}"; do
  IFS=: read -r crate target filter <<<"$entry"
  if [ "$target" = lib ]; then kind=(--lib); else kind=(--test "$target"); fi
  path=$(nice scripts/cargo-guarded.sh test --release --locked -p "$crate" "${kind[@]}" --no-run \
           --message-format=json 2>/dev/null \
         | grep -o '"executable":"[^"]*"' | tail -1 | cut -d'"' -f4)
  if [ -z "$path" ]; then echo "no test binary for $crate $target" >&2; exit 1; fi
  binary["$crate:$target"]="$path"
done

seed=${NRESE_FUZZ_FIRST:-1}
runs=0
failures=0
while [ "$(date +%s)" -lt "$deadline" ]; do
  for entry in "${targets[@]}"; do
    IFS=: read -r crate target filter <<<"$entry"
    log="$out/$crate-$target-$seed.log"
    if NRESE_FUZZ_SEED=$seed nice "${binary[$crate:$target]}" $filter -q >"$log" 2>&1; then
      rm -f "$log"
    else
      failures=$((failures + 1))
      echo "FAIL seed=$seed $crate $target ($log)"
    fi
    runs=$((runs + 1))
  done
  seed=$((seed + 1))
done
echo "DONE runs=$runs seeds=$((seed - ${NRESE_FUZZ_FIRST:-1})) failures=$failures"
