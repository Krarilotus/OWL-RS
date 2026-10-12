#!/usr/bin/env bash
# WatDiv's basic testing workload for one of its authors' datasets: the generator v0.6
# (watdiv_v06.tar) compiled in a capped container, then each of the 20 templates
# (testsuite/L1-L5, S1-S7, F1-F5, C1-C3) instantiated INSTANCES times from that dataset's
# saved generator state (saved.txt, shipped in the dataset's archive), recurrence factor 1.
#
#   benches/sparql/watdiv/queries.sh TIER [INSTANCES]     (TIER watdiv-10m or watdiv-100m; default 5)
#
# The generator seeds itself with the clock: an instantiation is never repeated, so its
# output is committed (benches/competitors/queries/TIER/TEMPLATE-N.rq) and is the pin.
# Runs where prepare.sh put the data (SPARQL_DATA, default ~/owlrs/sparql-data).
set -euo pipefail
tier=${1:?usage: queries.sh watdiv-10m|watdiv-100m [INSTANCES]}
instances=${2:-5}
OUT=${SPARQL_DATA:-$HOME/owlrs/sparql-data}
SRC=$OUT/src
state=$SRC/$tier/saved.txt
[ -s "$state" ] || { echo "$state missing: prepare.sh $tier first" >&2; exit 1; }
[ -d "$SRC/watdiv" ] || tar -xf "$SRC/watdiv_v06.tar" -C "$SRC"
lock=${NRESE_QUIET_DIR:-$HOME/.nrese-quiet-slot}
while [ -d "$lock" ] && kill -0 "$(cat "$lock/pid" 2>/dev/null || echo 0)" 2>/dev/null; do sleep 60; done
docker run --rm --name sparql-prep-watdiv --cpus 4 --memory 4g --memory-swap 4g --cpu-shares 2 \
  -v "$SRC:/src" -e TIER="$tier" -e N="$instances" -e UID_GID="$(id -u):$(id -g)" ubuntu:24.04 sh -c '
  set -e
  apt-get update -qq >/dev/null && apt-get install -y -qq g++ make libboost-date-time-dev >/dev/null 2>&1
  cd /src/watdiv && [ -x bin/Release/watdiv ] || make -s release BOOST_HOME=/usr >/dev/null
  rm -rf /src/$TIER/queries && mkdir -p /src/$TIER/queries && cd /src/$TIER
  for t in /src/watdiv/testsuite/*.txt; do
    name=$(basename "$t" .txt)
    /src/watdiv/bin/Release/watdiv -q /src/watdiv/model/wsdbm-data-model.txt "$t" "$N" 1 > "queries/$name.txt"
  done
  chown -R "$UID_GID" /src/watdiv /src/$TIER/queries'
ls -la "$SRC/$tier/queries"
