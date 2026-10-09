#!/usr/bin/env bash
# Runs a timing measurement in a quiet slot: one at a time on this machine, with no build
# of ours running. Several agents share a machine and each builds with its own job count,
# so without it timings carry the noise of everyone's compilers.
#
#   scripts/quiet-slot.sh <command> [args...]
#
# It takes a machine-wide lock (NRESE_QUIET_DIR, default ~/.nrese-quiet-slot), waits until no
# rustc or cargo of ours is running (at most NRESE_QUIET_DRAIN_S, default 600 s), runs the
# command and releases the lock. If compilers remain at the drain deadline, it releases
# the lock and exits 124 without starting the command (0 s means check without waiting).
# Failure to inspect processes exits 2; otherwise the command's exit status is preserved.
# While the lock is held, scripts/cargo-guarded.sh waits
# before starting a build, so the slot stays quiet. A lock whose owner died is taken over.
# Counts and other deterministic measures need no slot; only times do.
set -u
LOCK="${NRESE_QUIET_DIR:-$HOME/.nrese-quiet-slot}"
DRAIN="${NRESE_QUIET_DRAIN_S:-600}"
[ $# -gt 0 ] || { echo "usage: scripts/quiet-slot.sh <command> [args...]" >&2; exit 2; }

alive() { kill -0 "$1" 2>/dev/null; }
compilers() {
  if command -v tasklist >/dev/null 2>&1; then
    local tasks
    tasks=$(tasklist 2>/dev/null) || return 2
    printf '%s\n' "$tasks" | grep -qiE '^(rustc|cargo)\.exe'
  else
    # pgrep returns 1 for no matches, not a failed inspection. Use its status rather
    # than a count: `pgrep -c ... || echo 0` prints two zeros when nothing matches.
    pgrep -x 'rustc|cargo' >/dev/null 2>&1
  fi
}

while ! mkdir "$LOCK" 2>/dev/null; do
  owner=$(cat "$LOCK/pid" 2>/dev/null || echo "")
  if [ -n "$owner" ] && ! alive "$owner"; then
    rm -rf "$LOCK"
    continue
  fi
  echo "quiet slot held by $(cat "$LOCK/who" 2>/dev/null || echo '?'): waiting" >&2
  sleep 15
done
echo $$ > "$LOCK/pid"
printf '%s %s\n' "$(date '+%H:%M:%S')" "$*" > "$LOCK/who"
trap 'rm -rf "$LOCK"' EXIT

start=$SECONDS
# Our own cargo (this script's parent shell's) isn't running; others' may be.
while :; do
  compilers
  case $? in
    0) ;;
    1) break ;;
    *) echo 'quiet slot: cannot inspect compiler processes; measurement not started' >&2; exit 2 ;;
  esac
  if [ $((SECONDS - start)) -ge "$DRAIN" ]; then
    echo "quiet slot: compilers still running after ${DRAIN} s; measurement not started" >&2
    exit 124
  fi
  sleep 5
done
# What the measurement builds itself doesn't wait for the slot it holds.
NRESE_QUIET_HOLDER=1 "$@"
