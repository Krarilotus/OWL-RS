#!/usr/bin/env bash
# The NPD benchmark's ontology and queries (github.com/ontop/npd-benchmark, Apache-2.0),
# at a pinned commit, for the QL rewriting's stress case
# (benches/reasoning/queries/npd-stress/README.md):
#
#   benches/reasoning/prepare-npd.sh [DIR]   (default target/npd)
#
# Queries 28-30 use `xsd:` without declaring it; the declaration is added.
set -euo pipefail
DIR=${1:-target/npd}
COMMIT=5b1eeb39c36c0dd5c69c835fd93d204d19be30dd
BASE=https://raw.githubusercontent.com/ontop/npd-benchmark/$COMMIT
mkdir -p "$DIR/queries"
curl -sSfL -o "$DIR/npd-v2-ql.owl" "$BASE/ontology/npd-v2-ql.owl"
for i in $(seq -w 1 31); do
  curl -sSfL -o "$DIR/queries/q$i.rq" "$BASE/queries/$i.rq"
done
for i in 28 29 30; do
  sed -i '1i PREFIX xsd: <http://www.w3.org/2001/XMLSchema#>' "$DIR/queries/q$i.rq"
done
echo "NPD ontology and 31 queries in $DIR"
