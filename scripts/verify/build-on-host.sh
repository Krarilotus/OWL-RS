#!/usr/bin/env bash
# A debug nrese-server for /verify, built on a fleet host as a fleet job's step (verifykit/jobs.py
# sends this file inline, so the commit under test needn't have it). Never run it by hand on a
# fleet host; only the fleet starts it. It runs in the job's checkout of the commit to build.
#
# The binary goes to ~/fleet/cache/owlrs-verify/bin-<sha12>/ (nrese-server, REVISION, SHA256SUMS)
# for the run jobs on this host; a binary already there whose checksum holds is reused, not
# rebuilt. Builds share one target directory per toolchain (target-<release>) under the same
# cache, one build at a time (a lock), each kept within NRESE_VERIFY_BUDGET_GB (12) by
# scripts/cargo-guarded.sh.
#
# Deadlines (seconds; past one the step stops with exit 3, and the phase's command with every
# process it started is killed: timeout signals its whole process group):
#   NRESE_VERIFY_LOCK_S 1800 for the lock, NRESE_VERIFY_TOOLCHAIN_S 900 for installing the
#   toolchain, NRESE_VERIFY_BUILD_S 3600 for the build.
#
# The cache stays within NRESE_VERIFY_CACHE_GB (16) all told: after every build or reuse, entries
# (bin-*, target-*) unused for 7 days (binaries) or 14 (target directories) go, then the least
# recently used ones until the cache fits, by each entry's own last-use record
# (.verify-last-used, written when it is built or reused). This commit's binary and this
# toolchain's target directory are kept whatever their size. Exact paths under the cache only.
#
# Results ($FLEET_OUT): build.log, build.json (commit, host, toolchain, built or reused, seconds,
# the binary's SHA-256 and size, python3's version for the run job, the step's memory: its
# cgroup's peak, page cache included, and the highest anonymous memory sampled each second, what
# the build holds; the cache before and after, what went and the bytes reclaimed) and, for a
# fresh build, server/ (the binary with REVISION and SHA256SUMS), fetched back with the job.
set -euo pipefail
out=${FLEET_OUT:?a fleet job}
cache=${NRESE_VERIFY_CACHE:-$HOME/fleet/cache/owlrs-verify}
mkdir -p "$cache"
exec > >(tee "$out/build.log") 2>&1
export PATH="$HOME/.cargo/bin:$PATH" CARGO_BUILD_JOBS=${CARGO_BUILD_JOBS:-2}

# A phase within its deadline; past it, stopped with its processes, and the step ends (exit 3).
bounded() {
  local seconds=$1 what=$2 code=0
  shift 2
  timeout --kill-after=30 "$seconds" "$@" || code=$?
  if [ "$code" -eq 124 ] || [ "$code" -eq 137 ]; then
    echo "$what didn't finish within $seconds s: stopped with every process it started" >&2
    exit 3
  fi
  return "$code"
}

# The step's anonymous memory each second, its highest kept: what the build holds (memory.peak
# counts the page cache of the files it writes too).
cgroup=/sys/fs/cgroup$(sed -n 's/^0:://p' /proc/self/cgroup)
sample() {
  local max=0 now
  while sleep 1; do
    now=$(awk '$1 == "anon" { print $2 }' "$cgroup/memory.stat" 2> /dev/null || true)
    if [ -n "$now" ] && [ "$now" -gt "$max" ]; then
      max=$now
      echo "$max" > "$out/.anon-peak"
    fi
  done
}
sample &
sampler=$!
trap 'kill "$sampler" 2> /dev/null || true' EXIT

exec 9> "$cache/lock"
if ! flock -n 9; then
  echo "another /verify build runs on this host: waiting for it (at most ${NRESE_VERIFY_LOCK_S:-1800} s)"
  flock -w "${NRESE_VERIFY_LOCK_S:-1800}" 9 || { echo "the cache's lock wasn't free in time" >&2; exit 3; }
fi

sha=$(git rev-parse HEAD)
bin=$cache/bin-${sha:0:12}
start=$(date +%s)
channel=$(sed -n 's/^channel *= *"\(.*\)"/\1/p' rust-toolchain.toml)
# Never rustup itself: the host's owners keep it.
bounded "${NRESE_VERIFY_TOOLCHAIN_S:-900}" "installing toolchain $channel" \
  rustup toolchain install "$channel" --no-self-update --profile minimal > /dev/null 2>&1 \
  || { echo "no toolchain $channel" >&2; exit 3; }
release=$(rustc -vV | sed -n 's/^release: //p')
target=$cache/target-$release
if [ -x "$bin/nrese-server" ] && (cd "$bin" && sha256sum --quiet -c SHA256SUMS) \
  && [ "$(cat "$bin/REVISION")" = "$sha" ]; then
  built=false
  echo "reusing $bin/nrese-server (its checksum holds)"
else
  built=true
  export CARGO_TARGET_DIR=$target
  export NRESE_TARGET_BUDGET_GB=${NRESE_VERIFY_BUDGET_GB:-12}
  echo "=== building nrese-server (debug) at $sha with $release, $CARGO_BUILD_JOBS jobs"
  bounded "${NRESE_VERIFY_BUILD_S:-3600}" "the build" \
    bash scripts/cargo-guarded.sh build --locked -p nrese-server --bin nrese-server
  rm -rf -- "$bin.tmp"
  mkdir -p "$bin.tmp"
  cp "$CARGO_TARGET_DIR/debug/nrese-server" "$bin.tmp/nrese-server"
  echo "$sha" > "$bin.tmp/REVISION"
  (cd "$bin.tmp" && sha256sum nrese-server > SHA256SUMS)
  rm -rf -- "$bin"
  mv "$bin.tmp" "$bin"
  date +%s > "$target/.verify-last-used"
  mkdir -p "$out/server"
  cp "$bin/nrese-server" "$bin/REVISION" "$bin/SHA256SUMS" "$out/server/"
fi
date +%s > "$bin/.verify-last-used"
kill "$sampler" 2> /dev/null || true

VERIFY_SHA=$sha VERIFY_BIN=$bin VERIFY_TARGET=$target VERIFY_CACHE=$cache VERIFY_BUILT=$built \
  VERIFY_SECONDS=$(($(date +%s) - start)) VERIFY_CACHE_GB=${NRESE_VERIFY_CACHE_GB:-16} \
  VERIFY_RUSTC=$(rustc -vV | tr '\n' ';') VERIFY_HOST=$(hostname) VERIFY_CGROUP=$cgroup \
  VERIFY_PYTHON=$(python3 -c 'import sys; print(sys.version.split()[0])' 2>&1 || echo none) python3 - "$out" << 'EOF'
import glob, json, os, shutil, sys, time
e, out = os.environ, sys.argv[1]
cache, binary_dir, target = e["VERIFY_CACHE"], e["VERIFY_BIN"], e["VERIFY_TARGET"]
budget, now = int(float(e["VERIFY_CACHE_GB"]) * 2**30), time.time()


def size(path):
    """Bytes on disk under `path`, each file once (cargo hard-links its outputs)."""
    seen, total = set(), 0
    for root, _, files in os.walk(path):
        for name in files:
            try:
                st = os.lstat(os.path.join(root, name))
            except OSError:
                continue
            if (st.st_dev, st.st_ino) not in seen:
                seen.add((st.st_dev, st.st_ino))
                total += st.st_blocks * 512 if hasattr(st, "st_blocks") else st.st_size
    return total


def last_used(path):
    try:
        return float(open(os.path.join(path, ".verify-last-used")).read())
    except (OSError, ValueError):
        return os.stat(path).st_mtime  # an entry made before last-use records


def mib(path):
    try:
        return round(int(open(path).read().split()[0]) / 2**20, 1)
    except (OSError, ValueError, IndexError):
        return None


# This commit's binary and this toolchain's target directory are what the next jobs reuse.
kept = {binary_dir, target}
entries = [p for p in glob.glob(os.path.join(cache, "bin-*")) + glob.glob(os.path.join(cache, "target-*"))
           if os.path.isdir(p) and p not in kept]
before, removed = size(cache), []
total = before
for path in sorted(entries, key=last_used):  # least recently used first
    used = last_used(path)
    days = 7 if os.path.basename(path).startswith("bin-") else 14
    why = "unused too long" if now - used > days * 86400 else "over the cache's budget" if total > budget else None
    if why:
        freed = size(path)
        shutil.rmtree(path)
        total -= freed
        removed.append({"path": path, "bytes": freed, "unused_days": round((now - used) / 86400, 1), "why": why})
after = size(cache)
binary = os.path.join(binary_dir, "nrese-server")
json.dump({
    "rev": e["VERIFY_SHA"], "host": e["VERIFY_HOST"], "path": binary, "built": e["VERIFY_BUILT"] == "true",
    "seconds": int(e["VERIFY_SECONDS"]), "rustc": e["VERIFY_RUSTC"], "python3": e["VERIFY_PYTHON"],
    "bytes": os.path.getsize(binary), "sha256": open(os.path.join(binary_dir, "SHA256SUMS")).read().split()[0],
    "memory": {"cgroup_peak_mib": mib(os.path.join(e["VERIFY_CGROUP"], "memory.peak")),
               "anon_peak_mib": mib(os.path.join(out, ".anon-peak"))},
    "cache": {"path": cache, "budget_bytes": budget, "before_bytes": before, "after_bytes": after,
              "reclaimed_bytes": before - after, "removed": removed, "over_budget": after > budget},
}, open(os.path.join(out, "build.json"), "w"), indent=1)
EOF
rm -f "$out/.anon-peak"
cat "$out/build.json"
