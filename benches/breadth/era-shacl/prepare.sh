#!/usr/bin/env bash
# ERA-SHACL's three data subsets, made as upstream's get_data.sh makes them (commit below),
# from the Zenodo dump fetched and md5-checked into $NRESE_BREADTH/data/era-shacl:
#   ES.ttl   Spain       the vocabularies + lines 33,538,045-34,761,236 of the dump
#   FR.ttl   France      the vocabularies + lines 35,092,900-46,412,047
#   ERA.ttl  everything  the vocabularies + the whole dump
# The dump's quads become triples by cutting the graph IRI (upstream's rev|cut|rev|sed). The
# vocabularies are what upstream's loop leaves in vocabularies.ttl: the committed file, then
# ontology.ttl and a newline, then a newline (the loop's `cat` of the file onto itself
# fails). Prints each file's size and SHA-256 for benches/datasets.toml.
set -euo pipefail
COMMIT=fe109c940bf8f1b631f0b703bd7adfad07c570e5
WORK=${NRESE_BREADTH:-$HOME/nrese-breadth}
SRC=$WORK/src/oeg-upm_ERA-SHACL-Benchmark
OUT=$WORK/data/era-shacl
DUMP=$OUT/2025-01-05-rinf-xml-combined.nq.xz

[ -d "$SRC" ] || git clone -q https://github.com/oeg-upm/ERA-SHACL-Benchmark "$SRC"
git -C "$SRC" cat-file -e "$COMMIT^{commit}" 2>/dev/null || git -C "$SRC" fetch -q --unshallow
git -C "$SRC" checkout -q --detach "$COMMIT"
echo "d4cc6012a3193408916de0c96cfc609c  $DUMP" | md5sum -c --quiet

cd "$OUT"
if [ ! -s ERA.ttl ]; then
  { cat "$SRC/data/vocabularies/vocabularies.ttl" "$SRC/data/vocabularies/ontology.ttl"; echo; echo; } > vocabularies.ttl
  nice -n 19 xz -dc "$DUMP" | rev | cut -d "<" -f 2- | rev | sed 's/.$/./' > raw.nt
  sed -n 33538045,34761236p raw.nt | cat vocabularies.ttl - > ES.ttl
  sed -n 35092900,46412047p raw.nt | cat vocabularies.ttl - > FR.ttl
  cat vocabularies.ttl raw.nt > ERA.ttl
  rm raw.nt vocabularies.ttl
fi
mkdir -p shapes
cp "$SRC"/shapes/{tds,core,era}_shapes.ttl shapes/
for f in ES.ttl FR.ttl ERA.ttl shapes/*.ttl; do
  printf '%s %s %s\n' "$(stat -c %s "$f")" "$(sha256sum "$f" | cut -d' ' -f1)" "$f"
done
