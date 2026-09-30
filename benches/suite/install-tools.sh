#!/usr/bin/env bash
# Installs the suite's tools on this machine: the images of the other systems, the images
# the kits build (Jena, the generators, the oracles), the Rust image the scorecards build
# NRESE in, and Fuseki for the integration kit. Safe to run again: what is there is kept.
#
#   benches/suite/install-tools.sh            install
#   benches/suite/install-tools.sh --list     what is installed and what is missing
#   scripts/bench-cleanup.sh --tools          remove the tools again
#
# Tools stay between runs: once this script has run, the cleanup after a benchmark
# removes that run's containers, volumes, datasets and dumps, and leaves the images. The
# licensed systems (GraphDB, RDFox, Stardog) are installed without their licences and
# don't answer without them; see benches/competitors/README.md.
set -uo pipefail

HERE=$(cd "$(dirname "$0")" && pwd)
ROOT=$(cd "$HERE/../.." && pwd)
# shellcheck source=../../scripts/lib/disk.sh
. "$ROOT/scripts/lib/disk.sh"
TOOLBOX=$ROOT/.cache/bench-toolbox
FUSEKI_VERSION=${FUSEKI_VERSION:-6.0.0} # the integration workload's project pins it
RUST_IMAGE=rust:$(sed -n 's/^channel = "\(.*\)"/\1/p' "$ROOT/rust-toolchain.toml")-bookworm

PULLED=(
  "adfreiburg/qlever:latest"
  "ghcr.io/oxigraph/oxigraph:0.5.11"
  "openlink/virtuoso-opensource-7:7.2.17"
  "ontotext/graphdb:11.5.1"
  "oxfordsemantic/rdfox:7.6b"
  "stardog/stardog:latest"
  "eclipse/rdf4j-workbench:latest"
  "eclipse-temurin:21-jre"
  "python:3.13-slim"
  "alpine:latest"
  "$RUST_IMAGE"
)
# image=build context, relative to benches/
BUILT=(
  "nrese-bench/jena:6.2.0=competitors/jena"
  "nrese-bench/jena-reasoner=reasoning/jena"
  "nrese-bench/owlrl-oracle=reasoning/oracle"
  "nrese-bench/nemo=reasoning/nemo"
  "nrese-bench/lubm-uba=reasoning/lubm"
  "nrese-bench/owl2bench=reasoning/owl2bench"
)
FUSEKI=$ROOT/.cache/tools/apache-jena-fuseki-$FUSEKI_VERSION

have() { docker image inspect "$1" >/dev/null 2>&1; }

if [ "${1:-}" = "--list" ]; then
  for image in "${PULLED[@]}"; do printf '%-9s %s\n' "$(have "$image" && echo installed || echo missing)" "$image"; done
  for entry in "${BUILT[@]}"; do printf '%-9s %s\n' "$(have "${entry%%=*}" && echo installed || echo missing)" "${entry%%=*}"; done
  printf '%-9s %s\n' "$([ -f "$FUSEKI/fuseki-server.jar" ] && echo installed || echo missing)" "Fuseki $FUSEKI_VERSION ($FUSEKI)"
  exit 0
fi

require_free_gb "${NRESE_MIN_FREE_GB:-40}" "$(basename "$0")" "$ROOT"
mkdir -p "$(dirname "$TOOLBOX")"
failed=0
for image in "${PULLED[@]}"; do
  if have "$image"; then
    echo "there:  $image"
    continue
  fi
  echo "pull:   $image"
  if docker pull -q "$image" >/dev/null; then
    # Only what this script brought is the toolbox's to remove later.
    echo "$image" >>"$TOOLBOX"
  else
    echo "FAILED: $image" >&2
    failed=1
  fi
done
for entry in "${BUILT[@]}"; do
  image=${entry%%=*}
  echo "build:  $image"
  if docker build -q -t "$image" "$ROOT/benches/${entry#*=}" >/dev/null; then
    grep -qxF "$image" "$TOOLBOX" 2>/dev/null || echo "$image" >>"$TOOLBOX"
  else
    echo "FAILED: $image" >&2
    failed=1
  fi
done
if [ -f "$FUSEKI/fuseki-server.jar" ]; then
  echo "there:  Fuseki $FUSEKI_VERSION"
else
  echo "fetch:  Fuseki $FUSEKI_VERSION"
  mkdir -p "$ROOT/.cache/tools"
  archive=$ROOT/.cache/tools/fuseki.tar.gz
  for url in "https://repo1.maven.org/maven2/org/apache/jena/apache-jena-fuseki/$FUSEKI_VERSION/apache-jena-fuseki-$FUSEKI_VERSION.tar.gz" \
    "https://archive.apache.org/dist/jena/binaries/apache-jena-fuseki-$FUSEKI_VERSION.tar.gz"; do
    curl -fsSL --speed-limit 10000 --speed-time 30 -o "$archive" "$url" && break
  done
  tar -xzf "$archive" -C "$ROOT/.cache/tools" && rm -f "$archive" || { echo "FAILED: Fuseki" >&2; failed=1; }
fi
touch "$TOOLBOX"
echo
"$0" --list
exit $failed
