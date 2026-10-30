#!/usr/bin/env bash
# The LDBC SNB Interactive v2 initial data sets: the datagen's published output (the BI initial
# snapshots in composite-merged-fk CSV, which the Interactive v2 implementations load), fetched into
# a shared asset directory, hashed and extracted read-only. Idempotent: a tarball whose .sha256
# exists is not fetched again.
#
#   bash benches/property-graphs/snb-fetch.sh 1 3      # scale factors; results to $FLEET_OUT
set -euo pipefail
A=${SNB_ASSETS:-$HOME/nrese-assets/ldbc-snb-20261030}
URL=https://datasets.ldbcouncil.org/bi-pre-audit
mkdir -p "$A/source" "$A/csv"
out=$FLEET_OUT
for sf in "$@"; do
  t=bi-sf$sf-composite-merged-fk.tar.zst
  if [ ! -f "$A/source/$t.sha256" ]; then
    timeout 1800 curl --fail --silent --show-error --retry 3 -o "$A/source/$t.part" "$URL/$t"
    mv "$A/source/$t.part" "$A/source/$t"
    (cd "$A/source" && sha256sum "$t" > "$t.sha256")
    chmod a-w "$A/source/$t" "$A/source/$t.sha256"
  fi
  cat "$A/source/$t.sha256" >> "$out/tarballs.sha256"
  if [ ! -d "$A/csv/sf$sf" ]; then
    rm -rf "$A/csv/sf$sf.part"; mkdir -p "$A/csv/sf$sf.part"
    timeout 1800 tar -x --use-compress-program=unzstd -f "$A/source/$t" -C "$A/csv/sf$sf.part"
    mv "$A/csv/sf$sf.part" "$A/csv/sf$sf"
    chmod -R a-w "$A/csv/sf$sf"
  fi
  (cd "$A/csv/sf$sf" && find . -type f | sort | xargs -d '\n' ls -l) > "$out/files-sf$sf.txt"
  # Header and first two rows of each initial-snapshot entity's first part, and rows per entity
  # (no `head` in a pipe: under pipefail its early close fails the reader).
  : > "$out/heads-sf$sf.txt"; : > "$out/rows-sf$sf.txt"
  rd() { case "$1" in *.gz) zcat "$1";; *) cat "$1";; esac; }
  for d in $(cd "$A/csv/sf$sf" && find . -path '*initial_snapshot*' -type f -name '*.csv*' -printf '%h\n' | sort -u); do
    first=$(ls "$A/csv/sf$sf/$d" | grep csv | sort | sed -n 1p)
    { echo "== $d/$first"; rd "$A/csv/sf$sf/$d/$first" | sed -n 1,3p; } >> "$out/heads-sf$sf.txt"
    n=0; for f in "$A/csv/sf$sf/$d"/*csv*; do c=$(rd "$f" | tail -n +2 | wc -l); n=$((n + c)); done
    echo "$d $n" >> "$out/rows-sf$sf.txt"
  done
done
df -h "$A" > "$out/df.txt"
du -sh "$A"/* >> "$out/df.txt"
