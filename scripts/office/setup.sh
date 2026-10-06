#!/usr/bin/env bash
# Office PC: the benchmark suite's tools, NRESE, and NRESE on the Oxigraph libraries
# (the baseline branch), built at low priority. Each step logs; a failed step doesn't stop
# the next one.
#
# Usage: scripts/office/setup.sh   (on the office PC, ~/nrese-bench; expects baseline.bundle there for the baseline worktree)
set -u
export PATH=$HOME/.cargo/bin:$PATH
BENCH=$HOME/nrese-bench
REPO=$BENCH/OWL-RS
LOG=$BENCH/setup-2026-10-01
mkdir -p "$LOG"
step() { echo "$(date +%H:%M:%S) $1"; }

step "baseline branch from the bundle"
git -C "$REPO" fetch -q "$BENCH/baseline.bundle" baseline/pre-oxigraph-migration:baseline/pre-oxigraph-migration &&
  { [ -d "$BENCH/OWL-RS-baseline" ] || git -C "$REPO" worktree add -q "$BENCH/OWL-RS-baseline" baseline/pre-oxigraph-migration; } &&
  rm -f "$BENCH/baseline.bundle"
git -C "$BENCH/OWL-RS-baseline" log --oneline -1

step "suite tools (images, kit images, Fuseki)"
nice -n 10 bash "$REPO/benches/suite/install-tools.sh" > "$LOG/install-tools.log" 2>&1
echo "install-tools exit $?"
bash "$REPO/benches/suite/install-tools.sh" --list

step "NRESE release build"
(cd "$REPO" && nice -n 19 scripts/cargo-guarded.sh build --release -p nrese-server) > "$LOG/build-nrese.log" 2>&1
echo "nrese build exit $?"

step "NRESE on Oxigraph (baseline) release build"
(cd "$BENCH/OWL-RS-baseline" && nice -n 19 scripts/cargo-guarded.sh build --release -p nrese-server) > "$LOG/build-baseline.log" 2>&1
echo "baseline build exit $?"
ls -la "$REPO/target/release/nrese-server" "$BENCH/OWL-RS-baseline/target/release/nrese-server" 2>&1
step "done"
