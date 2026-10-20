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
# machine and disk (editors, containers, WSL) stays responsive, on Windows a memory
# cap on the build and everything it runs (NRESE_MEMORY_CAP_GB, default half the RAM),
# and admission: a cargo run, its tests included, starts only when the machine isn't busy,
# it is first in line, and its declared memory peak fits above the floor (below).
#
# Code is generated for this machine's CPU unless NRESE_TARGET_CPU says otherwise
# (`portable`, `x86-64-v3`, …; see scripts/lib/target-cpu.sh).
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
# shellcheck source=lib/disk.sh
. "$ROOT/scripts/lib/disk.sh"
# shellcheck source=lib/target-cpu.sh
. "$ROOT/scripts/lib/target-cpu.sh"

# --print-rustflags: the RUSTFLAGS a build here hands rustc (the CPU choice added), and nothing
# else: what a build's identity hashes (scripts/lib/gate-publish.sh).
if [ "${1:-}" = --print-rustflags ]; then
  export_target_cpu_rustflags
  printf '%s\n' "${RUSTFLAGS:-}"
  exit 0
fi

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
while [ -z "${NRESE_QUIET_HOLDER:-}" ] && [ -d "$QUIET" ] && [ "$waited" -lt "${NRESE_QUIET_WAIT_S:-1800}" ]; do
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

# Jobs per build: three quarters of the cores (the rest stays free for whoever uses the machine),
# shared by two builds, so that two at once don't run more compilers than the machine has threads
# (8 jobs x 3 builds on 16 threads did). Admission (below) may lower it to what memory allows.
jobs_given=${CARGO_BUILD_JOBS:-}
if [ -z "$jobs_given" ]; then
  cores=$(nproc 2>/dev/null || printf 4)
  jobs=$(( (cores * 3 / 4 + 1) / 2 ))
  export CARGO_BUILD_JOBS=$(( jobs > 2 ? jobs : 2 ))
fi

# Admission, one queue per machine (the main PC's guard stopped 47 of our processes at its hard
# line between 12:17 and 12:33 on 20 Oct 2026: builds started whatever the memory). A cargo run
# starts only when
# - the machine isn't busy: no ~/owlrs-guard/BUSY (NRESE_GUARD_BUSY), the guard's soft line;
# - it is first in line (first come, first served, across every worktree and agent); and
# - its declared peak fits: free memory, less the peaks the runs already admitted declared, less
#   its own, stays at or above the floor: NRESE_BUILD_FLOOR_GB, by default the guard's soft line
#   (16 GB) where the guard runs (~/owlrs-guard), else 0.
# Its peak: NRESE_BUILD_PEAK_GB, else 0.6 GB per compiler job (1.2 GB for release code, fat LTO)
# and 2 GB for the linker and the tests it runs. When that doesn't fit, it runs with fewer jobs
# (2 at least) unless CARGO_BUILD_JOBS was given; else it waits. Admitted, it holds its place for
# the whole cargo run, compiling and running tests alike, until cargo ends.
#
# It never builds anyway: it waits, saying its place in line every minute. Only at a terminal does
# it give up, after NRESE_BUILD_SLOT_WAIT_S (default 3600 s), with exit 75. Inside a fleet job's
# slice (FLEET_SLICE) the fleet admitted the job by its ask and the slice holds it there: no
# admission here. NRESE_BUILD_SLOTS=0 switches it off. Waits go to tmp/guard-slots.log.
ADMIT="${NRESE_BUILD_SLOT_DIR:-$HOME/.nrese-build-slots}"
BUSY="${NRESE_GUARD_BUSY:-$HOME/owlrs-guard/BUSY}"
if [ -d "$(dirname "$BUSY")" ]; then floor_default=16; else floor_default=0; fi
FLOOR_MB=$(( ${NRESE_BUILD_FLOOR_GB:-$floor_default} * 1024 ))
per_job_mb=614
previous=""
for arg in "$@"; do
  case "$previous $arg" in
    *" --release" | *" --profile=release" | *" --profile=lab" | *" --profile=profiling" \
      | "--profile release" | "--profile lab" | "--profile profiling") per_job_mb=1229 ;;
  esac
  previous=$arg
done
BASE_MB=2048
peak_of() {
  if [ -n "${NRESE_BUILD_PEAK_GB:-}" ]; then echo $(( NRESE_BUILD_PEAK_GB * 1024 )); else echo $(( $1 * per_job_mb + BASE_MB )); fi
}
# Free memory now (MiB): Linux's MemAvailable; Windows' free physical memory (Git Bash's MemFree,
# the guard's reading).
free_mb() {
  awk '/^MemAvailable:/ { a = $2 } /^MemFree:/ { f = $2 } END { print int((a ? a : f) / 1024) }' \
    "${NRESE_MEMINFO:-/proc/meminfo}"
}
alive() { kill -0 "$1" 2>/dev/null; }
# One admission at a time (a lock whose holder died, or never got to name itself, goes after a minute).
lock_admission() {
  local holder
  until mkdir "$ADMIT/.lock" 2>/dev/null; do
    holder=$(cat "$ADMIT/.lock/pid" 2>/dev/null || true)
    if { [ -n "$holder" ] && ! alive "$holder"; } || [ -n "$(find "$ADMIT/.lock" -maxdepth 0 -mmin +1 2>/dev/null)" ]; then
      rm -rf "$ADMIT/.lock"
    fi
    sleep 0.2
  done
  echo $$ > "$ADMIT/.lock/pid"
}
unlock_admission() { rm -rf "$ADMIT/.lock"; }

slot=""
ticket=""
if [ "${NRESE_BUILD_SLOTS:-1}" != 0 ] && [ -z "${FLEET_SLICE:-}" ]; then
  mkdir -p "$ADMIT/queue" "$ADMIT/run"
  ticket="$ADMIT/queue/$(date +%s%N)-$$"
  printf '%s cargo %s\n' "$ROOT" "$*" > "$ticket"
  trap 'rm -f "$ticket"; [ -z "$slot" ] || rm -rf "$slot"' EXIT
  waited=0 said=-60 limit="${NRESE_BUILD_SLOT_WAIT_S:-3600}"
  while :; do
    lock_admission
    for t in "$ADMIT"/queue/*; do if [ -e "$t" ] && ! alive "${t##*-}"; then rm -f "$t"; fi; done
    for r in "$ADMIT"/run/*; do if [ -e "$r" ] && ! alive "${r##*/}"; then rm -rf "$r"; fi; done
    place=1
    for t in "$ADMIT"/queue/*; do if [[ -e "$t" && "$t" < "$ticket" ]]; then place=$((place + 1)); fi; done
    others=0
    for r in "$ADMIT"/run/*; do if [ -e "$r/peak" ]; then others=$((others + $(cat "$r/peak"))); fi; done
    room=$(( $(free_mb) - FLOOR_MB - others ))
    peak=$(peak_of "$CARGO_BUILD_JOBS")
    why=""
    if [ -f "$BUSY" ]; then
      why="the machine is busy ($(cat "$BUSY" 2>/dev/null || true))"
    elif [ "$place" -gt 1 ]; then
      why="$((place - 1)) waiting before it"
    elif [ "$peak" -gt "$room" ]; then
      fit=$(( (room - BASE_MB) / per_job_mb ))
      if [ -z "$jobs_given" ] && [ -z "${NRESE_BUILD_PEAK_GB:-}" ] && [ "$fit" -ge 2 ]; then
        printf 'room for %s compiler jobs, not %s: building with %s\n' "$fit" "$CARGO_BUILD_JOBS" "$fit" >&2
        export CARGO_BUILD_JOBS=$fit
        peak=$(peak_of "$fit")
      else
        why="it may peak at $((peak / 1024)) GB; $(( room > 0 ? room / 1024 : 0 )) GB are free above the $((FLOOR_MB / 1024)) GB floor"
      fi
    fi
    if [ -z "$why" ]; then
      slot="$ADMIT/run/$$"
      mkdir -p "$slot"
      echo "$peak" > "$slot/peak"
      printf '%s cargo %s\n' "$ROOT" "$*" > "$slot/who"
      rm -f "$ticket"
      unlock_admission
      break
    fi
    unlock_admission
    if [ $((waited - said)) -ge 60 ]; then
      printf 'waiting to build, place %s in line: %s\n' "$place" "$why" >&2
      said=$waited
    fi
    if [ -t 2 ] && [ "$waited" -ge "$limit" ]; then
      printf 'no room to build after %s s; try again later, or build on the fleet\n' "$waited" >&2
      exit 75
    fi
    sleep 5
    waited=$((waited + 5))
  done
  if [ "$waited" -gt 0 ]; then
    mkdir -p "$ROOT/tmp"
    printf '%s waited %s s to build: cargo %s\n' "$(date '+%Y-%m-%d %H:%M:%S')" "$waited" "$*" >> "$ROOT/tmp/guard-slots.log"
  fi
fi

# The main PC's guard stops our processes below its hard line without telling them; it notes each
# in ~/owlrs-guard/killed.log (NRESE_GUARD_KILLED). A cargo run that failed while it did says so.
started=$(date +%s)
set +e
if command -v nice >/dev/null 2>&1; then
  nice -n 15 cargo "$@"
else
  cargo "$@"
fi
code=$?
set -e
killed="${NRESE_GUARD_KILLED:-$HOME/owlrs-guard/killed.log}"
if [ "$code" -ne 0 ] && [ -f "$killed" ]; then
  stopped=$(awk -v s="$started" '$1 >= s && $2 == "KILLED-BY-GUARD" { print $3 " " $4 }' "$killed" | tr '\n' ' ')
  if [ -n "$stopped" ]; then
    printf 'cargo %s: stopped by the PC guard, low memory (it stopped %s); not a build failure\n' "$*" "$stopped" >&2
    exit 75
  fi
fi
exit "$code"
