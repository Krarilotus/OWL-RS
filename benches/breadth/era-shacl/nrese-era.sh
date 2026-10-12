#!/bin/sh
# ERA-SHACL's engine contract for NRESE: nrese-era.sh DATA SHAPES REPORT. The perf lab
# loads DATA into memory, loads SHAPES into the shapes graph, validates once and writes the
# report (N-Triples, which is Turtle). Its own timers give the two times upstream asks for.
set -eu
log=/tmp/perf_lab.log
perf_lab --load "$1" --shapes "$2" --runs 1 --warmup 0 --shacl-report "$3" \
  --label era-shacl --json /tmp/perf_lab.json 2> "$log" || { cat "$log" >&2; exit 1; }
cat "$log" >&2
# "loaded N quads in S s" and "shacl: N results, p50 MS ms (max ...)"
sed -n 's/^loaded \([0-9]*\) quads in \([0-9.]*\) s$/Data graph size: \1\nLoad time: \2/p' "$log"
sed -n 's/^shacl: \([0-9]*\) results, p50 \([0-9.]*\) ms.*/Report results: \1\nValidation ms: \2/p' "$log" |
  awk '/^Validation ms:/ { printf "Validation time: %.3f\n", $3 / 1000; next } { print }'
