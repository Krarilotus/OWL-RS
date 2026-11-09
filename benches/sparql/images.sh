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
# What an image is built from, on the image itself: its context's git tree and the source commit it
# builds (the receipt reads them off the id that ran: suitekit/driver.py image_source). Only from a
# clean checkout, read as clean: a `git status` that fails is no evidence of one (review
# image-source-commit r1 F1). A build at another commit than its Dockerfile's says so in its own label.
labels() {  # CONTEXT (under the repository) COMMIT -> LABELS
  LABELS=()
  local status tree
  status=$(git -C "$ROOT" status --porcelain 2>/dev/null) || return 0
  [ -z "$status" ] || return 0
  tree=$(git -C "$ROOT" rev-parse "HEAD:$1" 2>/dev/null) || return 0
  LABELS=(--label "owlrs.built-from=$1@$tree" --label "owlrs.source-commit=$2")
}
# The source commit a context declares, the one place it is named: every `ARG NAME=` default of its
# Dockerfile, one non-empty value. Absent, empty or two values: the build refuses (r1 follow-up),
# also with a commit given, which a Dockerfile without the ARG wouldn't build.
declared() {  # CONTEXT NAME -> the value
  local values
  values=$(sed -n "s/^ARG $2=//p" "$ROOT/benches/competitors/$1/Dockerfile" | tr -d '\r' | sort -u)
  if [ -z "$values" ] || [ "$(printf '%s\n' "$values" | wc -l)" -ne 1 ]; then
    echo "images.sh: benches/competitors/$1/Dockerfile declares $2 with no single non-empty default" \
      "(ARG $2=...): ${values:-none}" >&2
    return 1
  fi
  printf '%s\n' "$values"
}
case ${1:-} in
  millenniumdb)
    default=$(declared millenniumdb MDB_COMMIT) || exit 2
    commit=${2:-$default}
    labels benches/competitors/millenniumdb "$commit"
    docker buildx build --builder "$BUILDER" --load --build-arg "MDB_COMMIT=$commit" --build-arg JOBS=8 \
      "${LABELS[@]}" -t "nrese-bench/millenniumdb:${commit:0:7}" "$ROOT/benches/competitors/millenniumdb"
    ;;
  gcare)
    commit=$(declared gcare GCARE_COMMIT) || exit 2
    labels benches/competitors/gcare "$commit"
    docker buildx build --builder "$BUILDER" --load "${LABELS[@]}" -t "nrese-bench/gcare:$commit" \
      "$ROOT/benches/competitors/gcare"
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
