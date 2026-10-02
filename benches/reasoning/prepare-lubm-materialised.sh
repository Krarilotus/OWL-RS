#!/usr/bin/env bash
# LUBM(N) with its OWL 2 RL closure written out, for comparing query engines that have no
# reasoner (QLever, Oxigraph, Virtuoso, RDF4J): every system loads the same closed data
# without inference and answers the 14 queries on it.
#
#   benches/reasoning/prepare-lubm-materialised.sh [universities...]      (default: 1 10 100)
#
# Produces /data/lubm-<N>-materialised.nt in the Docker volume `nrese-bench-data`: the
# ontology, the instance data and everything OWL 2 RL derives from them, as N-Triples.
#
# The closure is NRESE's (the release build in the volume `nrese-target`, which the suite
# builds first). It isn't taken on trust: the answers on it are checked against the
# published LUBM answers (queries/lubm/expected-lubm-1.tsv), and every system must return
# the same row counts on it.
set -euo pipefail

. "$(dirname "$0")/../../scripts/lib/disk.sh"
require_free_gb "${NRESE_MIN_FREE_GB:-40}" "$(basename "$0")" "$(dirname "$0")"
# What this creates (files in the dataset volume) goes with the volume in
# scripts/bench-cleanup.sh.
export MSYS_NO_PATHCONV=1
HERE=$(cd "$(dirname "$0")" && pwd)
channel=$(sed -n 's/^channel = "\(.*\)"/\1/p' "$HERE/../../rust-toolchain.toml")
RUST_IMAGE=${RUST_IMAGE:-rust:$channel-bookworm}

if ! docker run --rm -v nrese-target:/target alpine test -x /target/release/nrese-server; then
  echo "no NRESE release build in the volume nrese-target: run the suite with NRESE first" >&2
  exit 1
fi

for n in ${*:-1 10 100}; do
  if docker run --rm -v nrese-bench-data:/data alpine test -s "/data/lubm-$n-materialised.nt"; then
    echo "lubm-$n-materialised: present"
    continue
  fi
  bash "$HERE/prepare-lubm.sh" "$n"
  # The store lives in the container and goes with it.
  docker run --rm -v nrese-bench-data:/data -v nrese-target:/target:ro \
    -e NRESE_STORE_MODE=on-disk -e NRESE_DATA_DIR=/tmp/store -e NRESE_REASONING_MODE=owl2-rl \
    -e RUST_LOG=warn "$RUST_IMAGE" sh -c "
      set -e
      /target/release/nrese-server load /data/univ-bench.nt /data/lubm-$n.nt
      /target/release/nrese-server query --format nt 'CONSTRUCT { ?s ?p ?o } WHERE { ?s ?p ?o }' \
        > /data/lubm-$n-materialised.part
      mv /data/lubm-$n-materialised.part /data/lubm-$n-materialised.nt
      echo lubm-$n-materialised: \$(wc -l < /data/lubm-$n-materialised.nt) triples"
done
