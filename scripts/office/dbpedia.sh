#!/usr/bin/env bash
# DBpedia core (as benches/competitors/prepare-datasets.sh builds it) in the office scratch
# directory, as one filtered N-Triples file.
#
# Usage: scripts/office/dbpedia.sh   (downloads and filters DBpedia core into ~/nrese-bench/scratch/dbpedia)
set -euo pipefail
dir=~/nrese-bench/scratch/dbpedia
mkdir -p "$dir"
cd "$dir"
base=https://downloads.dbpedia.org/repo/dbpedia
get() { [ -s "$2" ] || curl -sSfL --retry 3 -o "$2.part" "$1" && mv -f "$2.part" "$2" 2>/dev/null || true; }
get "$base/mappings/mappingbased-objects/2022.12.01/mappingbased-objects_lang=en.ttl.bz2" objects.ttl.bz2
get "$base/mappings/mappingbased-literals/2022.12.01/mappingbased-literals_lang=en.ttl.bz2" literals.ttl.bz2
get "$base/generic/labels/2022.12.01/labels_lang=en.ttl.bz2" labels.ttl.bz2
get "$base/mappings/instance-types/2022.12.01/instance-types_lang=en_specific.ttl.bz2" types.ttl.bz2
ls -la
for f in objects literals labels types; do
  bzip2 -dc "$f.ttl.bz2" | python3 "$(cd "$(dirname "$0")/../.." && pwd)"/benches/competitors/nt-filter.py > "$f.nt"
done
cat objects.nt literals.nt labels.nt types.nt > dbpedia-core.nt
rm -f objects.nt literals.nt labels.nt types.nt
wc -l dbpedia-core.nt
du -h dbpedia-core.nt
echo DONE
