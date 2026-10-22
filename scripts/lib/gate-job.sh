#!/usr/bin/env bash
# Existing Linux gate workload and native Windows OS/measure adapter, one bounded complete
# part per official fleet job. Source inputs are git only. Outputs are logs/JSON only.
set -euo pipefail
export PYTHONDONTWRITEBYTECODE=1
mode=${1:?} tree=${2:?} list_hash=${3:?} part=${4:-} index=${5:-} key=${6:-} base=${7:-} flags=${8:-} rustc_commit=${9:-}
# Host-side veto comes before provisioning, building or placing anything in FLEET_OUT.
# This push adapter never exports executables, on any host; cargo mode is not a gate.
if [ "$mode" != push ]; then echo "gate-job: no export/build mode in the push adapter" >&2; exit 75; fi
shift 9
[ "${1:-}" = -- ] || exit 75
shift
[ -n "${FLEET_JOB:-}" ] && [ -n "${FLEET_STEP:-}" ] || exit 75
if [ "${NRESE_GATE_BOUNDED:-}" != 1 ]; then
  export NRESE_GATE_BOUNDED=1
  # A complete part includes setup, lock and shutdown. A timeout gives no verdict and
  # replays the part; it is not a resumable checkpoint. Do not omit unfinished tests.
  exec timeout --kill-after=30 "${NRESE_GATE_UNIT_S:-3000}" bash "$0" "$mode" "$tree" "$list_hash" "$part" "$index" "$key" "$base" "$flags" "$rustc_commit" -- "$@" < /dev/null
fi
infra() { echo "gate-job: no verdict: $*" >&2; exit 75; }
python() { if type -P python > /dev/null; then command python "$@"; else command python3 "$@"; fi; }
out=${FLEET_OUT:?}
if [ "$part" = windows ]; then out=$(cygpath -u "$out"); fi
mkdir -p "$out"
exec > >(tee "$out/unit-$index.log") 2>&1
rm -f -- "$out/unit-$index.json"
attempt="${FLEET_JOB}:${FLEET_STEP}:${EPOCHREALTIME/[.,]/}:$$"
# Fleet stores attempt boundaries in epoch seconds. Start strictly after its admission
# second so even a same-second restart cannot consume preceding output.
sleep 1
began=$(python -c 'import time; print(int(time.time()))')
[ "$FLEET_STEP" = "$((index + 1))" ] || infra "fleet step differs"
[ "$key" = "${NRESE_GATE_REQUEST:-}" ] || infra "request differs"
read -r cores ram < <(python - "$index" <<'PY'
import json, os, sys
n = json.loads(os.environ['NRESE_GATE_NEEDS'])[int(sys.argv[1])]
assert n['cores'] > 0 and n['ram_gb'] > 0
print(n['cores'], n['ram_gb'])
PY
)
[ -n "${cores:-}" ] && [ -n "${ram:-}" ] || infra "resource request missing"
export CARGO_BUILD_JOBS=$cores NEXTEST_TEST_THREADS=$cores RUST_TEST_THREADS=$cores
export NRESE_MEMORY_CAP_GB=$ram NRESE_BUILD_PEAK_GB=$ram
export CARGO_BUILD_WARNINGS=deny NRESE_W3C_REQUIRED=1
unset NRESE_W3C_OPTIONAL CARGO_ENCODED_RUSTFLAGS
export DOCKER_HOST=unix:///nonexistent/owlrs-gate-has-no-docker.sock
memory_job=""
if [ "$part" = windows ]; then
  [ -r "/proc/$$/winpid" ] || infra "not native Windows Git Bash"
  [ -n "${NRESE_GATE_CAP_FILE:-}" ] || infra "no native workload wrapper"
  cap_file=$(cygpath -u "$NRESE_GATE_CAP_FILE")
  # Wait only for this official step's memory-cap handshake, before provisioning workers.
  until [ -s "$cap_file" ]; do [ "$SECONDS" -lt 60 ] || infra "memory-cap handshake deadline"; sleep 1; done
  memory_job=$(cat "$cap_file")
  memory_job=${memory_job//$'\r'/}
  [ -n "$memory_job" ] || infra "no named memory job"
fi
cache=${NRESE_GATE_CACHE:-$HOME/fleet/cache/owlrs-gate}
mkdir -p "$cache/bin"
holding=""
cleanup() { [ -z "$holding" ] || rmdir "$cache/windows.lock"; }
trap cleanup EXIT
if [ "$part" = windows ]; then
  mkdir "$cache/windows.lock" || infra "Windows cache owned by another unit (or interrupted owner; inspect before clearing)"
  holding=1
elif [ "$part" = rest ]; then
  exec 9> "$cache/lock"
  flock -w 60 9 || infra "cache lock deadline"
else infra "unknown part"; fi
export PATH="$cache/bin:$HOME/fleet/tools/bin:$HOME/.cargo/bin:$PATH"
tool=$PWD
[ "$(git rev-parse HEAD)" = "${NRESE_GATE_HEAD:?}" ] || infra "input head differs"
[ "$(git rev-parse 'HEAD^{tree}')" = "$tree" ] || infra "input is not the complete source tree"
[ "$(cat scripts/lib/push-base.txt)" = "$base" ] || infra "published comparison base differs"
[ "$(bash scripts/lib/remote.sh tree)" = "$tree" ] || infra "input checkout changed"
repo=$(git rev-parse --path-format=absolute --git-common-dir)
sha=$(git rev-parse HEAD)
wt="$cache/wt"
[ "$part" != windows ] || wt="$cache/wt-windows"
# No inherited GIT_DIR/GIT_INDEX_FILE may redirect a cache checkout into the source input.
unset $(compgen -e GIT_) 2>/dev/null || true
if ! git -C "$wt" checkout -q -f --detach "$sha" 2>/dev/null; then
  [ ! -e "$wt" ] || infra "existing cache worktree is unavailable; its owner repairs it"
  git --git-dir="$repo" worktree add -q --detach "$wt" "$sha" || infra "no cache worktree"
fi
git -C "$wt" clean -q -fd || infra "cannot clean source"
cd "$wt"
export GATE_TOOLING="$wt"
[ "$(git rev-parse 'HEAD^{tree}')" = "$tree" ] || infra "cache source differs"
files="$cache/files-$part"
if [ $# -gt 0 ]; then printf '%s\n' "$@" > "$files"; else : > "$files"; fi
[ "$(git hash-object "$files")" = "$list_hash" ] || infra "list digest differs"
export PYTHONPATH="$wt/scripts/lib${PYTHONPATH:+:$PYTHONPATH}"
python -c 'from gate_protocol import check_overrides; check_overrides()' || infra "unbound host compiler override"
python - "$base" "$sha" "$list_hash" <<'PY' || infra "base/scope differs"
import sys
from gate_protocol import scope
assert scope(sys.argv[1], sys.argv[2])[1] == sys.argv[3]
PY
# Cargo configuration inside the snapshot is bound by the full tree. External configuration
# is not an input of this job and may not silently add rustflags, wrappers or linkers.
python - <<'PY' || infra "unbound external Cargo configuration"
from pathlib import Path
from gate_protocol import check_external_cargo_config
check_external_cargo_config(Path.cwd())
PY
quiet() { local log; log=$(mktemp); if "$@" > "$log" 2>&1; then rm -f "$log"; else tail -30 "$log"; rm -f "$log"; return 1; fi; }
channel=$(sed -n 's/^channel *= *"\(.*\)"/\1/p' rust-toolchain.toml)
if ! command -v rustup >/dev/null; then
  [ "$part" = rest ] || infra "no native rustup"
  # Original Linux adapter's private bootstrap; never change the host's default setup.
  export RUSTUP_HOME="$cache/rustup" CARGO_HOME="$cache/cargo"
  export PATH="$CARGO_HOME/bin:$PATH"
  if ! command -v rustup >/dev/null; then
    curl -fsSL --max-time 300 https://sh.rustup.rs -o "$cache/rustup-init.sh" \
      && quiet sh "$cache/rustup-init.sh" -y --no-modify-path --profile minimal --default-toolchain none \
      || infra "no private rustup"
  fi
  python -c 'from pathlib import Path; from gate_protocol import check_external_cargo_config; check_external_cargo_config(Path.cwd())' || infra "private Cargo configuration is unbound"
fi
if ! { rustc "+$channel" -V >/dev/null 2>&1 && cargo "+$channel" clippy --version >/dev/null 2>&1 && rustfmt "+$channel" --version >/dev/null 2>&1; }; then
  quiet rustup toolchain install "$channel" --no-self-update --profile minimal --component rustfmt,clippy || infra "toolchain setup"
fi
if [ "$part" = rest ]; then
  linker=${NRESE_GATE_CC:-cc}
  printf 'int main(void) { return 0; }\n' > "$cache/link-probe.c"
  quiet "$linker" "$cache/link-probe.c" -o "$cache/link-probe" || infra "C compiler cannot link"
  rm -f "$cache/link-probe.c" "$cache/link-probe"
fi
. scripts/lib/gate-key.sh
want=$(gate_tool_pin cargo-nextest)
if [ "$(cargo nextest --version 2>/dev/null | awk 'NR == 1 {print $2}')" != "$want" ]; then
  [ "$part" != windows ] || infra "install pinned native cargo-nextest through host tooling"
  curl -fsSL --max-time 300 "https://get.nexte.st/$want/linux" | tar -xz -C "$cache/bin" || infra "nextest setup"
fi
nightly=$(gate_tool_pin miri)
if [ "$part" = rest ]; then
  if ! cargo "+$nightly" miri --version >/dev/null 2>&1; then
    quiet rustup toolchain install "$nightly" --no-self-update --profile minimal --component miri || infra "Miri setup"
  fi
fi
# Only Linux suite contracts need the pinned numerical packages. The existing native
# OS/measure adapter uses standard-library Python, not a second SciPy installation.
py=""
if [ "$part" = rest ]; then
  py="$cache/py-$(git hash-object benches/suite/requirements.txt | cut -c1-12)"
  if [ ! -x "$py/bin/python" ]; then
    if ! quiet python -m venv "$py"; then
      quiet python -m venv --without-pip "$py" \
        && curl -fsSL --max-time 300 https://bootstrap.pypa.io/get-pip.py -o "$cache/get-pip.py" \
        && quiet "$py/bin/python" "$cache/get-pip.py" || infra "Python venv setup"
    fi
    quiet "$py/bin/python" -m pip install -r benches/suite/requirements.txt || infra "Python package setup"
  fi
  export PATH="$py/bin:$PATH"
  gate_tools_check python || infra "pinned Python packages differ"
fi
gate_tools_check cargo-nextest || infra "pinned nextest differs"
info=$(rustc -vV | tr -d '\r')
actual_commit=$(printf '%s\n' "$info" | sed -n 's/^commit-hash: //p')
release=$(printf '%s\n' "$info" | sed -n 's/^release: //p')
target=$(printf '%s\n' "$info" | sed -n 's/^host: //p')
[ "$release" = "${NRESE_GATE_RELEASE:?}" ] && [ "$actual_commit" = "$rustc_commit" ] || infra "compiler identity differs"
[ "$(bash scripts/cargo-guarded.sh --print-rustflags)" = "$flags" ] || infra "effective flags differ"
cpu=$(gate_cpu)
[ -n "$cpu" ] || infra "native CPU identity missing"
artifact=$(python - "$tree" "$release" "$rustc_commit" "$flags" "$target" "$cpu" <<'PY'
import sys
from gate_protocol import artifact_key
print(artifact_key(*sys.argv[1:]))
PY
) || infra "artifact identity failed"
# Owner-authorized bounded exception to host-cache section 3: the shared mutable
# target returned another revision's executable in the RSH probe. Keep that cache,
# registry/tool caches and identical-identity artifacts; do not share build-dir or
# workspace intermediates across trees. A new identity pays the dependency build
# cost within this part's declared resources and whole deadline; no fallback.
export CARGO_TARGET_DIR="$cache/target-gate-$artifact"
[ "$part" != windows ] || export CARGO_TARGET_DIR="$cache/target-gate-win-$artifact"
# Cargo can configure intermediates separately. Bind both locations, overriding
# even a tracked build.build-dir setting that would otherwise share workspace code.
export CARGO_BUILD_BUILD_DIR="$CARGO_TARGET_DIR"
if [ "$part" = rest ]; then
  export NRESE_MIRI_TARGET_DIR="$cache/target-miri-$nightly-$artifact"
fi
export NRESE_TARGET_BUDGET_GB=40
for entry in "$CARGO_TARGET_DIR" "$py" "${NRESE_MIRI_TARGET_DIR:-}"; do
  [ -z "$entry" ] || { mkdir -p "$entry"; echo "$EPOCHSECONDS" > "$entry/.last-used"; }
done
unit=all
export NRESE_GATE_REQUIRED="$cache/required-$part" NRESE_GATE_STEPS="$cache/steps-$part"
export NRESE_GATE_SELECTION="$cache/selection-$part-$key.json"
: > "$NRESE_GATE_REQUIRED"; : > "$NRESE_GATE_STEPS"
set +e
NRESE_GATE_INNER=unit NRESE_ON_BUILD_HOST=1 NRESE_GATE_PART="$part" NRESE_GATE_UNIT="$unit" \
  NRESE_FILES_FROM="$files" bash scripts/check.sh push < /dev/null
code=$?
set -e
[ "$code" -eq 0 ] || [ "$code" -eq 1 ] || infra "unit did not produce a test verdict (exit $code)"
after=$(bash scripts/lib/remote.sh tree) || infra "post-run snapshot failed"
changed=$(git status --porcelain --untracked-files=no)
export GATE_CODE=$code GATE_ATTEMPT=$attempt GATE_BEGAN=$began GATE_PART=$part GATE_INDEX=$index
export GATE_UNIT=$unit GATE_TREE=$tree GATE_BASE=$base GATE_LIST=$list_hash GATE_FLAGS=$flags
export GATE_RUSTC=$actual_commit GATE_RELEASE=$release GATE_TARGET=$target GATE_CPU=$cpu
export GATE_ARTIFACT=$artifact GATE_ARTIFACT_TARGET="${CARGO_TARGET_DIR##*/}"
export GATE_AFTER=$after GATE_CHANGED=$changed GATE_MEMORY_JOB=$memory_job GATE_RAM=$ram
python - "$out/unit-$index.json" <<'PY' || infra "evidence publication failed"
import json, os, time, sys
from pathlib import Path
from gate_protocol import atomic_json, tooling
e = os.environ
rows = [r.split('\t', 1) for r in Path(e['NRESE_GATE_STEPS']).read_text().splitlines()]
record = {'request': e['NRESE_GATE_REQUEST'], 'head': e['NRESE_GATE_HEAD'], 'tree': e['GATE_TREE'],
    'base': e['GATE_BASE'], 'list_hash': e['GATE_LIST'], 'tooling': tooling(e['GATE_TREE']),
    'flags': e['GATE_FLAGS'], 'release': e['GATE_RELEASE'], 'rustc_commit': e['GATE_RUSTC'],
    'target': e['GATE_TARGET'], 'cpu': e['GATE_CPU'], 'job': e['FLEET_JOB'], 'fleet_host': e['FLEET_HOST'],
    'artifact_key': e['GATE_ARTIFACT'], 'artifact_target': e['GATE_ARTIFACT_TARGET'],
    'step': int(e['GATE_INDEX']), 'part': e['GATE_PART'], 'unit': e['GATE_UNIT'],
    'attempt': e['GATE_ATTEMPT'], 'began': float(e['GATE_BEGAN']), 'ended': int(time.time()),
    'code': int(e['GATE_CODE']), 'snapshot_after': e['GATE_AFTER'], 'tracked_changes': e['GATE_CHANGED'],
    'required': Path(e['NRESE_GATE_REQUIRED']).read_text().splitlines(),
    'selection': json.loads(Path(e['NRESE_GATE_SELECTION']).read_text()),
    'steps': [{'name': name, 'code': int(code)} for code, name in rows],
    'memory_cap_gb': float(e['GATE_RAM']), 'memory_job': e['GATE_MEMORY_JOB']}
atomic_json(sys.argv[1], record)
PY
# Preserve existing shared caches. The owned targets carry .last-used for the
# existing host lifecycle owner; guarded cargo applies its unchanged per-target
# budget to this identity's target only. This patch performs no host-wide sweep.
exit "$code"
