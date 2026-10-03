#!/usr/bin/env bash
# Removes what benchmark runs leave behind. Results stay: scorecards (CSV), logs, reports.
#
#   scripts/bench-cleanup.sh             remove everything listed below
#   scripts/bench-cleanup.sh --dry-run   only say what would be removed
#   scripts/bench-cleanup.sh --tools     also remove the installed tools (below)
#   scripts/bench-cleanup.sh --datasets  also remove the reserved dataset volume (deliberately only)
#
# The scorecard scripts call this when they exit, so a run cleans up after itself, also
# when it fails or is interrupted. Between the runs of one batch, set NRESE_BENCH_KEEP=1
# (datasets and images stay, containers and store volumes still go) and run this script
# once at the end.
#
# It removes only what the benchmark scripts create, by the names they use. It never
# prunes "everything unused": other projects' containers, volumes and images are not its
# business.
#   containers  <system>-sc, <system>-sc-load, rsc-*      (with their anonymous volumes)
#   volumes     sc-*, qlever-index-*, nrese-target, nrese-oxigraph-target, nrese-cargo, jena-dist
#   datasets    the volume nrese-bench-data is reserved (budget in benches/datasets.toml): the
#               catalogued datasets stay; unpacked intermediates whose archive is there go,
#               and so do datasets outside the catalogue (unless NRESE_BENCH_KEEP). The volume
#               itself goes only with --datasets.
#   images      nrese-bench/* (built here), and the images a run pulled itself (it notes
#               them in tmp/bench-pulled-images; an image that was already on the
#               machine is someone else's and stays)
#   files       inferred-set dumps (*.inferred.nt) under benches/reasoning/results,
#               tmp/bench-run-* in the repository (scratch of a run),
#               datasets (*.nt) and store directories (store-*) under the local
#               benchmark directory (NRESE_BENCH_LOCAL, default ~/nrese-bench)
#
#   NRESE_BENCH_KEEP=1          keep the dataset volume, the local datasets and the images
#   NRESE_BENCH_KEEP_IMAGES=1   keep only the images
#
# Installed tools: where benches/suite/install-tools.sh has run, the images are this
# machine's tools and stay; a run's containers, volumes, datasets and dumps still go.
# `--tools` removes what that script installed (its images and .cache/tools).
set -uo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
# shellcheck source=lib/disk.sh
. "$ROOT/scripts/lib/disk.sh"

DRY=
TOOLS=
DATASETS=
for argument in "$@"; do
  case $argument in
    --dry-run) DRY=1 ;;
    --tools) TOOLS=1 ;;
    --datasets) DATASETS=1 ;;
    *) echo "unknown argument $argument" >&2; exit 2 ;;
  esac
done
TOOLBOX="$ROOT/.cache/bench-toolbox"
KEEP=${NRESE_BENCH_KEEP:-}
KEEP_IMAGES=${NRESE_BENCH_KEEP_IMAGES:-$KEEP}
# Installed tools stay unless they are what is to be removed.
[ -f "$TOOLBOX" ] && [ -z "$TOOLS" ] && KEEP_IMAGES=1
LOCAL=${NRESE_BENCH_LOCAL:-$HOME/nrese-bench}

# The images the runs pulled themselves (remember_pulls in lib/disk.sh).
PULLED_LIST="$ROOT/tmp/bench-pulled-images"
PULLED_IMAGES=$(sort -u "$PULLED_LIST" 2>/dev/null || true)

run() {
  if [ -n "$DRY" ]; then
    printf 'would run: %s\n' "$*"
  else
    "$@" >/dev/null 2>&1 || true
  fi
}

before=$(free_gb "$ROOT")

if command -v docker >/dev/null 2>&1 && docker info >/dev/null 2>&1; then
  for name in $(docker ps -a --format '{{.Names}}' | grep -E -- '(-sc|-sc-load)$|^rsc-' || true); do
    run docker rm -f -v "$name"
  done
  for volume in $(docker volume ls -q | grep -E '^(sc-.*|qlever-index-.*|nrese-target|nrese-oxigraph-target|nrese-cargo|jena-dist)$' || true); do
    run docker volume rm -f "$volume"
  done
  if docker volume inspect nrese-bench-data >/dev/null 2>&1; then
    if [ -n "$DATASETS" ]; then
      run docker volume rm -f nrese-bench-data
    else
      # The catalogue's files stay; intermediates with their archive present go, and
      # files outside the catalogue unless kept for a batch.
      catalogued=$(python "$ROOT/benches/suite/suite.py" data files 2>/dev/null | tr '\n' ' ')
      sweep='cd /data || exit 0
        for f in real/*.ttl; do [ -f "$f.bz2" ] && echo "$f"; done
        for d in real/*; do [ -d "$d" ] && [ -f "$d.zip" ] && echo "$d"; done
        for d in *.parts real/*.parts; do [ -d "$d" ] && echo "$d"; done
        if [ -z "$KEEP" ] && [ -n "$CATALOGUED" ]; then
          for f in $(find . -type f ! -name "*.source" | sed "s|^\./||"); do
            case " $CATALOGUED " in *" $f "*) ;; *) case "$f" in real/*) ;; *) echo "$f" ;; esac ;; esac
          done
        fi'
      for item in $(docker run --rm -e KEEP="$KEEP" -e CATALOGUED="$catalogued" -v nrese-bench-data:/data:ro alpine sh -c "$sweep" 2>/dev/null); do
        run docker run --rm -v nrese-bench-data:/data alpine rm -rf "/data/$item"
      done
    fi
  fi
  if [ -z "$KEEP_IMAGES" ]; then
    for image in $(docker images --format '{{.Repository}}:{{.Tag}}' | grep '^nrese-bench/' || true) $PULLED_IMAGES; do
      # No -f: an image a container still uses stays.
      docker image inspect "$image" >/dev/null 2>&1 && run docker rmi "$image"
    done
    [ -n "$DRY" ] || rm -f "$PULLED_LIST"
  fi
  if [ -n "$TOOLS" ] && [ -f "$TOOLBOX" ]; then
    for image in $(sort -u "$TOOLBOX"); do
      docker image inspect "$image" >/dev/null 2>&1 && run docker rmi "$image"
    done
    run rm -rf "$ROOT/.cache/tools"
    [ -n "$DRY" ] || rm -f "$TOOLBOX"
  fi
fi

# Inferred-set dumps: the scorecard has their counts and comparisons.
find "$ROOT/benches/reasoning/results" -name '*.inferred.nt' -type f 2>/dev/null | while read -r file; do
  run rm -f "$file"
done
for scratch in "$ROOT"/tmp/bench-run-*; do
  [ -e "$scratch" ] && run rm -rf "$scratch"
done
if [ -z "$KEEP" ] && [ -d "$LOCAL" ]; then
  # Datasets and store files, but nothing inside a git checkout (a synced repository
  # keeps its fixtures).
  find "$LOCAL" \( -name '*.nt' -type f -o -name 'store-*' -type d \) 2>/dev/null | while read -r item; do
    git -C "$(dirname "$item")" rev-parse --is-inside-work-tree >/dev/null 2>&1 && continue
    run rm -rf "$item"
  done
fi

[ -n "$DRY" ] || printf 'benchmark leftovers removed: %s GB free before, %s GB now\n' "$before" "$(free_gb "$ROOT")"
