#!/usr/bin/env bash
# The fast suite's competitor comparisons, overnight on the office PC (on the shared main
# PC they held the quiet slot too long): DL against Konclude and HermiT at equal resources,
# Nemo's OWL 2 RL rerun, the client sweep with QLever's and Virtuoso's concurrency tracks,
# then the plain SPARQL cases, the cache replay, the printer's rewritten queries and Jena's
# home-turf case without the full count. The systems of a run go one after another; every
# container is capped (DOCKER_CPUS, FAST_MAX_GB); `fast.py compete` checks counts before
# times.
#
# Expects in ~/nrese-bench: fast-compete.bundle (the branch bench/fast-compete) and
# dl-reference.tar.gz (the DL kit's image). Waits for the gate batch's marker and never runs
# beside it; gives up if the marker isn't there by 06:00, and starts no step after 07:30.
# Run detached, in a capped scope:
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
git -C "$WT" checkout -q --detach FETCH_HEAD
git -C "$WT" log --oneline -1
mkdir -p "$WT/.cache"
[ -e "$WT/.cache/bench-toolbox" ] || ln -s "$BENCH/OWL-RS/.cache/bench-toolbox" "$WT/.cache/bench-toolbox"

step "DL kit image"
docker image inspect nrese-bench/dl-reference >/dev/null 2>&1 || docker load -i "$BENCH/dl-reference.tar.gz"

cd "$WT"
export PATH=$HOME/.cargo/bin:$PATH PYTHONIOENCODING=utf-8
export CARGO_BUILD_JOBS=8 NRESE_MEMORY_CAP_GB=12
# Every container on 16 CPUs and at most 12 GB; the scope's other 8 CPUs are the harness's
# clients and the driver; colleagues' sessions keep their 13 GB.
export DOCKER_CPUS=16 FAST_MAX_GB=12
PY=python3.12

step "build (perf lab, DL tools, server)"
nice -n 10 $PY benches/fast/fast.py build > "$OUT/build.log" 2>&1

run() {  # a label, a time limit, then compete's arguments
  label=$1 limit=$2; shift 2
  if [ "$(date +%H%M)" -ge 730 ] && [ "$(date +%H)" -lt 21 ]; then
    step "compete $label: not started (past 07:30)"; return 0
  fi
  step "compete $label"
  timeout "$limit" $PY benches/fast/fast.py compete "$@" > "$OUT/$label.log" 2>&1 || step "compete $label: exit $?"
}
DL=dl-transitive,dl-transitive-400,dl-number,dl-nominals,dl-datatypes,dl-roles,el-classify,el-classify-irregular,horn-classify,dl-pipeline,dl-abox-lubm1,classify-store-horn,classify-store-tableau
run dl-konclude 2h --cases "$DL" --systems konclude
run dl-hermit 2h --cases "$DL" --systems hermit
run nemo 2h --systems nemo
run clients 3h --cases clients-sweep,clients-lookups --systems qlever,virtuoso,oxigraph,jena
run plain 3h --merge --systems qlever,oxigraph,virtuoso,jena \
  --cases q-paths,q-sideways,q-large-result,q-topk,op-group,op-aggregates,op-optional,op-negation,op-strings,op-dates,kernel-intersect,index-join-keys,rdf12-annotations
run cache 1h --cases cache-replay --systems qlever
run rewritten 3h --systems qlever,oxigraph,virtuoso \
  --cases rl-hierarchy,rdfs-hierarchy,rl-clique,rl-chain,rl-fed-chain,custom-tc,rdfs-full-lubm10,rdfs-plus-lubm10,horst-lubm10,ql-lubm10,ql-owl2bench,rl-lubm100
export SKIP_COUNT=1
run jena-horst 1h --cases horst-lubm10 --systems jena
unset SKIP_COUNT

step "results"
cp -r benches/fast/results "$OUT/results"
# Ours only: the fast suite's containers carry the worktree's name, the suite's its workload.
docker ps -a --format '{{.Names}}' | grep -E -- '-OWL-RS-fast$|^nrese-suite-.*-fast-' | xargs -r docker rm -f
docker volume rm -f nrese-fast-data nrese-cargo "nrese-target-$(basename "$WT")" >/dev/null 2>&1 || true
docker image rm nrese-bench/dl-reference >/dev/null 2>&1 || true
touch "$OUT/DONE"
step "done"
