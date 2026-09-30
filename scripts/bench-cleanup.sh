#!/usr/bin/env bash
# Removes what benchmark runs leave behind. Results stay: scorecards (CSV), logs, reports.
#
#   scripts/bench-cleanup.sh             remove everything listed below
#   scripts/bench-cleanup.sh --dry-run   only say what would be removed
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
#   volumes     sc-*, qlever-index-*, nrese-target, nrese-cargo, jena-dist,
#               and the dataset volume nrese-bench-data
#   images      nrese-bench/* (built here), and the systems' images the scripts pull
#   files       inferred-set dumps (*.inferred.nt) under benches/reasoning/results,
#               tmp/bench-run-* in the repository (scratch of a run),
#               datasets (*.nt) and store directories (store-*) under the local
#               benchmark directory (NRESE_BENCH_LOCAL, default ~/nrese-bench)
#
#   NRESE_BENCH_KEEP=1          keep the dataset volume, the local datasets and the images
#   NRESE_BENCH_KEEP_IMAGES=1   keep only the images
set -uo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
# shellcheck source=lib/disk.sh
. "$ROOT/scripts/lib/disk.sh"

DRY=
[ "${1:-}" = "--dry-run" ] && DRY=1
KEEP=${NRESE_BENCH_KEEP:-}
KEEP_IMAGES=${NRESE_BENCH_KEEP_IMAGES:-$KEEP}
LOCAL=${NRESE_BENCH_LOCAL:-$HOME/nrese-bench}

# The images the scripts pull (their defaults); an image another container uses stays.
PULLED_IMAGES="
ontotext/graphdb:11.5.1
adfreiburg/qlever:latest
ghcr.io/oxigraph/oxigraph:0.5.11
openlink/virtuoso-opensource-7:7.2.17
cambridgesemantics/anzograph:3.5.0
rust:$(sed -n 's/^channel = "\(.*\)"/\1/p' "$ROOT/rust-toolchain.toml")-bookworm
"

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
  for volume in $(docker volume ls -q | grep -E '^(sc-.*|qlever-index-.*|nrese-target|nrese-cargo|jena-dist)$' || true); do
    run docker volume rm -f "$volume"
  done
  [ -n "$KEEP" ] || run docker volume rm -f nrese-bench-data
  if [ -z "$KEEP_IMAGES" ]; then
    for image in $(docker images --format '{{.Repository}}:{{.Tag}}' | grep '^nrese-bench/' || true) $PULLED_IMAGES; do
      # No -f: an image a container still uses is somebody's, and stays.
      docker image inspect "$image" >/dev/null 2>&1 && run docker rmi "$image"
    done
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
