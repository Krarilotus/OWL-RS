#!/usr/bin/env bash
# One-time preparation on a cluster login node: a Rust toolchain in the home directory
# (no root needed), the workload's repository, and Fuseki as the workload's project
# uses it. Light work only, as login nodes require: downloads, no builds. The builds run
# in the first job (integration.sbatch).
#
#   benches/cluster/setup.sh            from the OWL-RS checkout on the cluster
#
#   NRESE_WORK   data, results, scratch (default: /work/$USER/nrese)
set -euo pipefail

ROOT=$(cd "$(dirname "$0")/../.." && pwd)
NRESE_WORK=${NRESE_WORK:-/work/$USER/nrese}
mkdir -p "$NRESE_WORK"

# The toolchain rust-toolchain.toml names is installed by rustup on first use.
if ! command -v cargo >/dev/null 2>&1 && [ ! -x "$HOME/.cargo/bin/cargo" ]; then
  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal --default-toolchain none
fi
export PATH=$HOME/.cargo/bin:$PATH
(cd "$ROOT" && rustc --version)
# Dependencies are downloaded here, because compute nodes may have no internet access.
(cd "$ROOT" && cargo fetch --locked && cargo fetch --locked --manifest-path benches/nrese-bench-harness/Cargo.toml)

REPO=${RGGS_REPO:-$NRESE_WORK/rgonline}
[ -d "$REPO/.git" ] || git clone https://github.com/HisQu/rgonline-gs-data-integration "$REPO"
git -C "$REPO" log -1 --format='workload repository at %h (%ad)' --date=short

# Fuseki at the version the project's justfile pins, where its `just fuseki-fetch` puts it.
version=$(sed -n 's/^FUSEKI_VERSION *:= *"\(.*\)"/\1/p' "$REPO/justfile")
if [ ! -f "$REPO/fuseki/apache-jena-fuseki-$version/fuseki-server.jar" ]; then
  mkdir -p "$REPO/fuseki"
  # The download mirror the justfile names carries only the latest release: Maven Central
  # and the Apache archive keep every one. A stalled download is given up after 30 s.
  for url in "https://repo1.maven.org/maven2/org/apache/jena/apache-jena-fuseki/$version/apache-jena-fuseki-$version.tar.gz" \
    "https://archive.apache.org/dist/jena/binaries/apache-jena-fuseki-$version.tar.gz"; do
    curl -fL --speed-limit 10000 --speed-time 30 -o "$REPO/fuseki/fuseki.tar.gz" "$url" && break
  done
  tar -xzf "$REPO/fuseki/fuseki.tar.gz" -C "$REPO/fuseki" && rm "$REPO/fuseki/fuseki.tar.gz"
fi
command -v java >/dev/null 2>&1 || echo "no java on this node: look for a module (module avail 2>&1 | grep -i -E 'java|jdk'), or set JAVA to a container command (README.md)"

cat <<EOF
ready:
  OWL-RS      $ROOT
  workload    $REPO
  work area   $NRESE_WORK
next: sbatch benches/cluster/integration.sbatch nrese example
EOF
