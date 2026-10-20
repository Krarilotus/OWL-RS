#!/usr/bin/env bash
# Removes what benchmark runs leave behind. Results stay: scorecards (CSV), logs, reports.
#
#   scripts/bench-cleanup.sh               this checkout's own containers and volumes, its scratch,
#                                          the images its runs pulled; then the host's sweep
#   scripts/bench-cleanup.sh --owner TOOL  of this checkout's, only TOOL's (a scorecard's exit trap:
#                                          the rest of its job keeps its build)
#   scripts/bench-cleanup.sh --job         the whole fleet job's (FLEET_JOB), every checkout of it
#   scripts/bench-cleanup.sh --dry-run     only say what would be removed
#   scripts/bench-cleanup.sh --tools       also remove the installed tools (below)
#   scripts/bench-cleanup.sh --images      also the shared benchmark images nrese-bench/* (by hand)
#   scripts/bench-cleanup.sh --datasets    also the shared datasets (by hand, never a job's cleanup)
#
# By label only (scripts/lib/ns.sh): every container and volume a benchmark makes carries its
# job's namespace (nrese.ns: FLEET_JOB, else a digest of the checkout's path), its checkout
# (nrese.checkout) and its tool (nrese.owner). A job removes its own namespace's and nothing
# else; on a shared host other jobs run beside it (19 Oct 2026: one A-B-B-A job's cleanup
# removed the others' build volumes, all named after the checkout directory `work`).
#   own         containers and volumes of this namespace and checkout (--job: every checkout)
#   sweep       labelled containers, volumes and networks whose checkout no longer exists (its job ended
#               and fleet removed it), created more than NRESE_BENCH_VOLUME_AGE_H hours ago
#               (default 6); unlabelled ones from before the labels are listed, never removed
#   files       inferred-set dumps (*.inferred.nt) under benches/reasoning/results and
#               tmp/bench-run-* in this checkout (scratch of its runs)
#   images      the images this checkout's runs pulled themselves (tmp/bench-pulled-images; an
#               image that was already on the machine is someone else's and stays); with
#               --images the shared nrese-bench/* too
#   build cache Docker's, bounded: what exceeds 10 GB (docker builder prune --keep-storage;
#               the cache is no job's, so no label says whose: only its size is bounded)
#   datasets    --datasets only: the reserved dataset volume nrese-bench-data (budget in
#               benches/datasets.toml) and the datasets (*.nt) and stores (store-*) under the
#               local benchmark directory (NRESE_BENCH_LOCAL, default ~/nrese-bench): every
#               job on the host reads them
#
#   NRESE_BENCH_KEEP=1 (or NRESE_BENCH_KEEP_IMAGES=1)   keep the images
#
# Installed tools: where benches/suite/install-tools.sh has run, the images are this
# machine's tools and stay. `--tools` removes what that script installed (its images and
# .cache/tools).
set -uo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
# shellcheck source=lib/disk.sh
. "$ROOT/scripts/lib/disk.sh"
# shellcheck source=lib/ns.sh
. "$ROOT/scripts/lib/ns.sh"

DRY=
TOOLS=
DATASETS=
IMAGES=
OWNER=
SCOPE=
while [ $# -gt 0 ]; do
  case $1 in
    --dry-run) DRY=1 ;;
    --tools) TOOLS=1 ;;
    --datasets) DATASETS=1 ;;
    --images) IMAGES=1 ;;
    --owner) OWNER=${2:?--owner TOOL}; shift ;;
    --job) [ -n "${FLEET_JOB:-}" ] || { echo "--job: no FLEET_JOB (a fleet job's step)" >&2; exit 2; }; SCOPE=all ;;
    *) echo "unknown argument $1" >&2; exit 2 ;;
  esac
  shift
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

# A labelled resource of a job that ended: its checkout is gone, and it is old enough.
gone() {  # CHECKOUT CREATED
  [ -n "$1" ] && [ ! -d "$1" ] || return 1
  local created
  created=$(date -d "$2" +%s 2>/dev/null) || return 1
  [ $(( $(date +%s) - created )) -ge $(( ${NRESE_BENCH_VOLUME_AGE_H:-6} * 3600 )) ]
}

before=$(free_gb "$ROOT")

if command -v "${NRESE_DOCKER:-docker}" >/dev/null 2>&1 && ns_docker info >/dev/null 2>&1; then
  # This job's own.
  if [ -n "$DRY" ]; then
    echo "would remove namespace $(ns_namespace "$ROOT")'s containers and volumes${OWNER:+ of $OWNER}"
  else
    ns_clean "$ROOT" "$OWNER" "$SCOPE" | sed 's/^/removed /'
  fi
  # The host's sweep: labelled resources of jobs that ended.
  for id in $(ns_docker ps -a -q --filter label=nrese.ns); do
    read -r checkout created < <(ns_docker inspect -f '{{index .Config.Labels "nrese.checkout"}} {{.Created}}' "$id")
    gone "$checkout" "$created" && run ns_docker rm -f -v "$id"
  done
  for volume in $(ns_docker volume ls -q --filter label=nrese.ns); do
    read -r checkout created < <(ns_docker volume inspect -f '{{index .Labels "nrese.checkout"}} {{.CreatedAt}}' "$volume")
    gone "$checkout" "$created" && run ns_volume rm "$volume"
  done
  for network in $(ns_docker network ls -q --filter label=nrese.ns); do
    read -r checkout created < <(ns_docker network inspect -f '{{index .Labels "nrese.checkout"}} {{.Created}}' "$network")
    gone "$checkout" "$created" && run ns_network rm "$network"
  done
  # Unlabelled leftovers of the names from before the namespaces: listed, never removed (one
  # may still belong to a job that started before them).
  legacy=$(for volume in $(ns_docker volume ls -q | grep -E \
             '^(sc-.*|qlever-index-.*|nrese-target(-.+)?|nrese-oxigraph-target|nrese-prof-target|nrese-cert-data|nrese-suite-.+|jena-dist)$'); do
             [ -z "$(ns_docker volume inspect -f '{{index .Labels "nrese.ns"}}' "$volume")" ] && echo "$volume"
           done)
  [ -z "$legacy" ] || echo "unlabelled volumes from before the job namespaces, not removed: $(echo $legacy)"
  data=$(ns_shared data)
  if [ -n "$DATASETS" ] && ns_docker volume inspect "$data" >/dev/null 2>&1; then
    run ns_volume rm "$data"
  fi
  if [ -z "$KEEP_IMAGES" ]; then
    for image in $( [ -n "$IMAGES" ] && ns_docker images --format '{{.Repository}}:{{.Tag}}' | grep '^nrese-bench/') $PULLED_IMAGES; do
      # No -f: an image a container still uses stays.
      ns_docker image inspect "$image" >/dev/null 2>&1 && run ns_docker rmi "$image"
    done
    [ -n "$DRY" ] || rm -f "$PULLED_LIST"
  fi
  # The build cache beyond 10 GB (newer daemons name the bound --reserved-space).
  if [ -n "$DRY" ]; then
    printf 'would run: docker builder prune -f --keep-storage 10GB\n'
  else
    ns_docker builder prune -f --keep-storage 10GB >/dev/null 2>&1 \
      || ns_docker builder prune -f --reserved-space 10GB >/dev/null 2>&1 || true
  fi
  if [ -n "$TOOLS" ] && [ -f "$TOOLBOX" ]; then
    for image in $(sort -u "$TOOLBOX"); do
      ns_docker image inspect "$image" >/dev/null 2>&1 && run ns_docker rmi "$image"
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
if [ -n "$DATASETS" ] && [ -d "$LOCAL" ]; then
  # Datasets and store files, but nothing inside a git checkout (a synced repository
  # keeps its fixtures).
  find "$LOCAL" \( -name '*.nt' -type f -o -name 'store-*' -type d \) 2>/dev/null | while read -r item; do
    git -C "$(dirname "$item")" rev-parse --is-inside-work-tree >/dev/null 2>&1 && continue
    run rm -rf "$item"
  done
fi

[ -n "$DRY" ] || printf 'benchmark leftovers removed: %s GB free before, %s GB now\n' "$before" "$(free_gb "$ROOT")"
