#!/usr/bin/env bash
# nrese-exec's unsafe primitives under Miri: their tests, interpreted for a Linux target
# (Miri can't run Windows' foreign functions) with Tree Borrows (Stacked Borrows stops in
# crossbeam, under Rayon, before it reaches our code). Run by the push tier when nrese-exec
# changed (scripts/check.sh); about 40 s cold, 10 s warm.
#
# Leaks are not Miri's to report here: crossbeam-epoch keeps its collector for the process
# by design, and the tests check what they made is freed (drop counts, budget charges).
#
# Usage: scripts/miri.sh. Needs the nightly toolchain with Miri:
#   rustup toolchain install nightly --component miri
set -u
cd "$(dirname "$0")/.."
# The tests that exercise unsafe code (one filter each).
tests=(
  budget::tests::a_panicking_parallel_fill_drops_every_item_it_wrote
  budget::tests::a_failing_parallel_fill_stops_and_drops_every_item_it_wrote
  region::tests::a_region_is_charged_before_it_grows_and_returns_everything
  region::tests::a_region_holds_at_its_boundaries
)
if ! cargo +nightly miri --version > /dev/null 2>&1; then
  echo "miri.sh: no nightly Miri; rustup toolchain install nightly --component miri" >&2
  exit 1
fi
export MIRIFLAGS="-Zmiri-tree-borrows -Zmiri-ignore-leaks"
# Nightly reports lints stable doesn't; the gate's lints are stable clippy's.
export CARGO_BUILD_WARNINGS=warn
export CARGO_TARGET_DIR=target/miri
status=0
# Through cargo-guarded.sh: Miri's compiles wait for room like any other (no CPU flag, as before).
for test in "${tests[@]}"; do
  NRESE_TARGET_CPU=portable bash scripts/cargo-guarded.sh +nightly miri test --locked -q \
    --target x86_64-unknown-linux-gnu -p nrese-exec --lib \
    -- --exact "$test" || status=1
done
exit $status
