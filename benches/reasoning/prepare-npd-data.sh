#!/usr/bin/env bash
# NPD's data as RDF, and Ontop's answers as the reference (benches/reasoning/queries/
# npd-stress/README.md): the benchmark's PostgreSQL dump (github.com/ontop/npd-benchmark,
# Apache-2.0; the data NLOD) loaded into PostgreSQL, its mappings materialised by Ontop
# (Apache-2.0) without the ontology (the asserted statements, as a store loads them), and,
# with --reference, NPD's queries answered by Ontop's endpoint over the database with its
# existential reasoning (OWL 2 QL certain answers):
#
#   benches/reasoning/prepare-npd-data.sh [--reference] [DIR]   (default target/npd)
#
# Needs Docker and benches/reasoning/prepare-npd.sh's ontology and queries in DIR (it runs
# it if they aren't there). Writes DIR/npd-v2-ql.nt (the ontology as N-Triples),
# DIR/db/npd-abox.nt (about 2.0 M statements) and with --reference DIR/ontop-counts.tsv
# (query, rows, ms). The containers and their network are removed at the end; the images
# stay for the next run.
set -euo pipefail
REFERENCE=0
if [ "${1:-}" = --reference ]; then REFERENCE=1; shift; fi
DIR=${1:-target/npd}
COMMIT=5b1eeb39c36c0dd5c69c835fd93d204d19be30dd
BASE=https://raw.githubusercontent.com/ontop/npd-benchmark/$COMMIT
ONTOP=ontop/ontop:5.1.2
POSTGRES=postgres:16-alpine
JDBC=https://repo1.maven.org/maven2/org/postgresql/postgresql/42.7.4/postgresql-42.7.4.jar
NET=npd-prepare-net
PG=npd-prepare-pg
ENDPOINT=npd-prepare-endpoint
export MSYS_NO_PATHCONV=1
ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
[ -f "$DIR/npd-v2-ql.owl" ] || bash "$ROOT/benches/reasoning/prepare-npd.sh" "$DIR"
mkdir -p "$DIR/db" "$DIR/jdbc"
ABS=$(cd "$DIR" && pwd -W 2>/dev/null || pwd)
for f in data/postgres/npd.psql mappings/postgres/npd-v2-ql.obda; do
  [ -f "$DIR/db/$(basename "$f")" ] || curl -sSfL -o "$DIR/db/$(basename "$f")" "$BASE/$f"
done
[ -f "$DIR/jdbc/postgresql-42.7.4.jar" ] || curl -sSfL -o "$DIR/jdbc/postgresql-42.7.4.jar" "$JDBC"
cat > "$DIR/db/npd.properties" <<EOF
jdbc.url = jdbc:postgresql://$PG:5432/npd
jdbc.user = npd
jdbc.password = npd
jdbc.driver = org.postgresql.Driver
EOF
# The endpoint's: with the tree-witness rewriting, for QL certain answers.
{ cat "$DIR/db/npd.properties"; echo "ontop.existentialReasoning = true"; } > "$DIR/db/npd-reasoning.properties"

cleanup() {
  docker rm -f "$PG" "$ENDPOINT" > /dev/null 2>&1 || true
  docker network rm "$NET" > /dev/null 2>&1 || true
}
trap cleanup EXIT
cleanup
docker network create "$NET" > /dev/null
docker run -d --name "$PG" --network "$NET" -e POSTGRES_USER=npd -e POSTGRES_PASSWORD=npd \
  -e POSTGRES_DB=npd -v "$ABS/db:/dump:ro" "$POSTGRES" > /dev/null
until docker exec "$PG" pg_isready -U npd -d npd > /dev/null 2>&1; do sleep 1; done
sleep 2
docker exec "$PG" psql -q -U npd -d npd -v ON_ERROR_STOP=1 -f /dump/npd.psql > /dev/null

# The asserted statements: the mappings without the ontology (no hierarchy saturation).
docker run --rm --network "$NET" -v "$ABS/jdbc:/opt/ontop/jdbc:ro" -v "$ABS/db:/work" "$ONTOP" \
  ontop materialize -m /work/npd-v2-ql.obda -p /work/npd.properties -f ntriples \
  -o /work/npd-abox.nt 2>&1 | grep -v "ANTLR Tool version" | tail -3
[ -s "$DIR/db/npd-abox.nt" ] || { echo "materialising failed" >&2; exit 1; }
# The ontology as N-Triples (RDF/XML by its extension).
cp "$DIR/npd-v2-ql.owl" "$DIR/npd-v2-ql.rdf"
NRESE=${NRESE_SERVER:-$ROOT/target/release/nrese-server}
[ -x "$NRESE" ] || NRESE=$ROOT/target/debug/nrese-server
"$NRESE" convert "$DIR/npd-v2-ql.rdf" "$DIR/npd-v2-ql.nt"

if [ "$REFERENCE" = 1 ]; then
  docker run -d --name "$ENDPOINT" --network "$NET" -p 127.0.0.1:18080:8080 \
    -e ONTOP_MAPPING_FILE=/work/npd-v2-ql.obda -e ONTOP_ONTOLOGY_FILE=/npd/npd-v2-ql.owl \
    -e ONTOP_PROPERTIES_FILE=/work/npd-reasoning.properties \
    -v "$ABS/jdbc:/opt/ontop/jdbc:ro" -v "$ABS/db:/work:ro" -v "$ABS:/npd:ro" "$ONTOP" > /dev/null
  until curl -sf "http://127.0.0.1:18080/sparql?query=ASK%7B%7D" > /dev/null 2>&1; do sleep 2; done
  : > "$DIR/ontop-counts.tsv"
  for f in "$DIR"/queries/q*.rq; do
    start=$(date +%s%N)
    rows=$(curl -s -m 300 http://127.0.0.1:18080/sparql -H "Accept: text/tab-separated-values" \
      --data-urlencode "query=$(cat "$f")" | tail -n +2 | grep -c . || true)
    printf '%s\t%s\t%s\n' "$(basename "$f" .rq)" "$rows" $(( ($(date +%s%N) - start) / 1000000 )) \
      >> "$DIR/ontop-counts.tsv"
  done
fi
echo "NPD data: $(wc -l < "$DIR/db/npd-abox.nt") statements in $DIR/db/npd-abox.nt"
