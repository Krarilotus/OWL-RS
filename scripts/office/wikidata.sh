#!/usr/bin/env bash
# The first 60 M valid triples of the Wikidata lexemes dump, as prepare-datasets.sh slices it.
#
# Usage: scripts/office/wikidata.sh   (the first 60 M valid lexeme triples into ~/nrese-bench/scratch/wikidata)
set -euo pipefail
dir=~/nrese-bench/scratch/wikidata
mkdir -p "$dir"; cd "$dir"
[ -s lexemes.nt.gz ] || { curl -sSfL --retry 3 -o lexemes.nt.gz.part https://dumps.wikimedia.org/wikidatawiki/entities/latest-lexemes.nt.gz && mv lexemes.nt.gz.part lexemes.nt.gz; }
ls -la
gunzip -c lexemes.nt.gz | python3 "$(cd "$(dirname "$0")/../.." && pwd)"/benches/competitors/nt-filter.py | head -n 60000000 > wikidata-lexemes-60m.nt || true
wc -l wikidata-lexemes-60m.nt; du -h wikidata-lexemes-60m.nt
echo DONE
