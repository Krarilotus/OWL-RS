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
# While the lock is held, scripts/cargo-guarded.sh waits before starting a build, and the
# watcher (lib/quiet-watch.py) stops whatever else of this user gets busy and pauses other
# containers until the measurement ends: the slot is quiet, not only asked to be. A lock
# whose owner died is taken over, what its watcher stopped resumed first.
# Counts and other deterministic measures need no slot; only times do.
set -u
LOCK="${NRESE_QUIET_DIR:-$HOME/.nrese-quiet-slot}"
DRAIN="${NRESE_QUIET_DRAIN_S:-600}"
here="${BASH_SOURCE[0]%/*}"
[ "$here" = "${BASH_SOURCE[0]}" ] && here=.
WATCH="$here/lib/quiet-watch.py"
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
    # What a dead holder's watcher suspended continues before the lock is taken over; what
    # can't be settled keeps the lock (its record isn't erased) and this measurement doesn't run.
    if [ -f "$LOCK/frozen" ] && ! python3 "$WATCH" "$LOCK" --resume; then
      echo "quiet slot: a dead holder's suspensions couldn't be resumed ($LOCK/frozen); not taken over" >&2
      exit 2
    fi
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
# While the measurement runs, the watcher suspends the job's own other busy work (its
# FLEET_JOB, its slice, its namespace's containers) and resumes it when it ends; anything else
# is left running and recorded (scripts/lib/quiet-watch.py; its verdict, quiet or disturbed,
# in watch.log).
# The measurement's own containers carry the label nrese.quiet=$NRESE_QUIET_TOKEN.
export NRESE_QUIET_TOKEN="$$-$(date +%s)"
watcher=""
if [ -d /proc ] && command -v python3 >/dev/null 2>&1; then
  # The measurement is whoever asked for the slot and all it runs (fast.py holds the slot with
  # a stand-in command and measures itself, beside this shell): this shell's parent.
  # A caller that is init (detached, or gone already) would make every process the
  # measurement: then the measurement is this shell's own command.
  root=$PPID
  if [ "$root" -le 1 ] || ! kill -0 "$root" 2>/dev/null; then root=$$; fi
  python3 "$WATCH" "$LOCK" "$root" "$NRESE_QUIET_TOKEN" &
  watcher=$!
fi
stop_watch() {
  local status=$? watched=0
  if [ -n "$watcher" ]; then
    kill -TERM "$watcher" 2>/dev/null
    wait "$watcher" || watched=$?
    [ -f "$LOCK/watch.log" ] && echo "quiet slot: $(tail -1 "$LOCK/watch.log")" >&2
  fi
  # Suspensions the watcher couldn't resume keep the lock and their record for the next holder,
  # and fail the slot (exit 3), whatever the measurement's own status.
  if [ -f "$LOCK/frozen" ] || [ "$watched" != 0 ]; then
    echo "quiet slot: suspensions not resumed, kept in $LOCK/frozen" >&2
    exit 3
  fi
  rm -rf "$LOCK"
  exit "$status"
}
trap stop_watch EXIT
# What the measurement builds itself doesn't wait for the slot it holds.
NRESE_QUIET_HOLDER=1 "$@"
