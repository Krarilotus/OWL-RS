# A job of this repository on the fleet: submit, follow, fetch (sourced by scripts/lib/fleet-gate.sh).
#
# The fleet's CLI: NRESE_FLEET_BIN, else git config nrese.fleetBin, else the development machine's. Its owner
# is FLEET_OWNER (your own thread: fleet-scheduler/skills/fleet-jobs). Every call has a deadline.
#
#   fleet_submit FILE             submits the job FILE; sets fleet_id (empty: refused, why on stderr)
#   fleet_follow START_S MAX_S [EST_S]
#                                 until the job ends; 0 done or failed (fleet_state says which,
#                                 fleet_host where it ran), 75
#                                 when no host started it within START_S (again after a host
#                                 stopped it), it ran past MAX_S, was cancelled or blocked, or the
#                                 fleet stayed silent too long; the job is cancelled then. While
#                                 the fleet expects a pending job to start at a time E (its status'
#                                 expected_start) from which a run of EST_S seconds (default 0) ends
#                                 within MAX_S, the start window reaches at least to E + START_S; a
#                                 later E moves it on, an earlier one never brings it back. An answer
#                                 with no E it can read (none, malformed, or no answer at all) moves
#                                 nothing: only the fixed window holds then.
#   fleet_results                 the directory its results were fetched to (nothing: none came)
#   fleet_cancel                  cancels it, once (also on an interrupt)
#   q TEXT                        TEXT as a TOML string
#
# Silence is a call with no answer: no reply, a call that failed (whatever it printed), a timeout or
# no state. It is unknown, not an
# end: the job is neither judged nor cancelled for it, and a pending job's start window doesn't run
# meanwhile. A spell of it ends the wait only once it has lasted NRESE_FLEET_SILENT_S seconds
# (default 600) or MAX_S has passed; then the job is cancelled. Both are checked before every fleet
# call, again when it returns, and before every sleep: no call starts and no sleep begins after
# either, and an answer that came after either isn't taken. A job is never submitted again.
fleet_bin=${NRESE_FLEET_BIN:-$(git config --get nrese.fleetBin || echo ${HOME:+"$HOME"/Documents/fleet-scheduler/state/owlrs/bin/fleet.exe})}
fleet_id=""
fleet_state=""
# The fleet's name for the host the job last ran on (fleet_follow).
fleet_host=""
# fleet_follow's end (MAX_S) in SECONDS, which also bounds fleet_results, and MAX_S itself; the job
# last cancelled.
fleet_end_by=0
fleet_max_s=0
fleet_cancelled=""
fleet_label=${fleet_label:-fleet job}

python() {
  if type -P python > /dev/null; then command python "$@"; else command python3 "$@"; fi
}
fleet_say() { echo "$fleet_label${fleet_id:+ $fleet_id}: $*" >&2; }
fleet_cancel() {
  [ -z "$fleet_id" ] || [ "$fleet_id" = "$fleet_cancelled" ] || {
    timeout --kill-after=10 60 "$fleet_bin" cancel "$fleet_id" > /dev/null 2>&1
    fleet_cancelled=$fleet_id
  }
}
trap 'fleet_say interrupted; fleet_cancel; exit 130' INT TERM

# A TOML basic string: backslashes and quotes escaped, and the line breaks and tabs a pasted
# statement may hold (a literal one is invalid TOML). No other control character is passed here.
q() {
  local s=${1//\\/\\\\}
  s=${s//\"/\\\"}
  s=${s//$'\n'/\\n}
  s=${s//$'\r'/\\r}
  printf '"%s"' "${s//$'\t'/\\t}"
}

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
  local start_s=$1 max_s=$2 est_s=${3:-0} poll_s=${NRESE_FLEET_POLL_S:-10}
  local start_by=$((SECONDS + start_s)) last="" quiet="" asked now reply expect said="" late e
  fleet_end_by=$((SECONDS + max_s)) fleet_max_s=$max_s
  while :; do
    fleet_may_wait "$quiet" || { fleet_cancel; return 75; }
    asked=$SECONDS
    # A failed call is silence, whatever it printed. The answer's first line: its expected start
    # (epoch seconds, then its local clock time with the basis in brackets: the fleet CLI gives a
    # local time without a zone, on this machine), or `-`; its second: state, host, reason.
    now="" reply="" expect=-
    reply=$(timeout --kill-after=10 30 "$fleet_bin" status "$fleet_id" --json 2> /dev/null) &&
      now=$(printf '%s\n' "$reply" | python -c '
import datetime, json, sys
j = json.load(sys.stdin)
if j.get("state"):
    try:
        at = datetime.datetime.fromisoformat(j["expected_start"])
        basis = j.get("basis") or ""
        print(int(at.timestamp()), at.strftime("%H:%M") + (f" ({basis})" if basis else ""))
    except (KeyError, TypeError, ValueError, OverflowError, OSError):
        print("-")
    print(j["state"], j.get("host") or "-", j.get("reason") or "")' 2> /dev/null | tr -d '\r')
    case $now in
      *$'\n'*) expect=${now%%$'\n'*} now=${now#*$'\n'} ;;
      *) now="" ;;
    esac
    [ -n "$now" ] || quiet=${quiet:-$asked}
    # An answer counts only if it came inside both deadlines.
    fleet_may_wait "$quiet" || { fleet_cancel; return 75; }
    if [ -n "$now" ]; then
      [ -z "$quiet" ] || start_by=$((start_by + SECONDS - quiet))
      quiet=""
      fleet_state=${now%% *}
      case $now in *" - "*) ;; *) fleet_host=$(printf '%s' "$now" | awk '{ print $2 }') ;; esac
      [ "$now" = "$last" ] || fleet_say "$now"
      last=$now
      case $fleet_state in
        running) start_by=$((SECONDS + start_s)) ;;
        pending | staging)
          # The fleet's expected start, in SECONDS' terms: it holds the window open only if a run
          # started then ends within MAX_S.
          late=""
          if [ "$expect" != - ]; then
            e=$((SECONDS + ${expect%% *} - EPOCHSECONDS))
            if [ $((e + est_s)) -le $fleet_end_by ]; then
              [ $((e + start_s)) -le $start_by ] || start_by=$((e + start_s))
              [ "${expect#* }" = "$said" ] || { said=${expect#* }; fleet_say "expected start $said"; }
            else
              late=" (the fleet expects it at $(printf '%s' "$expect" | awk '{ print $2 }'), past this gate's limit)"
            fi
          fi
          [ $SECONDS -lt $start_by ] || { fleet_say "no host started it within $start_s s$late"; fleet_cancel; return 75; } ;;
        done | failed) return 0 ;;
        cancelled | blocked) fleet_say "it was $fleet_state"; fleet_id=""; return 75 ;;
      esac
    fi
    # And before the sleep: taking the answer in can itself outlast MAX_S on a slow host.
    fleet_may_wait "$quiet" || { fleet_cancel; return 75; }
    sleep "$poll_s"
  done
}

# fleet_may_wait [SINCE]: whether the wait may go on: MAX_S not passed and, where a silence began at
# SINCE (in SECONDS), that silence still under its cap. Asked before every fleet call, after it
# returns (before its answer is taken), and before every sleep.
fleet_may_wait() {
  local since=${1:-}
  if [ $SECONDS -ge $fleet_end_by ]; then
    fleet_say "past $fleet_max_s s"
  elif [ -n "$since" ] && [ $((SECONDS - since)) -ge "${NRESE_FLEET_SILENT_S:-600}" ]; then
    fleet_say "the fleet didn't answer for $((SECONDS - since)) s"
  else
    return 0
  fi
  return 1
}

fleet_results() {
  local quiet="" asked dir got
  while fleet_may_wait "$quiet"; do
    asked=$SECONDS got=""
    if dir=$(timeout --kill-after=10 300 "$fleet_bin" results "$fleet_id" 2> /dev/null) && dir=${dir//$'\r'/}; then
      got=${dir##*$'\n'}
    fi
    [ -n "$got" ] || quiet=${quiet:-$asked}
    # Results count only if they came inside both deadlines.
    fleet_may_wait "$quiet" || return 75
    [ -z "$got" ] || { echo "$got"; return; }
    sleep "${NRESE_FLEET_POLL_S:-10}"
  done
  return 75
}
