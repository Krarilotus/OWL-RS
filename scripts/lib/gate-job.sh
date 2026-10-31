#!/usr/bin/env bash
# The push tier as a fleet job, on its host: the step scripts/lib/fleet-gate.sh submits (kind
# owlrs.gate). Never run it by hand on a fleet host; only the fleet starts it.
#
#   scripts/lib/gate-job.sh MODE TREE LIST_HASH -- FILES...
#
# It runs the tree's own scripts/check.sh MODE in a worktree that stays from one gate to the next
# (~/fleet/cache/owlrs-gate/wt: only what changed gets new mtimes, so builds stay incremental),
# after checking that its checkout is TREE and its file list hashes to LIST_HASH. One gate at a
# time per host (a lock): gates share the host's one cache with /verify
# (Harness/notes/2026-10-20-host-cache-contract.md): a target directory of the gates' own per
# toolchain release (target-gate-<release>, kept within NRESE_GATE_BUDGET_GB by
# scripts/cargo-guarded.sh), each entry's last use recorded, one sweep for the whole cache
# (NRESE_HOST_CACHE_GB).
#
# Its tools: the tree's toolchain (rust-toolchain.toml), the pinned cargo-nextest and Miri
# nightly (scripts/lib/gate-tools.txt) and the suite's Python packages, all installed once,
# nextest and Python's under the cache directory. No step reaches a Docker daemon.
#
# Results ($FLEET_OUT): gate.log, the run's output; check.log, check.sh's; attempt, this
# attempt's name; gate.json, what it was (scripts/lib/gate-record.py).
# Exit: check.sh's; 3 when the run couldn't be made (no verdict: the push waits).
set -u
mode=$1 tree=$2 list_hash=$3
shift 3
[ "${1:-}" = -- ] && shift
out=${FLEET_OUT:?a fleet job}
cache=${NRESE_GATE_CACHE:-$HOME/fleet/cache/owlrs-gate}
mkdir -p "$cache/bin"
exec > >(tee "$out/gate.log") 2>&1
# This attempt's own result only: what an earlier attempt of the job left goes, and the attempt is
# named (its job, step, start and pid), so its result can't be taken for another's.
rm -rf -- "$out/gate.json" "$out/check.log"
attempt="${FLEET_JOB:-none}:${FLEET_STEP:-0}:${EPOCHREALTIME/[.,]/}:$$"
echo "$attempt" > "$out/attempt"
export PATH="$cache/bin:$HOME/.cargo/bin:$PATH"
# Nothing here may reach a Docker daemon (Office's volumes and images are its colleagues').
export DOCKER_HOST=unix:///nonexistent/owlrs-gate-has-no-docker.sock
infra() { echo "gate-job: $*" >&2; exit 3; }
# A tool's run, its output shown only when it fails.
quiet() {
  local log
  log=$(mktemp)
  "$@" > "$log" 2>&1 || { tail -20 "$log" >&2; rm -f "$log"; return 1; }
  rm -f "$log"
}

exec 9> "$cache/lock"
if ! flock -n 9; then
  echo "another gate runs on this host: waiting for it"
  flock 9 || infra "no lock"
fi

# The worktree, at the commit under test: the job's own checkout's (a worktree of the fleet's
# repository).
tool=$PWD
repo=$(git rev-parse --path-format=absolute --git-common-dir) || infra "not in the job's checkout"
sha=$(git rev-parse "HEAD^{commit}") || infra "no commit HEAD"
wt=$cache/wt
if ! git -C "$wt" checkout -q -f --detach "$sha" 2> /dev/null; then
  rm -rf -- "$wt"
  git --git-dir="$repo" worktree prune
  git --git-dir="$repo" worktree add -q --detach "$wt" "$sha" || infra "no worktree at $sha"
fi
git -C "$wt" clean -q -fd
[ "$(git -C "$wt" rev-parse 'HEAD^{tree}')" = "$tree" ] || infra "the checkout isn't tree $tree"
files=$cache/files
if [ $# -gt 0 ]; then
  printf '%s\n' "$@" > "$files"
else
  : > "$files"
fi
[ "$(git hash-object "$files")" = "$list_hash" ] || infra "the file list isn't $list_hash"
cd "$wt" || infra "no worktree"

# Rust links through the system's C compiler: a host without one can't run a gate, and that is
# the host's failure, not the tree's (khaleesi-wsl, 20 Oct 2026: every build script failed).
linker=${NRESE_GATE_CC:-cc}
command -v "$linker" > /dev/null || infra "no C compiler and linker ($linker) on this host: its owner installs one (build-essential)"
# ... and one that links: a tiny program, built.
printf 'int main(void) { return 0; }\n' > "$cache/link-probe.c"
quiet "$linker" "$cache/link-probe.c" -o "$cache/link-probe" \
  || infra "the C compiler here ($linker) doesn't link a program: its owner repairs it (build-essential)"
rm -f -- "$cache/link-probe.c" "$cache/link-probe"
# A host without rustup (khaleesi-wsl, 20 Oct 2026) gets one of the gate's own, under the cache:
# the host's own home and PATH stay as they are.
if ! command -v rustup > /dev/null; then
  export RUSTUP_HOME="$cache/rustup" CARGO_HOME="$cache/cargo"
  export PATH="$CARGO_HOME/bin:$PATH"
  if ! command -v rustup > /dev/null; then
    curl -fsSL --max-time 300 https://sh.rustup.rs -o "$cache/rustup-init.sh" \
      && quiet sh "$cache/rustup-init.sh" -y --no-modify-path --profile minimal --default-toolchain none \
      || infra "no rustup (the gate's own, under $cache)"
  fi
fi
channel=$(sed -n 's/^channel *= *"\(.*\)"/\1/p' rust-toolchain.toml)
# Never rustup itself: the host's owners keep it (--no-self-update).
quiet rustup toolchain install "$channel" --no-self-update --profile minimal --component rustfmt,clippy \
  || infra "no toolchain $channel"
# The gate's pinned tools (scripts/lib/gate-tools.txt); check.sh checks them again.
pin() { sed -n "s/^$1 //p" "$tool/scripts/lib/gate-tools.txt" | tr -d '\r'; }
want=$(pin cargo-nextest)
if [ "$(cargo nextest --version 2> /dev/null | awk 'NR == 1 { print $2 }')" != "$want" ]; then
  curl -fsSL --max-time 300 "https://get.nexte.st/$want/linux" | tar -xz -C "$cache/bin" \
    || infra "no cargo-nextest $want"
fi
nightly=$(pin miri)
# Miri, for the push tier's nrese-exec step (scripts/miri.sh, which builds under this worktree:
# its target directory stays where the next gate runs it).
if [ "$mode" = push ] && ! cargo "+$nightly" miri --version > /dev/null 2>&1; then
  quiet rustup toolchain install "$nightly" --no-self-update --profile minimal --component miri \
    || infra "no Miri on $nightly"
fi
# A tree whose miri.sh asks for plain `+nightly` (before the pin) gets the pinned one, the main
# PC's, not whatever nightly is today.
if grep -q 'cargo +nightly' scripts/miri.sh 2> /dev/null; then
  mkdir -p "$cache/shim"
  printf '#!/bin/sh\nif [ "$1" = +nightly ]; then shift; exec %s "+%s" "$@"; fi\nexec %s "$@"\n' \
    "$(command -v cargo)" "$nightly" "$(command -v cargo)" > "$cache/shim/cargo"
  chmod +x "$cache/shim/cargo"
  export PATH="$cache/shim:$PATH"
fi
# The suite contracts' Python packages (benches/suite/requirements.txt), in an environment of
# their own, one per version of that file. Without the host's venv module's pip (Debian's
# python3-venv), pip comes from its own bootstrap, into that environment only.
py="$cache/py-$(git hash-object "$tool/benches/suite/requirements.txt" | cut -c1-12)"
if [ ! -x "$py/bin/python" ]; then
  rm -rf -- "$py"
  if ! quiet python3 -m venv "$py"; then
    rm -rf -- "$py"
    { quiet python3 -m venv --without-pip "$py" \
        && curl -fsSL --max-time 300 https://bootstrap.pypa.io/get-pip.py -o "$cache/get-pip.py" \
        && quiet "$py/bin/python" "$cache/get-pip.py"; } \
      || { rm -rf -- "$py"; infra "no Python environment for the suite (venv, pip)"; }
  fi
  quiet "$py/bin/python" -m pip install -r "$tool/benches/suite/requirements.txt" \
    || { rm -rf -- "$py"; infra "the suite's Python packages didn't install"; }
fi
export PATH="$py/bin:$PATH"

# shellcheck source=gate-key.sh
. "$tool/scripts/lib/gate-key.sh"
identity=$(gate_identity) || infra "no rustc"

# The host's one cache (Harness/notes/2026-10-20-host-cache-contract.md, shared with /verify): a
# target directory per toolchain release, each entry's last use recorded, one sweep for all, run
# after the work so it counts what the work added. The target is the gates' alone: cargo decides
# freshness by mtimes, so a build from another checkout in it (/verify's, which wrote
# target-<release> until 26 Oct 2026) could stand in for the tree's own code.
release=$(rustc -vV | sed -n 's/^release: //p')
export CARGO_TARGET_DIR="$cache/target-gate-$release"
export NRESE_TARGET_BUDGET_GB=${NRESE_GATE_BUDGET_GB:-40}
used=("$CARGO_TARGET_DIR" "$py")
for entry in "${used[@]}"; do mkdir -p "$entry" && echo "$EPOCHSECONDS" > "$entry/.last-used"; done
# bin-* after 7 days unused, target-* and py-* after 14, then the least recently used until the
# root fits NRESE_HOST_CACHE_GB; never what this run used (nor wt, bin, rustup, cargo). Miri's
# build in the worktree (wt/target) is an entry too.
sweep() {
  GATE_KEEP=$(IFS=:; echo "${used[*]}") python3 - "$cache" "${NRESE_HOST_CACHE_GB:-40}" << 'EOF'
import glob, os, shutil, sys, time
cache, budget, now = sys.argv[1], float(sys.argv[2]) * 2**30, time.time()
keep = set(os.environ["GATE_KEEP"].split(":"))
def size(path):
    seen, total = set(), 0
    for root, _, files in os.walk(path):
        for name in files:
            try:
                st = os.lstat(os.path.join(root, name))
            except OSError:
                continue
            if (st.st_dev, st.st_ino) not in seen:
                seen.add((st.st_dev, st.st_ino))
                total += st.st_blocks * 512
    return total
def used(path):
    try:
        return float(open(os.path.join(path, ".last-used")).read())
    except (OSError, ValueError):
        return os.stat(path).st_mtime
candidates = [p for pattern in ("bin-*", "target-*", "py-*") for p in glob.glob(os.path.join(cache, pattern))]
candidates.append(os.path.join(cache, "wt", "target"))
entries = sorted((p for p in candidates if os.path.isdir(p) and p not in keep), key=used)
total = size(cache)
for path in entries:
    days = 7 if os.path.basename(path).startswith("bin-") else 14
    why = "unused too long" if now - used(path) > days * 86400 else "over the host's cache budget" if total > budget else None
    if why:
        freed = size(path)
        shutil.rmtree(path)
        total -= freed
        print(f"removed {path} ({why}, {freed / 2**30:.1f} GB)")
EOF
}

echo "=== gate $mode of tree $tree on $(hostname) ($identity)"
began=$SECONDS
# check.sh's own output, whole: its summary is the gate's evidence, step by step.
NRESE_ON_BUILD_HOST=1 NRESE_FILES_FROM="$files" bash scripts/check.sh "$mode" 2>&1 | tee "$out/check.log"
code=${PIPESTATUS[0]}
# The memory the step's slice (FLEET_SLICE) held at most: the gate's whole peak on this host.
peak=""
cgroup=/sys/fs/cgroup$(awk -F: '$1 == "0" { print $3 }' /proc/self/cgroup 2> /dev/null)
while [ -n "${FLEET_SLICE:-}" ] && [ "$cgroup" != /sys/fs/cgroup ] && [ "${cgroup##*/}" != "$FLEET_SLICE" ]; do
  cgroup=$(dirname "$cgroup")
done
[ -r "$cgroup/memory.peak" ] && peak=$(cat "$cgroup/memory.peak")

GATE_TIER=$mode GATE_TREE=$tree GATE_LIST=$list_hash GATE_IDENTITY=$identity GATE_CODE=$code \
  GATE_CPU=$(gate_cpu) GATE_HOST=$(hostname) GATE_NEXTEST=$(cargo nextest --version 2> /dev/null | head -1) \
  GATE_CHANGED=$(git status --porcelain --untracked-files=no) GATE_SECONDS=$((SECONDS - began)) GATE_PEAK=$peak \
  GATE_JOB=${FLEET_JOB:-} GATE_FLEET_HOST=${FLEET_HOST:-} GATE_ATTEMPT=$attempt \
  python3 "$tool/scripts/lib/gate-record.py" "$out"
sweep
exit $code
