#!/usr/bin/env bash
# Runs a timing measurement in a quiet slot: one at a time on this machine, with no build
# of ours running. Several agents share a machine and each builds with its own job count,
# so without it timings carry the noise of everyone's compilers.
#
#   scripts/quiet-slot.sh <command> [args...]
#
# It takes a machine-wide lock (NRESE_QUIET_DIR, default ~/.nrese-quiet-slot), waits until no
# rustc or cargo of ours is running (at most NRESE_QUIET_DRAIN_S, default 600 s), runs the
# command and releases the lock. While the lock is held, scripts/cargo-guarded.sh waits
# before starting a build, so the slot stays quiet. A lock whose owner died is taken over.
# Counts and other deterministic measures need no slot; only times do.
set -u
LOCK="${NRESE_QUIET_DIR:-$HOME/.nrese-quiet-slot}"
DRAIN="${NRESE_QUIET_DRAIN_S:-600}"
[ $# -gt 0 ] || { echo "usage: scripts/quiet-slot.sh <command> [args...]" >&2; exit 2; }

alive() { kill -0 "$1" 2>/dev/null; }
compilers() {
  if command -v tasklist >/dev/null 2>&1; then
    tasklist 2>/dev/null | grep -ciE '^(rustc|cargo)\.exe'
  else
    pgrep -c -x 'rustc|cargo' 2>/dev/null || echo 0
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
while [ "$(compilers)" -gt 0 ] && [ $((SECONDS - start)) -lt "$DRAIN" ]; do
  sleep 5
done
[ "$(compilers)" -gt 0 ] && echo "quiet slot: compilers still running after ${DRAIN} s; measuring anyway" >&2
# What the measurement builds itself doesn't wait for the slot it holds.
NRESE_QUIET_HOLDER=1 "$@"
