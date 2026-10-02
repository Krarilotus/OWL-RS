#!/usr/bin/env bash
# Downloads the real-world scorecard datasets (Pf0) and converts each into one validated
# N-Triples file, /data/<name>.nt, in the Docker volume `nrese-bench-data`. Every loader
# gets identical input: N-Triples is the format each system parses fastest, and converting
# once with Oxigraph validates it.
#
#   benches/competitors/prepare-datasets.sh [names...]
#
# Datasets (download size; all public):
#   olympics          QLever's demo dataset (10 MB)
#   yago-tiny         YAGO 4.5 tiny (200 MB zip)
#   dbpedia-core      DBpedia 2022-12 English: mapping-based objects and literals, labels,
#                     specific instance types (560 MB bz2)
#   wikidata-lexemes  Wikidata lexemes dump, latest (1.5 GB gz)
# Full Wikidata truthy (72 GB gz, ~8 B triples) needs a large server and isn't included.
set -euo pipefail

# Datasets, store files and images take tens of GB: don't start on a nearly full disk.
. "$(dirname "$0")/../../scripts/lib/disk.sh"
require_free_gb "${NRESE_MIN_FREE_GB:-40}" "$(basename "$0")" "$(dirname "$0")"
# What this creates (the dataset volume, images) is removed by scripts/bench-cleanup.sh,
# which the scorecards run when they exit.
export MSYS_NO_PATHCONV=1

OXIGRAPH_IMAGE=${OXIGRAPH_IMAGE:-ghcr.io/oxigraph/oxigraph:0.5.11}
PYTHON_IMAGE=${PYTHON_IMAGE:-python:3.13-alpine}
HERE=$(cd "$(dirname "$0")" && pwd)
HERE=$(cygpath -m "$HERE" 2>/dev/null || echo "$HERE")
NAMES=${*:-olympics yago-tiny dbpedia-core wikidata-lexemes}

# Runs a shell command in /data/real of the data volume.
in_volume() {
  docker run --rm -v nrese-bench-data:/data -w /data/real alpine sh -euc "$1"
}

download() { # url file
  in_volume "[ -s '$2' ] || { apk add -q curl >/dev/null; curl -sSfL --retry 3 -o '$2.part' '$1'; mv '$2.part' '$2'; }"
}

# Converts the files matched by a glob (relative to /data/real) into /data/<name>.nt.
# With `filter`, line-based N-Triples sources first lose lines with invalid IRIs
# (nt-filter.py), so that strict and lenient loaders get identical input.
# The Oxigraph image has no shell, so it converts file by file; alpine concatenates.
convert() { # name glob [filter]
  local name=$1 glob=$2 filter=${3:-} file source format
  in_volume "rm -rf '$name.parts' && mkdir '$name.parts'"
  for file in $(in_volume "ls $glob"); do
    source=$file
    if [ -n "$filter" ]; then
      source=$name.parts/$(basename "$file").filtered
      docker run --rm -v nrese-bench-data:/data -v "$HERE:/scripts:ro" "$PYTHON_IMAGE" \
        sh -c "python /scripts/nt-filter.py < '/data/real/$file' > '/data/real/$source'"
    fi
    # The source's syntax by its extension (YAGO ships Turtle).
    case $file in
      *.ttl) format=ttl ;;
      *.nq) format=nq ;;
      *) format=nt ;;
    esac
    docker run --rm -v nrese-bench-data:/data "$OXIGRAPH_IMAGE" convert --from-format "$format" \
      --from-file "/data/real/$source" --to-file "/data/real/$name.parts/$(basename "$file").nt"
    [ -z "$filter" ] || in_volume "rm -f '$source'"
  done
  in_volume "cat '$name.parts'/*.nt > '/data/$name.nt.part'
    mv '/data/$name.nt.part' '/data/$name.nt'
    rm -rf '$name.parts'
    echo \"$name: \$(wc -l < '/data/$name.nt') triples, \$(du -h '/data/$name.nt' | cut -f1)\""
}

for name in $NAMES; do
  if docker run --rm -v nrese-bench-data:/data alpine test -s "/data/$name.nt"; then
    echo "$name: ready"
    continue
  fi
  case $name in
    olympics)
      download https://github.com/wallscope/olympics-rdf/raw/master/data/olympics-nt-nodup.zip olympics.zip
      in_volume "rm -rf olympics && mkdir olympics && unzip -q -o olympics.zip -d olympics"
      convert olympics "olympics/*.nt"
      ;;
    yago-tiny)
      download https://yago-knowledge.org/data/yago4.5/yago-4.5.0.2-tiny.zip yago-tiny.zip
      in_volume "rm -rf yago-tiny && mkdir yago-tiny && unzip -q -o yago-tiny.zip -d yago-tiny"
      convert yago-tiny "yago-tiny/*.ttl"
      ;;
    dbpedia-core)
      base=https://downloads.dbpedia.org/repo/dbpedia
      download "$base/mappings/mappingbased-objects/2022.12.01/mappingbased-objects_lang=en.ttl.bz2" dbpedia-objects.ttl.bz2
      download "$base/mappings/mappingbased-literals/2022.12.01/mappingbased-literals_lang=en.ttl.bz2" dbpedia-literals.ttl.bz2
      download "$base/generic/labels/2022.12.01/labels_lang=en.ttl.bz2" dbpedia-labels.ttl.bz2
      download "$base/mappings/instance-types/2022.12.01/instance-types_lang=en_specific.ttl.bz2" dbpedia-types.ttl.bz2
      in_volume "apk add -q bzip2 >/dev/null
        for f in dbpedia-*.ttl.bz2; do [ -s \"\${f%.bz2}\" ] || bunzip2 -k \"\$f\"; done"
      convert dbpedia-core "dbpedia-*.ttl" filter
      ;;
    wikidata-lexemes-*m)
      # The first N million valid triples, streamed from the dump without unpacking it
      # (the full dump is ~229 M triples and ~31 GB).
      millions=${name#wikidata-lexemes-}
      millions=${millions%m}
      download https://dumps.wikimedia.org/wikidatawiki/entities/latest-lexemes.nt.gz wikidata-lexemes.nt.gz
      in_volume "rm -rf '$name.parts' && mkdir '$name.parts'"
      docker run --rm -v nrese-bench-data:/data -v "$HERE:/scripts:ro" "$PYTHON_IMAGE" sh -c \
        "gunzip -c /data/real/wikidata-lexemes.nt.gz | python /scripts/nt-filter.py \
         | head -n $(( millions * 1000000 )) > /data/real/$name.parts/slice.filtered"
      docker run --rm -v nrese-bench-data:/data "$OXIGRAPH_IMAGE" convert --from-format nt \
        --from-file "/data/real/$name.parts/slice.filtered" --to-file "/data/real/$name.parts/slice.nt"
      in_volume "rm -f '$name.parts/slice.filtered'"
      in_volume "mv '$name.parts/slice.nt' '/data/$name.nt' && rm -rf '$name.parts'
        echo \"$name: \$(wc -l < '/data/$name.nt') triples, \$(du -h '/data/$name.nt' | cut -f1)\""
      ;;
    wikidata-lexemes)
      download https://dumps.wikimedia.org/wikidatawiki/entities/latest-lexemes.nt.gz wikidata-lexemes.nt.gz
      in_volume "[ -s wikidata-lexemes.nt ] || gunzip -k wikidata-lexemes.nt.gz"
      convert wikidata-lexemes "wikidata-lexemes.nt" filter
      ;;
    *)
      echo "unknown dataset: $name" >&2
      exit 1
      ;;
  esac
done
