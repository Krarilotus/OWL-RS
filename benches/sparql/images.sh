#!/usr/bin/env bash
# The SPARQL group's competitor images that no vendor publishes (benches/competitors/*/):
# built through a buildx builder of their own, capped at 8 CPUs, a low CPU weight and 8 GB,
# after any timed measurement holding the host's quiet slot. A plain `docker build` runs its
# compilers inside the daemon, where neither `nice` nor the client's limits reach them.
#
#   benches/sparql/images.sh millenniumdb [COMMIT]     (default: the Sparqloscope paper's ecbf6dd)
set -euo pipefail
ROOT=$(cd "$(dirname "$0")/../.." && pwd)
BUILDER=sparql-capped
lock=${NRESE_QUIET_DIR:-$HOME/.nrese-quiet-slot}
while [ -d "$lock" ] && kill -0 "$(cat "$lock/pid" 2>/dev/null || echo 0)" 2>/dev/null; do
  echo "quiet slot held by $(cat "$lock/who" 2>/dev/null): waiting"; sleep 60
done
docker buildx inspect "$BUILDER" >/dev/null 2>&1 || docker buildx create -q --name "$BUILDER" --driver docker-container \
  --driver-opt cpuset-cpus=0-7 --driver-opt cpu-shares=2 --driver-opt memory=8g --driver-opt memory-swap=8g >/dev/null
case ${1:-} in
  millenniumdb)
    commit=${2:-ecbf6dde5a5864f088eee3b0836ad6adba1d623b}
    docker buildx build --builder "$BUILDER" --load --build-arg "MDB_COMMIT=$commit" --build-arg JOBS=8 \
      -t "nrese-bench/millenniumdb:${commit:0:7}" "$ROOT/benches/competitors/millenniumdb"
    ;;
  *) sed -n 2,8p "$0"; exit 2 ;;
esac
