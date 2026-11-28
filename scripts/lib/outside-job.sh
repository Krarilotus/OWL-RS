#!/usr/bin/env bash
# The nightly run's outside check as a fleet job, on its host: the step scripts/lib/fleet-gate.sh nightly
# submits. It runs the commit's own scripts/check.sh nightly-outside in the job's checkout (its target
# directory beside it) and keeps what a gate job keeps, as scripts/lib/gate-job.sh does: check.sh's output
# as check.log (the fleet keeps a job's files, not what its step prints), attempt, this attempt's name, and
# gate.json, what it was (scripts/lib/gate-record.py), its tier nightly-outside.
#
#   scripts/lib/outside-job.sh TREE LIST_HASH IDENTITY
#
# Exit: check.sh's; 3 when check.sh passed but gate.json could not be written.
set -u
tree=$1 list_hash=$2 identity=$3
out=${FLEET_OUT:?a fleet job}
rm -f -- "$out/gate.json" "$out/check.log"
attempt="${FLEET_JOB:-none}:${FLEET_STEP:-0}:${EPOCHREALTIME/[.,]/}:$$"
echo "$attempt" > "$out/attempt"
export PATH="$HOME/.cargo/bin:$PATH" CARGO_TARGET_DIR=$PWD/../target-job
began=$SECONDS
bash scripts/check.sh nightly-outside 2>&1 | tee "$out/check.log"
code=${PIPESTATUS[0]}
# shellcheck source=gate-key.sh
. scripts/lib/gate-key.sh
GATE_TIER=nightly-outside GATE_TREE=$tree GATE_LIST=$list_hash GATE_IDENTITY=$identity GATE_CODE=$code \
  GATE_CPU=$(gate_cpu) GATE_HOST=$(hostname) GATE_NEXTEST=none \
  GATE_CHANGED=$(git status --porcelain --untracked-files=no 2> /dev/null) GATE_SECONDS=$((SECONDS - began)) \
  GATE_JOB=${FLEET_JOB:-} GATE_FLEET_HOST=${FLEET_HOST:-} GATE_ATTEMPT=$attempt \
  python3 scripts/lib/gate-record.py "$out" || { echo "outside-job: gate.json not written" >&2; [ "$code" -ne 0 ] || code=3; }
exit $code
