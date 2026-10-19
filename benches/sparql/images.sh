#!/usr/bin/env bash
# The SPARQL group's competitor images that no vendor publishes (benches/competitors/*/):
# built through a buildx builder of their own, capped at 8 CPUs, a low CPU weight and 8 GB,
# after any timed measurement holding the host's quiet slot. A plain `docker build` runs its
# compilers inside the daemon, where neither `nice` nor the client's limits reach them.
#
#   benches/sparql/images.sh millenniumdb [COMMIT]     (default: the Sparqloscope paper's ecbf6dd)
#   benches/sparql/images.sh gcare                     G-CARE's estimators at d603d2b
#   benches/sparql/images.sh export IMAGE DEST         an image for a host without Docker
#   benches/sparql/images.sh unpack NAME.tar ROOTFS-DIR  there, for the rootfs runtime
set -euo pipefail
ROOT=$(cd "$(dirname "$0")/../.." && pwd)
. "$ROOT/scripts/lib/ns.sh"
# This job's own builder (a buildx builder is a container: another job's is another's), removed
# with the build.
BUILDER=$(ns_name sparql-capped "$ROOT")
lock=${NRESE_QUIET_DIR:-$HOME/.nrese-quiet-slot}
case ${1:-} in millenniumdb|gcare) build=1 ;; *) build=0 ;; esac
[ "$build" = 1 ] && while [ -d "$lock" ] && kill -0 "$(cat "$lock/pid" 2>/dev/null || echo 0)" 2>/dev/null; do
  echo "quiet slot held by $(cat "$lock/who" 2>/dev/null): waiting"; sleep 60
done
[ "$build" = 0 ] || trap 'docker buildx rm "$BUILDER" >/dev/null 2>&1 || true' EXIT
[ "$build" = 0 ] || docker buildx inspect "$BUILDER" >/dev/null 2>&1 || docker buildx create --name "$BUILDER" --driver docker-container \
  --driver-opt cpuset-cpus=0-7 --driver-opt cpu-shares=2 --driver-opt memory=8g --driver-opt memory-swap=8g >/dev/null
case ${1:-} in
  millenniumdb)
    commit=${2:-ecbf6dde5a5864f088eee3b0836ad6adba1d623b}
    docker buildx build --builder "$BUILDER" --load --build-arg "MDB_COMMIT=$commit" --build-arg JOBS=8 \
      -t "nrese-bench/millenniumdb:${commit:0:7}" "$ROOT/benches/competitors/millenniumdb"
    ;;
  gcare)
    docker buildx build --builder "$BUILDER" --load -t nrese-bench/gcare:d603d2b "$ROOT/benches/competitors/gcare"
    ;;
  export)
    # An image for the rootfs runtime (suitekit/runtime.py RootfsRuntime) of a host without
    # Docker: DEST/<image with / and : as _>.tar (its filesystem) and .json (its inspect).
    image=${2:?usage: images.sh export IMAGE DEST}; dest=${3:?usage: images.sh export IMAGE DEST}
    name=$(printf '%s' "$image" | tr '/:' '__')
    mkdir -p "$dest"
    id=$(docker create "$image" true)
    docker export "$id" > "$dest/$name.tar" && docker rm -f "$id" >/dev/null
    docker image inspect "$image" > "$dest/$name.json"
    sha256sum "$dest/$name.tar"
    ;;
  unpack)
    # On the host without Docker: ROOTFS-DIR/<name>/rootfs and config.json from an export.
    tarfile=${2:?usage: images.sh unpack NAME.tar ROOTFS-DIR}; root=${3:?usage: images.sh unpack NAME.tar ROOTFS-DIR}
    name=$(basename "$tarfile" .tar)
    rm -rf "${root:?}/$name" && mkdir -p "$root/$name/rootfs"
    tar -xf "$tarfile" -C "$root/$name/rootfs" --no-same-owner --exclude='dev/*' 2>/dev/null || true
    cp "${tarfile%.tar}.json" "$root/$name/config.json"
    du -sh "$root/$name"
    ;;
  *) sed -n 2,8p "$0"; exit 2 ;;
esac
