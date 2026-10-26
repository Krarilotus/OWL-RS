#!/usr/bin/env bash
# The merge check's Windows part as a fleet job, on its host: what scripts/lib/windows-job.ps1 runs
# under Git Bash, from a copy (scripts/lib/fleet-gate.sh windows submits it). Never run it by hand
# on a fleet host; only the fleet starts it.
#
#   windows-job.sh COMMIT TREE
#
# The job's checkout is the submitter's HEAD (this script's commit), which holds COMMIT. It checks
# COMMIT out there and checks that it is TREE, then runs every step, whatever an earlier one did:
# the W3C suites' data (required unless NRESE_W3C_OPTIONAL), the whole workspace built (all
# targets), all its tests, its doc tests and the suite contracts, through the tree's own scripts,
# with the host's toolchain (the tree's rust-toolchain.toml; never one rustup would install: no
# verdict without it) and Python. It builds in its checkout's target directory, never a shared
# one, and its temporary files go to the checkout's tmp/windows-job: both go with the checkout.
#
# The fleet starts a Windows step without a usable stdin (fleet backlog I14): Python's subprocess
# and coreutils fail on it. The job says so after its summary, records it (`stdin` in gate.json)
# and runs with /dev/null.
#
# Results ($FLEET_OUT), as scripts/lib/gate-job.sh's: gate.log, the run's output; attempt, this
# attempt's name; gate.json, what it was (tier windows, the tree, the toolchain and flags, the CPU,
# the host, the job and attempt, each step's exit code and seconds, whether tracked files changed,
# and `tool`, this script's commit). Exit: 0 passed, 1 a step failed; 3 no verdict: the run
# couldn't be made, or the PC guard held or stopped a build (scripts/cargo-guarded.sh's exit 75 is
# not the tree's failure).
set -u
commit=$1 tree=$2
out=$(cygpath -m "${FLEET_OUT:?a fleet job}" 2> /dev/null || printf '%s' "$FLEET_OUT")

python() {
  if type -P python > /dev/null; then command python "$@"; else command python3 "$@"; fi
}
infra() { echo "windows-job: $*" >&2; exit 3; }
declare -a results=()
step() {
  local name=$1 start=$SECONDS code
  shift
  echo "=== $name"
  "$@"
  code=$?
  results+=("$code  $name ($((SECONDS - start)) s)")
  echo "=== $name: exit $code"
}
workspace_tests() {
  if cargo nextest --version > /dev/null 2>&1; then
    bash scripts/cargo-guarded.sh nextest run --workspace --locked --no-fail-fast
  else
    bash scripts/cargo-guarded.sh test --workspace --locked --no-fail-fast --lib --bins --tests
  fi
}

run() {
  type -P python > /dev/null || type -P python3 > /dev/null || infra "no Python on this host"
  # What the fleet gave as stdin, then /dev/null in its place.
  local stdin=valid
  python -c 'import os; os.fstat(0)' 2> /dev/null || stdin=invalid
  exec < /dev/null
  # This attempt's own result only (as gate-job.sh's).
  rm -f -- "$out/gate.json"
  local attempt="${FLEET_JOB:-none}:${FLEET_STEP:-0}:${EPOCHREALTIME/[.,]/}:$$"
  echo "$attempt" > "$out/attempt"

  local tool
  tool=$(git rev-parse HEAD) || infra "not in the job's checkout"
  # shellcheck source=gate-key.sh
  . scripts/lib/gate-key.sh || infra "no scripts/lib/gate-key.sh at $tool"
  git checkout -q --detach "$commit" || infra "no checkout of $commit here"
  [ "$(git rev-parse 'HEAD^{tree}')" = "$tree" ] || infra "the checkout isn't tree $tree"
  # rustup's proxies would install a missing toolchain, with no deadline, on the host's own rustup.
  export RUSTUP_AUTO_INSTALL=0
  local identity
  identity=$(gate_identity) || infra "no toolchain here for the tree's rust-toolchain.toml"
  echo "=== windows part of $commit (tree $tree) on $(hostname) ($identity), by $tool"

  local temp
  mkdir -p tmp/windows-job/temp
  temp=$(cygpath -m "$PWD/tmp/windows-job/temp" 2> /dev/null || printf '%s' "$PWD/tmp/windows-job/temp")
  # Its own target directory, whatever the host sets: cargo decides freshness by mtimes, so two
  # checkouts building in one directory can hand a job the other's code (26 Oct 2026, lane D).
  CARGO_TARGET_DIR=$(cygpath -m "$PWD/target" 2> /dev/null || printf '%s' "$PWD/target")
  # The suite finds Git Bash by NRESE_BASH, never WSL's.
  NRESE_BASH=$(cygpath -m "$BASH" 2> /dev/null || printf '%s' "$BASH")
  export TMPDIR=$temp TEMP=$temp TMP=$temp CARGO_TARGET_DIR NRESE_BASH CARGO_BUILD_WARNINGS=deny PYTHONUTF8=1
  local began=$SECONDS
  if [ -n "${NRESE_W3C_OPTIONAL:-}" ]; then
    unset NRESE_W3C_REQUIRED
  else
    export NRESE_W3C_REQUIRED=1
    step "W3C data" bash scripts/fetch-w3c-tests.sh
  fi
  step build bash scripts/cargo-guarded.sh build --workspace --all-targets --locked
  step test workspace_tests
  step "doc tests" bash scripts/cargo-guarded.sh test --workspace --locked --doc --no-fail-fast
  step "suite contracts" python benches/suite/tests/isolation.py discover -s benches/suite/tests

  local result code=0
  for result in "${results[@]}"; do
    case ${result%% *} in
      0) ;;
      75) code=3 ;;
      *) [ $code -eq 3 ] || code=1 ;;
    esac
  done
  echo
  echo "=== summary"
  printf '  %s\n' "${results[@]}"
  [ $stdin = valid ] || echo "stdin: the fleet gave this step none it could use (fleet backlog I14); it ran with /dev/null"
  [ $code -ne 3 ] || echo "the PC guard held or stopped a build (exit 75): no verdict, not the tree's failure"

  GATE_TREE=$tree GATE_LIST=$(git hash-object --stdin < /dev/null) GATE_IDENTITY=$identity GATE_CODE=$code \
    GATE_CPU=$(gate_cpu) GATE_HOST=$(hostname) GATE_NEXTEST=$(cargo nextest --version 2> /dev/null | head -1) \
    GATE_CHANGED=$(git status --porcelain --untracked-files=no) GATE_SECONDS=$((SECONDS - began)) \
    GATE_JOB=${FLEET_JOB:-} GATE_FLEET_HOST=${FLEET_HOST:-} GATE_ATTEMPT=$attempt GATE_TOOL=$tool GATE_STDIN=$stdin \
    python - "$out" "${results[@]}" << 'EOF'
import json, os, re, sys
e, out = os.environ, sys.argv[1]
steps = []
for line in sys.argv[2:]:
    m = re.match(r"^(\d+)  (.*) \((\d+) s\)$", line)
    steps.append({"name": m.group(2), "code": int(m.group(1)), "seconds": int(m.group(3))})
json.dump({
    "tier": "windows", "tree": e["GATE_TREE"], "list_hash": e["GATE_LIST"],
    "identity": e["GATE_IDENTITY"], "w3c_optional": bool(e.get("NRESE_W3C_OPTIONAL")),
    "cpu": e["GATE_CPU"], "host": e["GATE_HOST"], "nextest": e["GATE_NEXTEST"],
    "job": e["GATE_JOB"], "fleet_host": e["GATE_FLEET_HOST"], "attempt": e["GATE_ATTEMPT"],
    "summary": True, "steps": steps, "code": int(e["GATE_CODE"]), "tracked_changes": e["GATE_CHANGED"],
    "seconds": int(e["GATE_SECONDS"]), "tool": e["GATE_TOOL"], "stdin": e["GATE_STDIN"],
}, open(f"{out}/gate.json", "w", encoding="utf-8"), indent=1)
EOF
  exit $code
}

run 2>&1 | tee "$out/gate.log"
exit "${PIPESTATUS[0]}"
