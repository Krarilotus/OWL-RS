#!/usr/bin/env bash
# The push tier of a pushed commit, or the merge check's Windows part, as a fleet job
# (scripts/check.sh push, scripts/check.sh windows).
#
#   scripts/lib/fleet-gate.sh MODE LIST TREE LIST_HASH IDENTITY WHERE COMMIT
#
# MODE push: submits a job of kind owlrs.gate that runs the commit's own scripts/lib/gate-job.sh on
# COMMIT (its tree TREE) with the changed files of LIST. The job yields to timed work: never
# started beside it, stopped when a timed job of higher priority wants its host. It runs on a
# Linux host with the test role.
# MODE windows (LIST empty): a job of kind owlrs.any on a Windows host that can run the OWL-RS
# Windows suite (`has = ["owlrs-windows-suite"]`, declared in the fleet's config), which runs the
# commit's own scripts/lib/windows-job.ps1 and through it its scripts/check.sh windows.
# NRESE_GATE_HOSTS: only these hosts. It follows the job and reads its result: valid only when it
# is for TREE, LIST_HASH, this tier and IDENTITY (the toolchain and the flags,
# scripts/lib/gate-key.sh) and left no tracked file changed.
#
# The merge check's jobs are the fleet's merge gates (`priority_class = "merge-gate"`, a class its
# config ranks): either tier's job when COMMIT is for refactor/engine-v2, i.e. NRESE_PUSH_REF, the
# ref it goes to, is refs/heads/refactor/engine-v2: the pre-push hook's, or by hand the caller's
# word (a merge check run before its push states it for both tiers). A package's push or Windows
# check isn't one, a merge among its commits or not (Sol, merge-gate-class r1; PR #69 r1 R1), nor a
# check by hand that names no ref.
#
# A failure on a host whose failures don't count alone (NRESE_GATE_UNCONFIRMED_HOSTS, default
# khaleesi and khaleesi-wsl: the owner, 20 Oct 2026, its RAM never formally verified) is reported
# as unconfirmed and the gate runs again on another host (NRESE_GATE_CONFIRM_HOSTS, default
# office-pc, phuoc-yu, gamergrill; for windows kraris-gptler). The tree fails only if that run
# fails too; if it passes, the pass counts and both jobs are named as a possible fault of the
# first host.
#
# Exit: 0 passed (WHERE then names the job, its host and the CPU), 1 failed (its log is shown),
# 75 no verdict: no FLEET_OWNER, the fleet refused or didn't answer, no host started it within
# NRESE_GATE_START_S (default 180 s; again after a host stopped it), it ran past NRESE_GATE_MAX_S
# (default 10800 s), or its result is missing or for something else. The job is cancelled then,
# and nothing runs here instead: no verdict, no push. For windows the start window is 1800 s and
# the ask 4 cores, measured: a 4-core run took 899 s and started within 11 min, where a 6-core
# one hadn't started in 25 (PR #66's, 31 Oct 2026: the host's console cap gives the fleet 8 of its
# 16 threads, its owner's own load 2-6 of those); 6 cores took 696 s. The build and the tests are
# what more cores speed up, not the suite contracts (466 s of the 899).
set -u
mode=$1 list=$2 tree=$3 list_hash=$4 identity=$5 where=$6 commit=$7
cd "$(git rev-parse --show-toplevel)" || exit 75
export NRESE_FLEET_POLL_S=${NRESE_GATE_POLL_S:-10}
fleet_label="fleet gate"
# shellcheck source=fleet-job.sh
. scripts/lib/fleet-job.sh
start_s=${NRESE_GATE_START_S:-180}
max_s=${NRESE_GATE_MAX_S:-10800}
unconfirmed=" ${NRESE_GATE_UNCONFIRMED_HOSTS:-khaleesi khaleesi-wsl} "
no_verdict() {
  fleet_say "$*: no verdict, so no pass"
  fleet_cancel
  exit 75
}

fleet_ready || no_verdict "no fleet to ask"
[ "$(git rev-parse "$commit^{tree}" 2> /dev/null)" = "$tree" ] || no_verdict "commit $commit isn't tree $tree"

# What a tier asks for (cores also bound its compilers; the step's slice caps its memory on
# Linux, scripts/cargo-guarded.sh's job object on Windows).
os=linux kind=owlrs.gate confirm="office-pc phuoc-yu gamergrill"
reason="the $mode gate of a pushed commit (scripts/check.sh); no push without it"
case $mode in
  push) cores=${NRESE_GATE_CORES:-8} ram=${NRESE_GATE_RAM_GB:-12} est=${NRESE_GATE_EST_MIN:-40} ;;
  windows)
    # 9 GB: the suite's measured peak is 7.5 GB (five runs on kraris-gptler). The fleet admits a job
    # there only with 16 GB available and its ask within available less the 6.3 GB headroom, so a
    # 9 GB job admitted at 16 GB that peaks at 7.5 GB still leaves 8.5 GB, clear of the 4 GB
    # eviction floor (the fleet's owner, 27 Oct 2026). 16 GB kept checks waiting while 11-15 GB
    # was free.
    # est: the 4-core run's 899 s with clippy's 38 s, x 1.5.
    cores=${NRESE_GATE_CORES:-4} ram=${NRESE_GATE_RAM_GB:-9} est=${NRESE_GATE_EST_MIN:-24}
    os=windows kind=owlrs.any confirm=kraris-gptler start_s=${NRESE_GATE_START_S:-1800}
    reason="the merge check's Windows part (scripts/check.sh windows); no merge without it" ;;
  *) cores=${NRESE_GATE_CORES:-2} ram=${NRESE_GATE_RAM_GB:-4} est=${NRESE_GATE_EST_MIN:-10} ;;
esac
priority=${NRESE_GATE_PRIORITY:-600}
# The merge check's jobs (above): either tier's, for a commit that goes to engine-v2.
class=""
if [ "${NRESE_PUSH_REF:-}" = refs/heads/refactor/engine-v2 ]; then
  class=merge-gate
fi
job=$(mktemp)
trap 'rm -f "$job"' EXIT

# gate HOSTS: one gate job (on one of HOSTS, if any), followed to its end; sets verdict, log, host.
gate() {
  {
    echo "id = $(q "gate-$mode-${tree:0:10}-$(date +%s)")"
    echo 'group = "gate"'
    echo "kind = $(q "$kind")"
    # 600 by default; a gate someone waits for may say so (NRESE_GATE_PRIORITY with its reason).
    echo "priority = $priority"
    echo "priority_reason = $(q "${NRESE_GATE_REASON:-$reason}")"
    [ -z "$class" ] || echo "priority_class = $(q "$class")"
    echo "est_min = $est"
    if [ "$mode" = windows ]; then
      printf 'steps = [["powershell", "-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-File", %s, %s]]\n' \
        '"scripts/lib/windows-job.ps1"' "$(q "$tree")"
    else
      printf 'steps = [["bash", "scripts/lib/gate-job.sh", %s, %s, %s, "--"' "$(q "$mode")" "$(q "$tree")" "$(q "$list_hash")"
      while IFS= read -r file; do printf ', %s' "$(q "$file")"; done < "$list"
      echo ']]'
    fi
    echo '[env]'
    # Its tests too: the fleet caps a slice's memory, not its CPUs, and a host shared with its
    # people (a presence cap) gets no more than the ask.
    echo "CARGO_BUILD_JOBS = $(q "$cores")"
    echo "NEXTEST_TEST_THREADS = $(q "${NEXTEST_TEST_THREADS:-$cores}")"
    echo "RUST_TEST_THREADS = $(q "$cores")"
    # Windows has no slice: the build's own job object holds it to the ask.
    [ "$mode" != windows ] || echo "NRESE_MEMORY_CAP_GB = $(q "$ram")"
    for var in NRESE_TARGET_CPU RUSTFLAGS NRESE_W3C_OPTIONAL NRESE_GATE_BUDGET_GB; do
      [ -z "${!var:-}" ] || echo "$var = $(q "${!var}")"
    done
    echo '[requires]'
    echo "os = $(q "$os")"
    echo "cores = $cores"
    echo "ram_gb = $ram"
    # A host that can run the suite there (its Python packages, Git Bash), which its config declares.
    [ "$mode" != windows ] || echo 'has = ["owlrs-windows-suite"]'
    if [ -n "$1" ]; then
      printf 'hosts = ['
      local h sep=""
      for h in $1; do printf '%s%s' "$sep" "$(q "$h")"; sep=", "; done
      echo ']'
    fi
    echo '[[inputs]]'
    echo 'name = "checkout"'
    echo "source = { git = $(q "$(fleet_repo)"), rev = $(q "$commit") }"
  } > "$job"
  fleet_submit "$job" || no_verdict "the fleet refused the job"
  fleet_say "submitted (priority $priority, asks $cores cores and $ram GB${1:+, on $1})"
  fleet_follow "$start_s" "$max_s" || no_verdict "no result"
  host=$fleet_host
  local results out
  results=$(fleet_results)
  out=$(find "$results" -name gate.json 2> /dev/null | head -1)
  [ -n "$out" ] || no_verdict "its result is missing (it ended $fleet_state)"
  log="$(dirname "$out")/gate.log"
  # A verdict only from a whole, consistent record of this job's last attempt where the fleet says
  # it ran (Sol, review-remote-gates-r1-20261020): a pass needs the job done, check.sh's exit 0 and
  # its summary with every step at 0; a failure needs the job failed, exit 1 and a step that
  # failed. Anything else, or a record of another job, host or attempt, is no verdict.
  verdict=$(python - "$out" "$mode" "$tree" "$list_hash" "$identity" "${NRESE_W3C_OPTIONAL:+1}"       "$fleet_state" "$fleet_id" "$fleet_host" "$(dirname "$out")/attempt" << 'EOF'
import json, sys
path, mode, tree, list_hash, identity, w3c, state, job, host, attempt_file = sys.argv[1:11]
r = json.load(open(path, encoding="utf-8"))
try:
    attempt = open(attempt_file, encoding="utf-8").read().strip()
except OSError:
    attempt = None
want = {"tier": mode, "tree": tree, "list_hash": list_hash, "identity": identity,
        "w3c_optional": bool(w3c), "tracked_changes": "", "job": job, "fleet_host": host,
        "attempt": attempt, "summary": True}
wrong = [f"{k} {r.get(k)!r}, not {v!r}" for k, v in want.items() if r.get(k) != v]
steps = r.get("steps") or []
failing = [s["name"] for s in steps if s.get("code") != 0]
if wrong:
    print("invalid " + "; ".join(wrong))
elif not steps:
    print("invalid no steps in its summary")
elif state == "done" and r.get("code") == 0 and not failing:
    print(f"passed {r['fleet_host']} cpu={r['cpu']}")
elif state == "failed" and r.get("code") == 1 and failing:
    print(f"failed on {r['fleet_host']}: " + ", ".join(failing))
else:
    print(f"invalid the job {state}, check.sh's exit {r.get('code')!r}, failed steps {failing}: they disagree")
EOF
  )
  case $verdict in passed* | failed*) ;; *) no_verdict "its result doesn't count: ${verdict#invalid }" ;; esac
}

gate "${NRESE_GATE_HOSTS:-}"
first=$fleet_id first_host=$host recheck=""
if [[ $verdict == failed* && $unconfirmed == *" $host "* ]]; then
  sed -n '/^=== summary/,$p' "$log"
  fleet_say "failed on $host: unconfirmed ($host); running it again on another host (log: $log)"
  fleet_id=""
  gate "${NRESE_GATE_CONFIRM_HOSTS:-$confirm}"
  case $verdict in
    passed*)
      recheck="fleet:$first@$first_host (failed there)"
      fleet_say "passed here after failing on $first_host ($first): a possible fault of $first_host, not of the tree" ;;
    failed*) fleet_say "failed again on $host, so the failure is the tree's (first $first on $first_host)" ;;
  esac
fi
case $verdict in
  passed*)
    sed -n '/^=== summary/,$p' "$log"
    fleet_say "passed on ${verdict#passed }; its log: $log"
    # Where it passed; after a re-check, also the run it re-checked (its job and host).
    echo "fleet:$fleet_id@${verdict#passed }${recheck:+ re-checking $recheck}" > "$where"
    exit 0
    ;;
  *)
    cat "$log"
    fleet_say "${verdict#failed }: the tree fails the gate (log: $log)."
    exit 1
    ;;
esac
