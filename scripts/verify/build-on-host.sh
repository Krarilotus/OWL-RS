#!/usr/bin/env bash
# A debug nrese-server for /verify, built on a fleet host as a fleet job's step (verifykit/jobs.py
# sends this file inline, so the commit under test needn't have it). Never run it by hand on a
# fleet host; only the fleet starts it. It runs in the job's checkout of the commit to build.
#
# The binary goes to ~/fleet/cache/owlrs-verify/bin-<sha12>/ (nrese-server, REVISION, SHA256SUMS)
# for the run jobs on this host; a binary already there whose checksum holds is reused, not
# rebuilt. Builds share one target directory per toolchain (target-<release>) under the same
# cache, one build at a time (a lock), kept within NRESE_VERIFY_BUDGET_GB (12) by
# scripts/cargo-guarded.sh; binaries of other commits go after 7 days unused, another
# toolchain's target directory after 14. Exact paths under the cache, nothing else.
#
# Results ($FLEET_OUT): build.log, build.json (commit, host, toolchain, built or reused, seconds,
# the binary's SHA-256 and size, python3's version for the run job, the step's cgroup memory
# peak) and, for a fresh build,
# server/ (the binary with REVISION and SHA256SUMS), fetched back with the job.
set -euo pipefail
out=${FLEET_OUT:?a fleet job}
cache=${NRESE_VERIFY_CACHE:-$HOME/fleet/cache/owlrs-verify}
mkdir -p "$cache"
exec > >(tee "$out/build.log") 2>&1
export PATH="$HOME/.cargo/bin:$PATH" CARGO_BUILD_JOBS=${CARGO_BUILD_JOBS:-2}

exec 9> "$cache/lock"
if ! flock -n 9; then
  echo "another /verify build runs on this host: waiting for it"
  flock 9
fi

sha=$(git rev-parse HEAD)
bin=$cache/bin-${sha:0:12}
start=$(date +%s)
channel=$(sed -n 's/^channel *= *"\(.*\)"/\1/p' rust-toolchain.toml)
# Never rustup itself: the host's owners keep it.
rustup toolchain install "$channel" --no-self-update --profile minimal > /dev/null 2>&1 \
  || { echo "no toolchain $channel" >&2; exit 3; }
release=$(rustc -vV | sed -n 's/^release: //p')
if [ -x "$bin/nrese-server" ] && (cd "$bin" && sha256sum --quiet -c SHA256SUMS) \
  && [ "$(cat "$bin/REVISION")" = "$sha" ]; then
  built=false
  echo "reusing $bin/nrese-server (its checksum holds)"
else
  built=true
  export CARGO_TARGET_DIR=$cache/target-$release
  export NRESE_TARGET_BUDGET_GB=${NRESE_VERIFY_BUDGET_GB:-12}
  for dir in "$cache"/target-*; do
    if [ "$dir" != "$CARGO_TARGET_DIR" ] && [ -d "$dir" ] && [ -n "$(find "$dir" -maxdepth 0 -mtime +14)" ]; then
      echo "removing $dir (another toolchain's, unused for 14 days)"
      rm -rf -- "$dir"
    fi
  done
  echo "=== building nrese-server (debug) at $sha with $release, $CARGO_BUILD_JOBS jobs"
  bash scripts/cargo-guarded.sh build --locked -p nrese-server --bin nrese-server
  rm -rf -- "$bin.tmp"
  mkdir -p "$bin.tmp"
  cp "$CARGO_TARGET_DIR/debug/nrese-server" "$bin.tmp/nrese-server"
  echo "$sha" > "$bin.tmp/REVISION"
  (cd "$bin.tmp" && sha256sum nrese-server > SHA256SUMS)
  rm -rf -- "$bin"
  mv "$bin.tmp" "$bin"
  mkdir -p "$out/server"
  cp "$bin/nrese-server" "$bin/REVISION" "$bin/SHA256SUMS" "$out/server/"
fi
touch "$bin"
for dir in "$cache"/bin-*; do
  if [ "$dir" != "$bin" ] && [ -d "$dir" ] && [ -n "$(find "$dir" -maxdepth 0 -mtime +7)" ]; then
    echo "removing $dir (unused for 7 days)"
    rm -rf -- "$dir"
  fi
done

VERIFY_SHA=$sha VERIFY_BIN=$bin VERIFY_BUILT=$built VERIFY_SECONDS=$(($(date +%s) - start)) \
  VERIFY_RUSTC=$(rustc -vV | tr '\n' ';') VERIFY_HOST=$(hostname) \
  VERIFY_PYTHON=$(python3 -c 'import sys; print(sys.version.split()[0])' 2>&1 || echo none) python3 - "$out" << 'EOF'
import json, os, sys
e = os.environ
binary = os.path.join(e["VERIFY_BIN"], "nrese-server")
try:  # the step's cgroup (cgroup v2): the build's peak memory, what BUILD's ram_gb must cover
    scope = open("/proc/self/cgroup").read().strip().splitlines()[-1].split("::", 1)[1]
    peak_mib = round(int(open(f"/sys/fs/cgroup{scope}/memory.peak").read()) / 2**20, 1)
except (OSError, IndexError, ValueError):
    peak_mib = None
json.dump({
    "rev": e["VERIFY_SHA"], "host": e["VERIFY_HOST"], "path": binary, "built": e["VERIFY_BUILT"] == "true",
    "seconds": int(e["VERIFY_SECONDS"]), "rustc": e["VERIFY_RUSTC"], "python3": e["VERIFY_PYTHON"],
    "bytes": os.path.getsize(binary), "sha256": open(os.path.join(e["VERIFY_BIN"], "SHA256SUMS")).read().split()[0],
    "cgroup_peak_mib": peak_mib,
}, open(os.path.join(sys.argv[1], "build.json"), "w"), indent=1)
EOF
cat "$out/build.json"
