#!/usr/bin/env bash
# StarBench's data (BKR as RDF-star, REF benchmark, Zenodo 4148888, Apache-2.0), in the two
# forms its systems read, into $NRESE_BREADTH/data/starbench:
#   bkr-star.ttls   the published Turtle-star, decompressed: GraphDB and Stardog load it
#                   (RDF-star, their own semantics, as the paper ran them)
#   bkr-star.nts    the same as N-Triples-star, written by Jena 4.7.0 (the paper's Jena)
#   bkr-rdf12.nt    RDF 1.2 N-Triples, one reifier per distinct quoted triple
#                   (rdfstar_to_rdf12.py): NRESE, Jena 6 and Oxigraph 0.5 load it
# Prints each file's size and SHA-256 for benches/datasets.toml.
set -euo pipefail
HERE=$(cd "$(dirname "$0")" && pwd)
WORK=${NRESE_BREADTH:-$HOME/nrese-breadth}
OUT=$WORK/data/starbench
JENA=$WORK/src/apache-jena-4.7.0
cd "$OUT"
echo "eb91d948b94c3d7607c4f07650433e16  BKR-star-fullKGdump.ttls.gz" | md5sum -c --quiet
if [ ! -d "$JENA" ]; then
  mkdir -p "$WORK/src/jena-4.7.0-download"
  curl -sfL -o "$WORK/src/jena-4.7.0-download/apache-jena-4.7.0.tar.gz" \
    https://archive.apache.org/dist/jena/binaries/apache-jena-4.7.0.tar.gz
  echo "$(curl -sfL https://archive.apache.org/dist/jena/binaries/apache-jena-4.7.0.tar.gz.sha512 | cut -d' ' -f1)  $WORK/src/jena-4.7.0-download/apache-jena-4.7.0.tar.gz" | sha512sum -c --quiet
  tar -xzf "$WORK/src/jena-4.7.0-download/apache-jena-4.7.0.tar.gz" -C "$WORK/src"
fi
[ -s bkr-star.ttls ] || zcat BKR-star-fullKGdump.ttls.gz > bkr-star.ttls
if [ ! -s bkr-star.nts ]; then
  docker run --rm -v "$JENA:/jena:ro" -v "$OUT:/d" -e JVM_ARGS=-Xmx16g eclipse-temurin:21-jre \
    /jena/bin/riot --syntax=turtle --output=nt /d/bkr-star.ttls > bkr-star.nts.part
  mv bkr-star.nts.part bkr-star.nts
fi
if [ ! -s bkr-rdf12.nt ]; then
  python3 -I "$HERE/rdfstar_to_rdf12.py" < bkr-star.nts > bkr-rdf12.nt.part
  mv bkr-rdf12.nt.part bkr-rdf12.nt
fi
for f in bkr-star.ttls bkr-star.nts bkr-rdf12.nt; do
  printf '%s %s %s\n' "$(stat -c %s "$f")" "$(sha256sum "$f" | cut -d' ' -f1)" "$f"
done
