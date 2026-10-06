#!/usr/bin/env bash
# The fast suite's competitor comparisons, overnight on the office PC (on the shared main
# PC they held the quiet slot too long): DL against Konclude and HermiT at equal resources,
# Nemo's OWL 2 RL rerun, the client sweep with QLever's and Virtuoso's concurrency tracks,
# then the plain SPARQL cases, the cache replay, the printer's rewritten queries and Jena's
# home-turf case without the full count. The systems of a run go one after another; every
# container is capped (DOCKER_CPUS, FAST_MAX_GB); `fast.py compete` checks counts before
# times.
#
#   fast-compete.sh dry-run   everything up to the first case, now: the bundle's commit
#                             checked out, the image loaded, the build, the harness, every
#                             image and dataset present; ends with DRY-RUN-OK
#   fast-compete.sh batch     waits until START_AT (default 19:00), then the same and the
#                             cases; starts no step between 07:30 and START_AT
#
# Expects in ~/nrese-bench: fast-compete.bundle (the branch bench/fast-compete) and
# dl-reference.tar.gz (the DL kit's image). Run detached, in a capped scope:
#
#   nohup setsid systemd-run --user --scope -p MemoryMax=16G -p CPUQuota=2400% \
#     bash ~/nrese-bench/fast-compete.sh batch > ~/nrese-bench/fast-compete.log 2>&1 < /dev/null &
#
# Results: ~/nrese-bench/scratch/fast-compete/ (results/, logs, DONE when finished).
set -eu
MODE=${1:-batch}
START_AT=${START_AT:-19:00}
BENCH=/home/krarilotus/nrese-bench
WT=$BENCH/OWL-RS-fast
OUT=$BENCH/scratch/fast-compete
cd "$BENCH"
mkdir -p "$OUT"
step() { echo "$(date '+%F %H:%M:%S') $*"; }
hhmm() { echo "$1" | tr -d :; }
case "$MODE" in dry-run|batch) ;; *) echo "usage: $0 dry-run|batch" >&2; exit 2 ;; esac

if [ "$MODE" = batch ]; then
  step "waiting until $START_AT"
  until [ "$(date +%-H%M)" -ge "$(hhmm "$START_AT" | sed 's/^0*//')" ]; do sleep 60; done
fi

step "code"
git -C "$BENCH/OWL-RS" fetch -q "$BENCH/fast-compete.bundle" bench/fast-compete
# One commit for the worktree and its checkout: a linked worktree has no FETCH_HEAD of its own.
COMMIT=$(git -C "$BENCH/OWL-RS" rev-parse --verify FETCH_HEAD^{commit})
if [ -d "$WT" ]; then
  git -C "$WT" checkout -q --detach "$COMMIT"
else
  git -C "$BENCH/OWL-RS" worktree add -q --detach "$WT" "$COMMIT"
fi
[ "$(git -C "$WT" rev-parse HEAD)" = "$COMMIT" ] || { step "checkout is not $COMMIT"; exit 1; }
git -C "$WT" log --oneline -1
mkdir -p "$WT/.cache"
[ -e "$WT/.cache/bench-toolbox" ] || ln -s "$BENCH/OWL-RS/.cache/bench-toolbox" "$WT/.cache/bench-toolbox"

step "DL kit image"
docker image inspect nrese-bench/dl-reference >/dev/null 2>&1 || docker load -q -i "$BENCH/dl-reference.tar.gz"

cd "$WT"
export PATH=$HOME/.cargo/bin:$PATH PYTHONIOENCODING=utf-8
export CARGO_BUILD_JOBS=8 NRESE_MEMORY_CAP_GB=12 CARGO_TARGET_DIR=$WT/target
# Every container on 16 CPUs and at most 12 GB; the scope's other 8 CPUs are the harness's
# clients and the driver; colleagues' sessions keep their 13 GB.
export DOCKER_CPUS=16 FAST_MAX_GB=12
PY=python3.12

step "build (perf lab, DL tools, server; the harness)"
nice -n 10 $PY benches/fast/fast.py build > "$OUT/build.log" 2>&1
nice -n 10 bash scripts/cargo-guarded.sh build --release --locked --quiet \
  --manifest-path benches/nrese-bench-harness/Cargo.toml >> "$OUT/build.log" 2>&1

step "present: images, datasets, toolbox, driver"
missing=0
for image in "$($PY -c 'import sys; sys.path.insert(0, "benches/fast"); import fast; print(fast.rust_image())')" \
             adfreiburg/qlever:latest ghcr.io/oxigraph/oxigraph:0.5.11 openlink/virtuoso-opensource-7:7.2.17 \
             nrese-bench/jena:6.2.0 nrese-bench/nemo:latest nrese-bench/dl-reference:latest; do
  docker image inspect "$image" >/dev/null 2>&1 || { step "missing image $image"; missing=1; }
done
for file in lubm-1.nt lubm-10.nt lubm-100.nt univ-bench.nt owl2bench-ql-1.nt owl2bench-rl-1.nt; do
  docker run --rm -v nrese-bench-data:/d:ro alpine test -s "/d/$file" || { step "missing dataset $file"; missing=1; }
done
[ -e "$WT/.cache/bench-toolbox" ] || { step "missing toolbox"; missing=1; }
[ -x "$WT/target/release/nrese-bench-harness" ] || { step "missing harness"; missing=1; }
docker run --rm -v "nrese-target-$(basename "$WT")":/t:ro alpine sh -c \
  'test -x /t/release/examples/perf_lab && test -x /t/release/examples/ql_print_check && test -x /t/release/nrese-server' \
  || { step "missing build outputs"; missing=1; }
$PY benches/fast/fast.py list > "$OUT/cases.txt" || { step "fast.py list failed"; missing=1; }
step "$(wc -l < "$OUT/cases.txt") cases listed; commit $COMMIT"
[ "$missing" = 0 ] || { step "not ready"; exit 1; }

if [ "$MODE" = dry-run ]; then
  # The build volume, the image and the worktree stay for tonight's batch.
  touch "$OUT/DRY-RUN-OK"
  step "dry run ok: ready for the first case"
  exit 0
fi

run() {  # a label, a time limit, then compete's arguments
  label=$1 limit=$2; shift 2
  now=$(date +%-H%M) start=$(hhmm "$START_AT" | sed 's/^0*//')
  if [ "$now" -ge 730 ] && [ "$now" -lt "$start" ]; then
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
  --cases q-paths,q-sideways,q-large-result,q-topk,op-group,op-aggregates,op-optional,op-negation,op-strings,op-dates,kernel-intersect,index-join-keys,rdf12-annotations,q-cycles
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
