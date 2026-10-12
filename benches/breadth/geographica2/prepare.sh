#!/usr/bin/env bash
# Geographica 2's data (geographica2.di.uoa.gr/datasets, fetched into
# $NRESE_BREADTH/data/geographica2), in the form its runtime loads it:
#   real.nq          the six real-world sets, each in its named graph (map_to_contexts.txt,
#                    upstream's scripts/): the micro and most macro workloads
#   census.nt        New York's streets (TIGER), default graph: macro geocoding
#   synthetic.nt     the generator's 512 set, default graph: the synthetic workload
#   scalability-10m.nt  the first 10 M lines of the 500 M reference set, as upstream's
#                    scalabilityDSGen.sh cuts it
# Prints each file's size and SHA-256 for benches/datasets.toml.
set -euo pipefail
HERE=$(cd "$(dirname "$0")" && pwd)
OUT=${NRESE_BREADTH:-$HOME/nrese-breadth}/data/geographica2
cd "$OUT"
mkdir -p real
for set in gag corine linkedgeodata geonames dbpedia hotspots census generator_512; do
  [ -n "$(ls real/ 2>/dev/null | grep -i "^${set%%_*}")" ] || nice -n 19 tar -xJf "$set.tar.xz" -C real
done
if [ ! -s real.nq ]; then
  : > real.nq.part
  while IFS=$'\t' read -r file graph; do
    [ -n "$file" ] || continue
    # Each N-Triples line ends in " ." (an optional space before it): the graph goes there.
    sed -E "s/[[:space:]]*\.[[:space:]]*\$/ ${graph//\//\\/} ./" "real/$file" >> real.nq.part
  done < "$HERE/map_to_contexts.txt"
  mv real.nq.part real.nq
fi
[ -s census.nt ] || cp real/census.new_york_roads.rdf.NO_CRS.nt census.nt
[ -s synthetic.nt ] || cp real/generator_512.nt synthetic.nt
[ -s scalability-10m.nt ] || { zcat scalability500MRefDS.nt.gz | head -n 10000000 > scalability-10m.nt || true; }
for f in real.nq census.nt synthetic.nt scalability-10m.nt; do
  printf '%s %s %s %s\n' "$(stat -c %s "$f")" "$(wc -l < "$f")" "$(sha256sum "$f" | cut -d' ' -f1)" "$f"
done
