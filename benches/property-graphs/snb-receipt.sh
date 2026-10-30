#!/usr/bin/env bash
# The SNB set's receipt: one SHA256SUMS over every file snb-fetch.sh and snb-map.sh made (the
# tarballs, the RDF mappings and their counts), checked in full, read-only, with a manifest of
# how each was made. A second host making the same set must write the same receipt.
#
#   bash benches/property-graphs/snb-receipt.sh      # results to $FLEET_OUT
set -euo pipefail
A=${SNB_ASSETS:-$HOME/nrese-assets/ldbc-snb-20261030}
here=$(cd "$(dirname "$0")" && pwd)
root=$(cd "$here/../.." && pwd)
cd "$A"
receipt=$(mktemp "$A/.SHA256SUMS.XXXXXX")
{
  for t in source/*.sha256; do sed "s#  #  source/#" "$t"; done
  for s in rdf/sf*/SHA256SUMS; do d=$(dirname "$s"); sed "s#  #  $d/#" "$s"; done
} | sort -k2 > "$receipt"
sha256sum --check --quiet "$receipt"
if [ -f SHA256SUMS ]; then
  cmp SHA256SUMS "$receipt"
  rm -f "$receipt"
else
  chmod a-w "$receipt"
  mv "$receipt" SHA256SUMS
  cat > MANIFEST.md <<EOF
# LDBC SNB Interactive v2 initial data sets, SF1 and SF3 (read-only)

- source/: the datagen's published output, $(sed -n 's/.*\(bi-sf[0-9]*\)-.*/\1/p' source/*.sha256 | tr '\n' ' ')from
  https://datasets.ldbcouncil.org/bi-pre-audit/ (snb-fetch.sh); csv/: the same, extracted.
- rdf/sfN/: RDF 1.2 N-Triples, variant A (edges with properties reified) and B (every edge), and
  the CSV's counts (snb-map.sh, snb_to_rdf.py of OWL-RS $(git -C "$root" rev-parse HEAD 2>/dev/null || echo unknown)).
- SHA256SUMS: every file above; verified in full when written.
- Not an LDBC benchmark result; LDBC data under Apache-2.0.
EOF
  chmod a-w MANIFEST.md
fi
cp SHA256SUMS "$FLEET_OUT/SHA256SUMS"
cp MANIFEST.md "$FLEET_OUT/MANIFEST.md"
du -sb source csv rdf > "$FLEET_OUT/sizes.txt"
