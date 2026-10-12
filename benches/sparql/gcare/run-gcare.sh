#!/usr/bin/env bash
# G-CARE's own estimators on its data and queries, by the paper's accuracy procedure
# (scripts/run-build.sh and run-accuracy-exps.sh at d603d2b): p = 0.03, seed 0; 30 trials for
# the sampling estimators (wj, cs, jsub, impr), one for the deterministic ones (cset, sumrdf,
# bsk with budget 4096, sumrdf with threshold 0.15). Compiled and run in a capped container
# (4 CPUs, 16 GB), after any timed measurement holding the host's quiet slot.
#
#   benches/sparql/gcare/run-gcare.sh OUT-DIR [DATASET...] [-- METHOD...]
#
# DATASET: aids human lubm80 yago (default all); METHOD default all seven. Reads the published
# archives unpacked under SPARQL_DATA/src/gcare (prepare.sh gcare) and the code checked out at
# SPARQL_DATA/src/gcare-code; writes OUT-DIR/DATASET_METHOD_query_result.txt (per query: the
# mean estimate, time and variance, then every trial's estimate) and the build results.
# summarise.py turns them and NRESE's estimates.py output into q-error percentiles.
set -euo pipefail
OUTDIR=$(realpath -m "${1:?usage: run-gcare.sh OUT-DIR [DATASET...] [-- METHOD...]}"); shift
datasets=() methods=()
while [ $# -gt 0 ]; do
  if [ "$1" = "--" ]; then shift; methods=("$@"); break; fi
  datasets+=("$1"); shift
done
[ ${#datasets[@]} -gt 0 ] || datasets=(aids human lubm80 yago)
[ ${#methods[@]} -gt 0 ] || methods=(wj cset cs jsub impr bsk sumrdf)
DATA=${SPARQL_DATA:-$HOME/owlrs/sparql-data}
code=$DATA/src/gcare-code
[ "$(git -C "$code" rev-parse --short=7 HEAD)" = d603d2b ] || { echo "$code is not G-CARE d603d2b" >&2; exit 1; }
mkdir -p "$OUTDIR"
lock=${NRESE_QUIET_DIR:-$HOME/.nrese-quiet-slot}
while [ -d "$lock" ] && kill -0 "$(cat "$lock/pid" 2>/dev/null || echo 0)" 2>/dev/null; do sleep 60; done
docker run --rm --name sparql-gcare --cpus 4 --memory 16g --memory-swap 16g --cpu-shares 2 \
  -v "$code:/gcare" -v "$DATA/src/gcare:/published:ro" -v "$OUTDIR:/out" \
  -e DATASETS="${datasets[*]}" -e METHODS="${methods[*]}" -e UID_GID="$(id -u):$(id -g)" ubuntu:24.04 sh -c '
  set -e
  apt-get update -qq >/dev/null && apt-get install -y -qq g++ cmake make libboost-all-dev >/dev/null 2>&1
  cd /gcare && mkdir -p build && cd build && [ -x gcare_graph ] || { cmake .. >/dev/null && make -j4 >/dev/null; }
  mkdir -p /work/dataset && for d in $DATASETS; do mkdir -p /work/dataset/$d && cp /published/dataset/$d/$d.txt /work/dataset/$d/; done
  for d in $DATASETS; do
    for m in $METHODS; do
      bin=/gcare/build/gcare_graph; case $m in cs|bsk) bin=/gcare/build/gcare_relation ;; esac
      export GCARE_BSK_BUDGET=4096 GCARE_SUMRDF_THRESHOLD=0.15
      repeat=30; case $m in cset|bsk|sumrdf) repeat=1 ;; esac
      echo "== $d $m (build, then $repeat trials per query)"
      $bin -b -m $m -i /work/dataset/$d/$d.txt -d /work/dataset/$d/$d -p 0.03 -s 0 -o /out/${d}_${m}_build_result.txt
      $bin -q -m $m -i /published/queryset/$d/ -d /work/dataset/$d/$d -p 0.03 -n $repeat -s 0 -o /out/${d}_${m}_query_result.txt
    done
  done
  chown -R "$UID_GID" /out /gcare/build'
