#!/usr/bin/env bash
# The SPARQL group's datasets (benches/cases/sparql.toml), made from their published sources
# into one directory: each source is fetched once, checked against its published or pinned
# checksum, and turned into the N-Triples file every system loads. benches/datasets.toml
# freezes the results (`suite.py data freeze`); a rerun must give the same SHA-256.
#
#   benches/sparql/prepare.sh DATASET... [--out DIR]     (Linux with Docker; DIR default ~/owlrs/sparql-data)
#
# DATASET: dblp | dblp-kg | watdiv-10m | watdiv-100m | bsbm-10m | bsbm-100m | sp2bench-1m | sp2bench-25m
#          | gcare | wdbench (the last needs 1 TB free: khaleesi S: or Draco, not office-pc)
#
# Downloads go to DIR/src/ (untrusted: unpacked there, never run from the repository);
# generators run in throwaway containers named sparql-prep-*, capped (8 CPUs, 8 GB, lowest
# priority) so that they never compete with a colleague's work on a shared host.
set -euo pipefail
# This job's names (scripts/lib/ns.sh): its containers and volumes; the shared datasets' volume.
NS_ROOT=$(cd "$(dirname "$0")/../.." && pwd)
. "$NS_ROOT/scripts/lib/ns.sh"
DATA_VOLUME=$(ns_shared data)
HERE=$(cd "$(dirname "$0")" && pwd)
OUT=${SPARQL_DATA:-$HOME/owlrs/sparql-data}
sets=()
while [ $# -gt 0 ]; do
  case $1 in
    --out) OUT=$2; shift 2 ;;
    *) sets+=("$1"); shift ;;
  esac
done
[ ${#sets[@]} -gt 0 ] || { sed -n 2,16p "$0"; exit 2; }
SRC=$OUT/src
mkdir -p "$OUT" "$SRC"
CAP=(--cpus 8 --memory 8g --memory-swap 8g)

say() { echo "$(date '+%F %T') $*"; }
fetch() {  # URL FILE: once, resumable, keeping the server's date
  [ -s "$2" ] && return 0
  curl -sSLR --retry 5 -C - -o "$2.part" "$1" && mv "$2.part" "$2"
}
check() {  # FILE ALGORITHM DIGEST
  local got
  got=$("$2sum" "$1" | cut -d' ' -f1)
  [ "$got" = "$3" ] || { echo "$1: $2 $got, expected $3" >&2; exit 1; }
}
record() {  # FILE: what datasets.toml needs
  say "$(basename "$1"): $(stat -c %s "$1") bytes, sha256 $(sha256sum "$1" | cut -d' ' -f1), $(wc -l < "$1") lines"
}
quiet() {  # waits while another thread's timed measurement holds the host's quiet slot
  local lock=${NRESE_QUIET_DIR:-$HOME/.nrese-quiet-slot}
  while [ -d "$lock" ] && kill -0 "$(cat "$lock/pid" 2>/dev/null || echo 0)" 2>/dev/null; do
    say "quiet slot held by $(cat "$lock/who" 2>/dev/null): waiting"; sleep 60
  done
}
box() {  # NAME IMAGE sh -c SCRIPT: a capped throwaway container with OUT at /out; on a host
          # without Docker the script itself, in a capped scope at the lowest priority, with
          # /out read as OUT (the image's tool, java or g++, then comes from the host's PATH)
  local name=$1 image=$2; shift 2
  if command -v docker >/dev/null 2>&1; then
    ns_run_args "$(ns_name "sparql-prep-$name" "$NS_ROOT")" "$NS_ROOT" sparql
    docker run --rm "${NS_ARGS[@]}" "${CAP[@]}" --cpu-shares 2 -u "$(id -u):$(id -g)" \
      -v "$OUT:/out" -w /out "$image" "$@"
  else
    local script=${3//\/out/$OUT}
    systemd-run --user --scope -q -p MemoryMax=8G -p CPUQuota=800% nice -n 19 ionice -c3 sh -c "$script"
  fi
}

# DBLP, the monthly RDF snapshot of 1 April 2025 (CC0): the release Sparqloscope's DBLP
# benchmark was generated from (its docs/setup.md), with the MD5 dblp publishes beside it.
dblp() {
  local f=$SRC/dblp-2025-04-01.nt.gz
  fetch https://drops.dagstuhl.de/storage/artifacts/dblp/rdf/2025/dblp-2025-04-01.nt.gz "$f"
  check "$f" md5 6c5628d88b8e1b27eef918239a7e9802
  quiet; [ -s "$OUT/dblp-2025-04-01.nt" ] || { gunzip -c "$f" > "$OUT/dblp-2025-04-01.nt.part" && mv "$OUT/dblp-2025-04-01.nt.part" "$OUT/dblp-2025-04-01.nt"; }
  record "$OUT/dblp-2025-04-01.nt"
}

# DBLP's knowledge graph as Sparqloscope's Qleverfile downloads it (dblp_KG_with_associated_data.tar,
# a snapshot dblp replaces as it updates; pinned by the SHA-256 of the copy fetched on 12 Oct
# 2026, dblp's release of 11 Oct 2026): its dblp.ttl.gz, the KG proper (the published triple
# counts are its size; the archive's other files are OpenCitations' links), read as the
# Qleverfile reads it (`@base <https://dblp.org/rdf/schema>` first) and written as N-Triples by
# Jena's riot. The monthly N-Triples dump (dblp above) lacks the stream records several queries
# are about (dblp:relatedStream has no instance there).
dblp_kg() {
  local f=$SRC/dblp-kg/dblp_KG_with_associated_data.tar
  mkdir -p "$SRC/dblp-kg"
  fetch https://sparql.dblp.org/download/dblp_KG_with_associated_data.tar "$f"
  check "$f" sha256 "$(grep "dblp_KG_with_associated_data.tar" "$HERE/sources.sha256" | cut -d' ' -f1)"
  [ -s "$SRC/dblp-kg/dblp.ttl.gz" ] || tar -xf "$f" -C "$SRC/dblp-kg" dblp.ttl.gz
  if [ ! -s "$OUT/dblp-kg-20261011.nt" ]; then
    quiet
    box dblp-kg-riot nrese-bench/jena:6.2.0 sh -c \
      "{ echo '@base <https://dblp.org/rdf/schema> .'; gunzip -c /out/src/dblp-kg/dblp.ttl.gz; } \
       | riot --syntax=ttl --output=nt > /out/dblp-kg-20261011.nt.part && mv /out/dblp-kg-20261011.nt.part /out/dblp-kg-20261011.nt"
  fi
  record "$OUT/dblp-kg-20261011.nt"
}

# WatDiv's own datasets (University of Waterloo; no licence stated: internal use until its
# authors agree), generated by its authors with watdiv v0.6 at scale 100 and 1000.
watdiv() {
  local size=$1 lower=${1,,}
  local f=$SRC/watdiv.$size.tar.bz2
  fetch "https://dsg.uwaterloo.ca/watdiv/watdiv.$size.tar.bz2" "$f"
  check "$f" sha256 "$(grep "watdiv.$size.tar.bz2" "$HERE/sources.sha256" | cut -d' ' -f1)"
  # The archive holds the data (watdiv.SIZE.nt) and, for some sizes, the generator's state
  # (saved.txt), which instantiating the query templates needs (queries.sh); the state stays.
  if [ ! -s "$OUT/watdiv-$lower.nt" ]; then
    quiet
    mkdir -p "$SRC/watdiv-$lower" && tar -xjf "$f" -C "$SRC/watdiv-$lower"
    mv "$SRC/watdiv-$lower/watdiv.$size.nt" "$OUT/watdiv-$lower.nt"
  fi
  record "$OUT/watdiv-$lower.nt"
}

# BSBM V3.1 with the BIBM tools 0.7.8 (the generator and driver of the published V3.1
# results, April 2013): forward-chained (-fc) and Pareto-distributed (-pareto) as there, in
# N-Triples; the test driver's parameter data beside it (bsbm-TIER.td/). The product counts are
# the report's table (28480 and 284800, giving its 10119864 and 100062249 triples), not the
# 284826 of its example command line.
bsbm() {
  local tier=$1 products
  case $tier in 10m) products=28480 ;; 100m) products=284800 ;; *) echo "bsbm tier 10m or 100m" >&2; exit 2 ;; esac
  local f=$SRC/bibm-0.7.8.tgz
  fetch "https://sourceforge.net/projects/bibm/files/bibm-0.7.8.tgz/download" "$f"
  check "$f" sha256 4f36056e99d8d2fe7dbc99efc8b80f9629dce63119ccaeffcf90c9cf0e710793
  [ -d "$SRC/bibm/bibm" ] || { mkdir -p "$SRC/bibm" && tar -xf "$f" -C "$SRC/bibm"; }
  if [ ! -s "$OUT/bsbm-$tier.nt" ]; then
    quiet; box "bsbm-$tier" eclipse-temurin:21-jre sh -c \
      "cd /out/src/bibm/bibm && java -Xmx6g -cp 'bin:lib/*' benchmark.generator.Generator \
       -fc -pareto -pc $products -s nt -fn /out/bsbm-$tier -dir /out/bsbm-$tier.td"
  fi
  record "$OUT/bsbm-$tier.nt"
}

# SP2Bench v1.01 (BSD): the generator compiled from its source (v1.01 fixes the 64-bit bug
# of v1.00, so documents match the paper's), a document of the given number of triples.
sp2bench() {
  local tier=$1 triples
  case $tier in 1m) triples=1000000 ;; 25m) triples=25000000 ;; *) echo "sp2bench tier 1m or 25m" >&2; exit 2 ;; esac
  local f=$SRC/sp2b-v1_01-full.tar.gz
  fetch https://dbis.informatik.uni-freiburg.de/content/projects/SP2B/docs/sp2b-v1_01-full.tar.gz "$f"
  check "$f" sha256 6640506cf2e2d142ea3435b1b8cc3999879349ebcd1a3cdc601db831f5cb3d73
  [ -d "$SRC/sp2b/sp2b" ] || { mkdir -p "$SRC/sp2b" && tar -xzf "$f" -C "$SRC/sp2b"; }
  # The Makefile's compiler line, without its -m32 (v1.01 is 64-bit clean), with the C headers
  # its 2008 sources took for granted, and at -O2: its
  # authors warn that -O3 changes the floating point and with it the documents.
  if [ ! -x "$SRC/sp2b/build/sp2b_gen" ]; then
    quiet
    box sp2b-build gcc:14 sh -c "rm -rf /out/src/sp2b/build && cp -r /out/src/sp2b/sp2b/src /out/src/sp2b/build \
      && cd /out/src/sp2b/build && make -s CPP='g++ -w -O2 -include cstring -include cstdlib -include cstdio' && ls -la sp2b_gen"
  fi
  # The generator writes Turtle with prefixes; Jena's riot writes it once as N-Triples, so
  # that every system reads the same statements in the simplest syntax.
  if [ ! -s "$OUT/sp2bench-$tier.nt" ]; then
    quiet
    box "sp2b-$tier" gcc:14 sh -c "cd /out/src/sp2b/build && ./sp2b_gen -t $triples /out/src/sp2b/sp2bench-$tier.ttl"
    box "sp2b-riot-$tier" nrese-bench/jena:6.2.0 sh -c \
      "riot --syntax=ttl --output=nt /out/src/sp2b/sp2bench-$tier.ttl > /out/sp2bench-$tier.nt.part \
       && mv /out/sp2bench-$tier.nt.part /out/sp2bench-$tier.nt"
  fi
  record "$OUT/sp2bench-$tier.nt"
}

# G-CARE (SIGMOD 2020): its data graphs, query graphs and true cardinalities as published
# (Google Drive, linked from github.com/yspark-dblab/gcare), converted to N-Triples and
# SPARQL by convert.py (each vertex label a type, each edge label a predicate).
gcare() {
  local d=$SRC/gcare
  mkdir -p "$d"
  for spec in queryset:1Dlj43rBAOVPAsfzKlYxIbZ9RsqeGM_MN true_cardinality:1Bc6Q2RZQTcIB8IfOw5KafNYwPhq2BO94 \
              dataset:1HAgSVE-24NOap6_Q1_twH56Dkb2kPvGU; do
    local n=${spec%%:*} id=${spec#*:}
    fetch "https://drive.usercontent.google.com/download?id=$id&export=download&confirm=t" "$d/$n.tar.gz"
    check "$d/$n.tar.gz" sha256 "$(grep "gcare-$n.tar.gz" "$HERE/sources.sha256" | cut -d' ' -f1)"
    [ -d "$d/$n" ] || { mkdir -p "$d/$n" && tar -xzf "$d/$n.tar.gz" -C "$d/$n"; }
  done
  # The queries go to OUT/queries/gcare-NAME and stay there (pinned in queries.sha256).
  quiet; python3 -I "$HERE/gcare/convert.py" "$d" "$OUT" "$OUT/queries"
  for g in aids human lubm80 yago; do record "$OUT/gcare-$g.nt"; done
}

# WDBench (ISWC 2022): Wikidata truthy of 2021-06-23 reduced to its direct properties, as
# its authors publish it on Figshare (CC0), about 1.25 B statements. Needs about 1 TB of
# disk for the file and the stores, and more memory than office-pc and phuoc-yu have for
# the competitors' loads: khaleesi S: or Draco.
wdbench() {
  local f=$SRC/wdbench-truthy-direct.nt.bz2
  fetch "${WDBENCH_URL:?set WDBENCH_URL to the Figshare file of figshare.com/s/50b7544ad6b1f51de060}" "$f"
  check "$f" sha256 "$(grep "wdbench-truthy-direct.nt.bz2" "$HERE/sources.sha256" | cut -d' ' -f1)"
  quiet; [ -s "$OUT/wdbench.nt" ] || { bunzip2 -c "$f" > "$OUT/wdbench.nt.part" && mv "$OUT/wdbench.nt.part" "$OUT/wdbench.nt"; }
  record "$OUT/wdbench.nt"
}

for s in "${sets[@]}"; do
  say "== $s"
  case $s in
    dblp) dblp ;;
    dblp-kg) dblp_kg ;;
    watdiv-10m) watdiv 10M ;;
    watdiv-100m) watdiv 100M ;;
    bsbm-*) bsbm "${s#bsbm-}" ;;
    sp2bench-*) sp2bench "${s#sp2bench-}" ;;
    gcare) gcare ;;
    wdbench) wdbench ;;
    *) echo "unknown dataset $s" >&2; exit 2 ;;
  esac
done
say done
