#!/usr/bin/env bash
# cargo, with a bounded build directory. Use it in place of `cargo` for anything that
# compiles (build, test, clippy, run, bench):
#
#   scripts/cargo-guarded.sh test --locked --workspace
#   scripts/cargo-guarded.sh --status        sizes and limits, nothing else
#   scripts/cargo-guarded.sh --clean         empty the build directory now
#
# Before cargo starts it checks two limits:
# - the build directory's budget (NRESE_TARGET_BUDGET_GB, default 25): over it, the debug
#   output is removed, and if that isn't enough, everything. Cargo never removes stale
#   artifacts itself, so this is what keeps the directory from growing without bound.
#   The directory is measured at most every ten minutes.
# - the free-space floor (NRESE_MIN_FREE_GB, default 20): below it nothing is built. A
#   full build writes several GB, and a full system disk affects every program.
#
# It also keeps a build from taking the whole machine: half the cores unless
# CARGO_BUILD_JOBS says otherwise, low process priority, so other work on the same
# machine and disk (editors, containers, WSL) stays responsive, and on Windows a memory
# cap on the build and everything it runs (NRESE_MEMORY_CAP_GB, default half the RAM).
#
# Code is generated for this machine's CPU unless NRESE_TARGET_CPU says otherwise
# (`portable`, `x86-64-v3`, …; see scripts/lib/target-cpu.sh).
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
# shellcheck source=lib/disk.sh
. "$ROOT/scripts/lib/disk.sh"
# shellcheck source=lib/target-cpu.sh
. "$ROOT/scripts/lib/target-cpu.sh"

TARGET="${CARGO_TARGET_DIR:-$ROOT/target}"
BUDGET_GB="${NRESE_TARGET_BUDGET_GB:-25}"
MIN_FREE_GB="${NRESE_MIN_FREE_GB:-20}"
STAMP="$TARGET/.guard-measured"

status() {
  printf 'build directory %s: %s GB (budget %s GB); free: %s GB (floor %s GB)\n' \
    "$TARGET" "$(dir_gb "$TARGET")" "$BUDGET_GB" "$(free_gb "$ROOT")" "$MIN_FREE_GB"
}

# Every removal is logged, so a slow build can be told apart from one that started from
# nothing (tmp/ is git-ignored).
WIPES="$ROOT/tmp/guard-wipes.log"
logged() {
  mkdir -p "$ROOT/tmp"
  printf '%s %s: %s\n' "$(date '+%Y-%m-%d %H:%M:%S')" "$TARGET" "$1" >> "$WIPES"
  printf '%s\n' "$1" >&2
}

# Until the directory fits the budget: first the stale builds cargo keeps of every crate
# (the newest of each stays, so the next build reuses it), then the debug output, then
# everything.
shrink() {
  local size freed
  size=$(dir_gb "$TARGET")
  if [ "$size" -gt "$BUDGET_GB" ]; then
    freed=$(python "$ROOT/scripts/lib/prune-stale.py" "$TARGET" 2>/dev/null || printf 0)
    logged "build directory was $size GB, over its $BUDGET_GB GB budget: stale builds removed ($((freed / 1048576)) MiB)"
    size=$(dir_gb "$TARGET")
  fi
  if [ "$size" -gt "$BUDGET_GB" ]; then
    logged "still $size GB: removing the debug output"
    (cd "$ROOT" && cargo clean --profile dev >&2)
    size=$(dir_gb "$TARGET")
  fi
  if [ "$size" -gt "$BUDGET_GB" ]; then
    logged "still $size GB: removing the whole build directory"
    (cd "$ROOT" && cargo clean >&2)
  fi
  mkdir -p "$TARGET"
  date +%s > "$STAMP"
}

case "${1:-}" in
  --status)
    status
    exit 0
    ;;
  --clean)
    (cd "$ROOT" && cargo clean)
    status
    exit 0
    ;;
esac

# Measuring walks the directory, so it is done at most every ten minutes.
now=$(date +%s)
last=$(cat "$STAMP" 2>/dev/null || printf 0)
if [ $((now - last)) -ge 600 ]; then
  shrink
fi
require_free_gb "$MIN_FREE_GB" "cargo $*" "$ROOT"

# A timing measurement holds the quiet slot (scripts/quiet-slot.sh): builds wait for it, at
# most NRESE_QUIET_WAIT_S (default 1800 s), so the measurement isn't timing our compilers.
QUIET="${NRESE_QUIET_DIR:-$HOME/.nrese-quiet-slot}"
waited=0
while [ -d "$QUIET" ] && [ "$waited" -lt "${NRESE_QUIET_WAIT_S:-1800}" ]; do
  owner=$(cat "$QUIET/pid" 2>/dev/null || echo "")
  if [ -n "$owner" ] && ! kill -0 "$owner" 2>/dev/null; then
    break
  fi
  [ "$waited" -eq 0 ] && printf 'a measurement holds the quiet slot (%s): waiting\n' \
    "$(cat "$QUIET/who" 2>/dev/null || echo '?')" >&2
  sleep 10
  waited=$((waited + 10))
done

export_target_cpu_rustflags

# A memory cap on everything this build starts (rustc, test binaries, examples): half the
# machine's memory unless NRESE_MEMORY_CAP_GB says otherwise (0: none). On Windows the
# script's own process joins a job object with that limit before cargo starts, so every
# child inherits it and a runaway test fails its allocation instead of taking the machine
# (one committed 128 GB on a 64 GB PC on 3 October 2026). Elsewhere it isn't enforced
# yet: the office runs go through containers with their own limits.
if [ -z "${NRESE_MEMORY_CAP_GB:-}" ]; then
  total_kb=$(awk '/^MemTotal:/ { print $2 }' /proc/meminfo 2>/dev/null || printf 0)
  NRESE_MEMORY_CAP_GB=$(( total_kb / 2 / 1048576 ))
fi
if [ "${NRESE_MEMORY_CAP_GB:-0}" -gt 0 ] && [ -r "/proc/$$/winpid" ] && command -v powershell >/dev/null 2>&1; then
  powershell -NoProfile -ExecutionPolicy Bypass -File "$(cygpath -w "$ROOT/scripts/lib/memory-cap.ps1")" \
    -ProcessId "$(cat "/proc/$$/winpid")" -LimitGB "$NRESE_MEMORY_CAP_GB" >&2 \
    || printf 'memory cap not applied; building uncapped\n' >&2
fi

if [ -z "${CARGO_BUILD_JOBS:-}" ]; then
  cores=$(nproc 2>/dev/null || printf 4)
  export CARGO_BUILD_JOBS=$(( cores > 3 ? cores / 2 : 2 ))
fi
if command -v nice >/dev/null 2>&1; then
  exec nice -n 15 cargo "$@"
fi
exec cargo "$@"
