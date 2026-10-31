#!/usr/bin/env bash
# The merge check's Windows part as a fleet job, on its host: what scripts/lib/windows-job.ps1 runs
# under Git Bash (scripts/lib/fleet-gate.sh windows submits it). Never run it by hand on a fleet
# host; only the fleet starts it.
#
#   windows-job.sh TREE
#
# It readies the Windows host for the tree's own scripts/check.sh windows and runs it in the job's
# checkout, once the checkout is TREE: the host's toolchain only (the tree's rust-toolchain.toml;
# never one rustup would install: no verdict without it), a target directory of its own in the
# checkout, never a shared one, and its temporary files in the checkout's tmp/windows-job: both go
# with the checkout. The suite contracts' Python packages come from an environment of their own in
# the host's cache, as the Linux gate's do (scripts/lib/suite-python.sh), never the host's Python.
#
# The fleet starts a Windows step without a usable stdin (fleet backlog I14): Python's subprocess
# and coreutils fail on it. The job says so after check.sh's summary, records it (`stdin` in
# gate.json) and runs with /dev/null.
#
# Results ($FLEET_OUT), as scripts/lib/gate-job.sh's: gate.log, the run's output; check.log,
# check.sh's; attempt, this attempt's name; gate.json, what it was (scripts/lib/gate-record.py).
# Exit: check.sh's (0 passed, 1 failed, 75 no verdict); 3 when the run couldn't be made.
set -u
tree=$1
: "${FLEET_OUT:?a fleet job}"
out=$(cygpath -m "$FLEET_OUT" 2> /dev/null || printf '%s' "$FLEET_OUT")

python() {
  if type -P python > /dev/null; then command python "$@"; else command python3 "$@"; fi
}
infra() { echo "windows-job: $*" >&2; exit 3; }

run() {
  type -P python > /dev/null || type -P python3 > /dev/null || infra "no Python on this host"
  # What the fleet gave as stdin, then /dev/null in its place.
  local stdin=valid
  python -c 'import os; os.fstat(0)' 2> /dev/null || stdin=invalid
  exec < /dev/null
  # This attempt's own result only (as gate-job.sh's).
  rm -f -- "${out:?}/gate.json" "${out:?}/check.log"
  local attempt="${FLEET_JOB:-none}:${FLEET_STEP:-0}:${EPOCHREALTIME/[.,]/}:$$"
  echo "$attempt" > "$out/attempt"

  [ "$(git rev-parse 'HEAD^{tree}' 2> /dev/null)" = "$tree" ] || infra "the checkout isn't tree $tree"
  # shellcheck source=gate-key.sh
  . scripts/lib/gate-key.sh
  # rustup's proxies would install a missing toolchain, with no deadline, on the host's own rustup.
  export RUSTUP_AUTO_INSTALL=0
  local identity
  identity=$(gate_identity) || infra "no toolchain here for the tree's rust-toolchain.toml"
  echo "=== windows part of tree $tree on $(hostname) ($identity)"

  # The suite's own Python environment, from the host's `python` (Windows' `python3` may be the
  # Store's stub): first on PATH, so check.sh's suite contracts and the record run with it.
  local cache=${NRESE_GATE_CACHE:-$HOME/fleet/cache/owlrs-gate} pybin
  # Git Bash's form (/c/…): a `C:/…` entry in PATH would split at its colon.
  cache=$(cygpath -u "$cache" 2> /dev/null || printf '%s' "$cache")
  # shellcheck source=suite-python.sh
  . scripts/lib/suite-python.sh
  pybin=$(suite_python "$cache" benches/suite/requirements.txt python) || infra "no Python for the suite"
  echo "$EPOCHSECONDS" > "${pybin%/*}/.last-used"
  export PATH="$pybin:$PATH"

  local temp
  mkdir -p tmp/windows-job/temp
  temp=$(cygpath -m "$PWD/tmp/windows-job/temp" 2> /dev/null || printf '%s' "$PWD/tmp/windows-job/temp")
  # Its own target directory, whatever the host sets: cargo decides freshness by mtimes, so two
  # checkouts building in one directory can hand a job the other's code (26 Oct 2026, lane D).
  CARGO_TARGET_DIR=$(cygpath -m "$PWD/target" 2> /dev/null || printf '%s' "$PWD/target")
  # The suite finds Git Bash by NRESE_BASH, never WSL's.
  NRESE_BASH=$(cygpath -m "$BASH" 2> /dev/null || printf '%s' "$BASH")
  export TMPDIR=$temp TEMP=$temp TMP=$temp CARGO_TARGET_DIR NRESE_BASH PYTHONUTF8=1
  local began=$SECONDS code
  # check.sh's own output, whole: its summary is the evidence, step by step.
  NRESE_ON_BUILD_HOST=1 bash scripts/check.sh windows 2>&1 | tee "$out/check.log"
  code=${PIPESTATUS[0]}
  [ $stdin = valid ] || echo "stdin: the fleet gave this step none it could use (fleet backlog I14); it ran with /dev/null"

  GATE_TIER=windows GATE_TREE=$tree GATE_LIST=$(git hash-object --stdin < /dev/null) GATE_IDENTITY=$identity \
    GATE_CODE=$code GATE_CPU=$(gate_cpu) GATE_HOST=$(hostname) \
    GATE_NEXTEST=$(cargo nextest --version 2> /dev/null | head -1) \
    GATE_CHANGED=$(git status --porcelain --untracked-files=no) GATE_SECONDS=$((SECONDS - began)) \
    GATE_JOB=${FLEET_JOB:-} GATE_FLEET_HOST=${FLEET_HOST:-} GATE_ATTEMPT=$attempt GATE_STDIN=$stdin \
    python scripts/lib/gate-record.py "$out" || infra "no record of the run"
  exit $code
}

run 2>&1 | tee "$out/gate.log"
exit "${PIPESTATUS[0]}"
