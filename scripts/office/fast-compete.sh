#!/usr/bin/env bash
# The fast suite's comparisons that are too noisy for the shared main PC, overnight on the
# office PC: DL against Konclude and HermiT at equal resources, Nemo's OWL 2 RL rerun, and
# the client sweep with QLever's and Virtuoso's concurrency tracks. One system at a time;
# every container capped (DOCKER_CPUS, its case's memory); counts checked before times by
# `fast.py compete` itself.
#
# Expects in ~/nrese-bench: fast-compete.bundle (the branch bench/fast-compete) and
# dl-reference.tar.gz (the DL kit's image). Waits for the gate batch's marker and never runs
# beside it; gives up if the marker isn't there by 06:00. Run detached, in a capped scope:
#
#   nohup setsid systemd-run --user --scope -p MemoryMax=16G -p CPUQuota=2400% \
#     bash ~/nrese-bench/fast-compete.sh > ~/nrese-bench/fast-compete.log 2>&1 < /dev/null &
#
# Results: ~/nrese-bench/scratch/fast-compete-2026-10-06/ (results/, logs, DONE when finished).
set -eu
BENCH=/home/krarilotus/nrese-bench
WT=$BENCH/OWL-RS-fast
OUT=$BENCH/scratch/fast-compete-2026-10-06
GATES=$BENCH/scratch/gates-2026-10-06/done
cd "$BENCH"
mkdir -p "$OUT"
step() { echo "$(date +%H:%M:%S) $*"; }

step "waiting for the gate batch ($GATES)"
until [ -e "$GATES" ]; do
  if [ "$(date +%H)" -ge 6 ] && [ "$(date +%H)" -lt 12 ]; then
    step "no gate marker by 06:00: not running"; exit 1
  fi
  sleep 60
done
step "gates done"

step "code"
git -C "$BENCH/OWL-RS" fetch -q "$BENCH/fast-compete.bundle" bench/fast-compete
[ -d "$WT" ] || git -C "$BENCH/OWL-RS" worktree add -q --detach "$WT" FETCH_HEAD
git -C "$WT" log --oneline -1
mkdir -p "$WT/.cache"
[ -e "$WT/.cache/bench-toolbox" ] || ln -s "$BENCH/OWL-RS/.cache/bench-toolbox" "$WT/.cache/bench-toolbox"

step "DL kit image"
docker image inspect nrese-bench/dl-reference >/dev/null 2>&1 || docker load -i "$BENCH/dl-reference.tar.gz"

cd "$WT"
export PATH=$HOME/.cargo/bin:$PATH PYTHONIOENCODING=utf-8
export CARGO_BUILD_JOBS=8 NRESE_MEMORY_CAP_GB=12
# Every container on 16 CPUs; the scope's other 8 are the harness's clients and the driver.
export DOCKER_CPUS=16
PY=python3.12

step "build (perf lab, DL tools, server)"
nice -n 10 $PY benches/fast/fast.py build > "$OUT/build.log" 2>&1

run() {  # a label, then compete's arguments
  label=$1; shift
  step "compete $label"
  timeout 4h $PY benches/fast/fast.py compete "$@" > "$OUT/$label.log" 2>&1 || step "compete $label: exit $?"
}
DL=dl-transitive,dl-transitive-400,dl-number,dl-nominals,dl-datatypes,dl-roles,el-classify,el-classify-irregular,horn-classify,dl-pipeline,dl-abox-lubm1,classify-store-horn,classify-store-tableau
run dl-konclude --cases "$DL" --systems konclude
run dl-hermit --cases "$DL" --systems hermit
run nemo --systems nemo
for system in qlever virtuoso oxigraph jena; do
  run "clients-$system" --cases clients-sweep,clients-lookups --systems "$system"
done

step "results"
cp -r benches/fast/results "$OUT/results"
# Ours only: the fast suite's containers carry the worktree's name, the suite's its workload.
docker ps -a --format '{{.Names}}' | grep -E -- '-OWL-RS-fast$|^nrese-suite-.*-fast-' | xargs -r docker rm -f
docker volume rm -f nrese-fast-data nrese-cargo "nrese-target-$(basename "$WT")" >/dev/null 2>&1 || true
docker image rm nrese-bench/dl-reference >/dev/null 2>&1 || true
touch "$OUT/DONE"
step "done"
