#!/usr/bin/env bash
# Maps the fetched SNB data sets (snb-fetch.sh) to RDF 1.2 N-Triples, variants A and B
# (snb_to_rdf.py), hashed and stored read-only beside the CSV. Idempotent: a scale whose
# SHA256SUMS exists is not mapped again; its sums are checked instead.
#
#   bash benches/property-graphs/snb-map.sh 1 3      # scale factors; results to $FLEET_OUT
set -euo pipefail
A=${SNB_ASSETS:-$HOME/nrese-assets/ldbc-snb-20261030}
here=$(cd "$(dirname "$0")" && pwd)
python=${PYTHON:-python3}
for sf in "$@"; do
  d=$A/rdf/sf$sf
  if [ ! -f "$d/SHA256SUMS" ]; then
    rm -rf "$d.part"; mkdir -p "$d.part"
    start=$(date +%s)
    "$python" -I "$here/snb_to_rdf.py" \
      "$A/csv/sf$sf/bi-sf$sf-composite-merged-fk/graphs/csv/bi/composite-merged-fk/initial_snapshot" "$d.part"
    echo "sf$sf map_s $(( $(date +%s) - start ))" >> "$FLEET_OUT/times.txt"
    (cd "$d.part" && sha256sum snb-a.nt snb-b.nt counts.json > SHA256SUMS)
    mv "$d.part" "$d"
    chmod -R a-w "$d"
  else
    (cd "$d" && sha256sum --check --quiet SHA256SUMS)
  fi
  cp "$d/SHA256SUMS" "$FLEET_OUT/SHA256SUMS-sf$sf"
  cp "$d/counts.json" "$FLEET_OUT/counts-sf$sf.json"
  ls -l "$d" >> "$FLEET_OUT/files.txt"
done
"$python" --version >> "$FLEET_OUT/times.txt"
