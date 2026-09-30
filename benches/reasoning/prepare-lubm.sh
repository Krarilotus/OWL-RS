#!/usr/bin/env bash
# LUBM data for the reasoning benchmark, generated into the Docker volume `nrese-bench-data`.
#
#   benches/reasoning/prepare-lubm.sh [universities...]      (default: 1 10 100)
#
# Produces:
#   /data/univ-bench.nt   the univ-bench ontology as N-Triples, in the namespace the generated
#                         data and the published queries use (see below)
#   /data/lubm-<N>.nt     LUBM(N) instance data, one file (≈ 0.1 / 1.3 / 13.9 M triples)
#
# Two fixes to the raw material, both needed for the data, ontology and queries to agree:
#   - The ontology is published at swat.cse.lehigh.edu, but UBA writes, and the 14 queries
#     ask for, http://www.lehigh.edu/~zhp2/2004/0401/univ-bench.owl#. The ontology is
#     rewritten to the latter.
#   - UBA writes one `<> owl:imports ...` header line per file; `<>` isn't valid N-Triples,
#     so those lines are dropped.
# The generator is pinned (lubm/Dockerfile), so the output is the same on every machine.
set -euo pipefail

# Datasets, store files and images take tens of GB: don't start on a nearly full disk.
. "$(dirname "$0")/../../scripts/lib/disk.sh"
require_free_gb "${NRESE_MIN_FREE_GB:-40}" "$(basename "$0")" "$(dirname "$0")"
export MSYS_NO_PATHCONV=1
native() { cygpath -m "$1" 2>/dev/null || echo "$1"; }
HERE=$(native "$(cd "$(dirname "$0")" && pwd)")
JENA_IMAGE=${JENA_IMAGE:-nrese-bench/jena:6.2.0}
# The pinned Jena image (benches/competitors/jena), built here if missing.
docker image inspect "$JENA_IMAGE" >/dev/null 2>&1 ||
  docker build -q -t "$JENA_IMAGE" "$HERE/../competitors/jena" >/dev/null
ONTOLOGY_URL=http://swat.cse.lehigh.edu/onto/univ-bench.owl

docker build -q -t nrese-bench/lubm-uba "$HERE/lubm" >/dev/null

if ! docker run --rm -v nrese-bench-data:/data alpine test -s /data/univ-bench.nt; then
  docker run --rm -v nrese-bench-data:/data alpine sh -c "
    apk add -q curl >/dev/null &&
    curl -sSfL -o /tmp/univ-bench.owl $ONTOLOGY_URL &&
    sed 's#http://swat.cse.lehigh.edu/onto/univ-bench.owl#http://www.lehigh.edu/~zhp2/2004/0401/univ-bench.owl#g' \
      /tmp/univ-bench.owl > /data/univ-bench.owl"
  docker run --rm -v nrese-bench-data:/data --entrypoint sh "$JENA_IMAGE" -c \
    '/opt/apache-jena-6.2.0/bin/riot --quiet --output=nt /data/univ-bench.owl > /data/univ-bench.nt &&
     rm /data/univ-bench.owl'
fi

for n in ${*:-1 10 100}; do
  if docker run --rm -v nrese-bench-data:/data alpine test -s "/data/lubm-$n.nt"; then
    echo "lubm-$n: present"
    continue
  fi
  docker run --rm -v nrese-bench-data:/data nrese-bench/lubm-uba \
    -u "$n" -f NTRIPLES --consolidate Maximal -t 8 -q -o "/data/lubm-gen-$n" >/dev/null
  docker run --rm -v nrese-bench-data:/data alpine sh -c "
    cat /data/lubm-gen-$n/* | grep -v '^<> ' > /data/lubm-$n.nt && rm -rf /data/lubm-gen-$n &&
    echo lubm-$n: \$(wc -l < /data/lubm-$n.nt) triples"
done
