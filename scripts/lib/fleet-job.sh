# A job of this repository on the fleet: submit, follow, fetch (sourced by scripts/lib/fleet-gate.sh).
#
# The fleet's CLI: NRESE_FLEET_BIN, else git config nrese.fleetBin, else the main PC's. Its owner
# is FLEET_OWNER (your own thread: fleet-scheduler/skills/fleet-jobs). Every call has a deadline.
#
#   fleet_submit FILE             submits the job FILE; sets fleet_id (empty: refused, why on stderr)
#   fleet_follow START_S MAX_S    until the job ends; 0 done or failed (fleet_state says which,
#                                 fleet_host where it ran), 75
#                                 when no host started it within START_S (again after a host
#                                 stopped it), it ran past MAX_S, was cancelled or blocked, or the
#                                 fleet stopped answering; the job is cancelled then
#   fleet_results                 the directory its results were fetched to
#   fleet_cancel                  cancels it (also on an interrupt)
#   q TEXT                        TEXT as a TOML string
fleet_bin=${NRESE_FLEET_BIN:-$(git config --get nrese.fleetBin || echo C:/Users/Johannes/Documents/fleet-scheduler/state/owlrs/bin/fleet.exe)}
fleet_id=""
fleet_state=""
# The fleet's name for the host the job last ran on (fleet_follow).
fleet_host=""
fleet_label=${fleet_label:-fleet job}

python() {
  if type -P python > /dev/null; then command python "$@"; else command python3 "$@"; fi
}
fleet_say() { echo "$fleet_label${fleet_id:+ $fleet_id}: $*" >&2; }
fleet_cancel() { [ -z "$fleet_id" ] || timeout --kill-after=10 60 "$fleet_bin" cancel "$fleet_id" > /dev/null 2>&1; }
trap 'fleet_say interrupted; fleet_cancel; exit 130' INT TERM

q() { local s=${1//\\/\\\\}; printf '"%s"' "${s//\"/\\\"}"; }

fleet_ready() {
  [ -n "${FLEET_OWNER:-}" ] || { fleet_say "no FLEET_OWNER (your own thread: fleet-scheduler/skills/fleet-jobs)"; return 1; }
  [ -x "$fleet_bin" ] || type -P "$fleet_bin" > /dev/null || { fleet_say "no fleet CLI at $fleet_bin"; return 1; }
}

# This checkout as a git input's path.
fleet_repo() { cygpath -m "$PWD" 2> /dev/null || echo "$PWD"; }

fleet_submit() {
  local err
  err=$(mktemp)
  fleet_id=$(timeout --kill-after=10 60 "$fleet_bin" submit "$1" 2> "$err" | tr -d '\r' | head -1)
  [ -n "$fleet_id" ] || fleet_say "the fleet refused it: $(tr -d '\r' < "$err" | tail -3)"
  rm -f "$err"
  [ -n "$fleet_id" ]
}

fleet_follow() {
  local start_s=$1 max_s=$2 poll_s=${NRESE_FLEET_POLL_S:-10}
  local start_by=$((SECONDS + start_s)) end_by=$((SECONDS + max_s)) last="" silent=0 now
  while :; do
    now=$(timeout --kill-after=10 30 "$fleet_bin" status "$fleet_id" --json 2> /dev/null | python -c '
import json, sys
j = json.load(sys.stdin)
print(j["state"], j.get("host") or "-", j.get("reason") or "")' 2> /dev/null | tr -d '\r')
    fleet_state=${now%% *}
    case $now in *" - "* | "") ;; *) fleet_host=$(printf '%s' "$now" | awk '{ print $2 }') ;; esac
    if [ -z "$now" ]; then
      silent=$((silent + 1))
      [ $silent -lt 6 ] || { fleet_say "the fleet doesn't answer"; fleet_cancel; return 75; }
    else
      silent=0
      [ "$now" = "$last" ] || fleet_say "$now"
      last=$now
    fi
    case $fleet_state in
      running) start_by=$((SECONDS + start_s)) ;;
      pending | staging)
        [ $SECONDS -lt $start_by ] || { fleet_say "no host started it within $start_s s"; fleet_cancel; return 75; } ;;
      done | failed) return 0 ;;
      cancelled | blocked) fleet_say "it was $fleet_state"; fleet_id=""; return 75 ;;
    esac
    [ $SECONDS -lt $end_by ] || { fleet_say "past $max_s s"; fleet_cancel; return 75; }
    sleep "$poll_s"
  done
}

fleet_results() { timeout --kill-after=10 300 "$fleet_bin" results "$fleet_id" 2> /dev/null | tr -d '\r' | tail -1; }
