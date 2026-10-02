#!/usr/bin/env bash
# Bulk-load memory against load time on DBpedia core (67 M): no budget, then 2 GiB and
# 1 GiB budgets (spilled chunks of 1 GiB and 512 MiB). Peak RSS from rss-profile's samples.
#
# Usage: benches/probes/load-memory.sh   (Linux; DBpedia core in $SCRATCH/dbpedia)
set -u
source "$(dirname "$0")/common.sh"
cd "$SCRATCH"
for budget in 0 2147483648 1073741824; do
  echo "=== budget $budget"
  start=$(date +%s)
  NRESE_BULK_LOAD_MEMORY=$budget mkdir -p "$SCRATCH/noq"; bash "$REPO/benches/probes/rss-profile.sh" "$SCRATCH/dbpedia/dbpedia-core.nt" --queries "$SCRATCH/noq" >/dev/null 2>&1
  echo "wall $(( $(date +%s) - start )) s"
  grep -E "bulk load|load_s|loaded|spill|error" prof.log | cut -c1-260
  awk '{ if ($2 > m) m = $2 } END { print "peak rss MiB", m }' rss.log
done
rm -f prof.log rss.log
echo SPILL-DONE
