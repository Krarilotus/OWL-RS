#!/usr/bin/env bash
# Converts the images the benchmark suite runs into SIF files for Apptainer
# (`benches/suite/suite.py run --runtime apptainer --sif-dir <dir>`).
#
#   benches/cluster/build-sif.sh <sif-dir> [systems...]     (default: every system with an adapter)
#
# Where the images come from, in this order:
#   1. <sif-dir>/<name>.tar, a `docker save` archive (docker-archive://): how the images this
#      repository builds (nrese-bench/*) reach a cluster without Docker. On a workstation:
#        docker save nrese-bench/jena:6.2.0 -o <sif-dir>/nrese-bench_jena_6.2.0.tar
#      then copy the directory to the cluster.
#   2. The local Docker daemon (docker-daemon://), where Docker runs next to Apptainer.
#   3. The image's registry (docker://), for the pulled images.
# A SIF file that exists is kept. Its name is the image with / and : as _ (suitekit/runtime.py).
# The licensed systems' images carry no licence; the suite mounts the licence files.
set -euo pipefail

HERE=$(cd "$(dirname "$0")" && pwd)
ROOT=$(cd "$HERE/../.." && pwd)
SIF_DIR=${1:?usage: $0 <sif-dir> [systems...]}
shift
mkdir -p "$SIF_DIR"
command -v apptainer >/dev/null || { echo "no apptainer on this machine" >&2; exit 2; }
PYTHON=${PYTHON:-python3}
command -v "$PYTHON" >/dev/null || PYTHON=python

failed=0
while IFS=$'\t' read -r image context; do
  name=$(printf '%s' "$image" | tr '/:' '__')
  sif=$SIF_DIR/$name.sif
  if [ -s "$sif" ]; then
    echo "$image: $sif present"
    continue
  fi
  if [ -s "$SIF_DIR/$name.tar" ]; then
    source=docker-archive://$SIF_DIR/$name.tar
  elif command -v docker >/dev/null && docker image inspect "$image" >/dev/null 2>&1; then
    # docker-daemon:// wants a tag.
    case ${image##*/} in *:*) tagged=$image ;; *) tagged=$image:latest ;; esac
    source=docker-daemon://$tagged
  elif [ "$context" = "-" ]; then
    source=docker://$image
  else
    echo "$image: built by this repository (benches/$context); docker save it into $SIF_DIR/$name.tar first" >&2
    failed=1
    continue
  fi
  echo "$image: from $source"
  apptainer build --force "$sif.partial" "$source" && mv "$sif.partial" "$sif" || {
    rm -f "$sif.partial"
    echo "$image: the build failed" >&2
    failed=1
  }
done < <("$PYTHON" "$ROOT/benches/suite/suite.py" images "$@")
exit $failed
