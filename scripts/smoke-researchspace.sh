#!/usr/bin/env bash
# Checks a ResearchSpace platform that runs on NRESE (ops/researchspace/docker-compose.yml)
# by using it as a client would, through ResearchSpace itself:
#   1. log in
#   2. query through its SPARQL endpoint: counts over all graphs, GRAPH, ASK, CONSTRUCT,
#      a property path
#   3. write through it: a SPARQL update, then a resource created, read and deleted with
#      its LDP container API
#   4. a keyword search as its stock templates ask it (Blazegraph's bds:search)
#
# Usage: scripts/smoke-researchspace.sh [ResearchSpace URL] [user] [password]
#        (defaults: http://127.0.0.1:10214 admin admin)
# Exits non-zero on the first failure and says what failed. It removes what it creates.
set -euo pipefail

RS="${1:-http://127.0.0.1:10214}"
USER_NAME="${2:-admin}"
PASSWORD="${3:-admin}"
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT
JAR="$WORK/cookies"

RDFS_LABEL="http://www.w3.org/2000/01/rdf-schema#label"
CONTAINER="http://www.researchspace.org/resource/system/fieldDefinitionContainer"
TEST_GRAPH="https://nrese.dev/smoke/researchspace"

step=0
say() { step=$((step + 1)); printf '%2d. %s\n' "$step" "$1"; }
fail() { printf 'FAILED: %s\n' "$1" >&2; exit 1; }
urlencode() {
  local text=$1 out="" i c
  for ((i = 0; i < ${#text}; i++)); do
    c=${text:i:1}
    case $c in
      [a-zA-Z0-9.~_-]) out+=$c ;;
      *) out+=$(printf '%%%02X' "'$c") ;;
    esac
  done
  printf '%s' "$out"
}
# query <SPARQL> [Accept]: the answer goes to $WORK/body; fails unless 200.
query() {
  local status
  status=$(curl -sS -b "$JAR" -o "$WORK/body" -w '%{http_code}' -G \
    -H "Accept: ${2:-application/sparql-results+json}" --data-urlencode "query=$1" "$RS/sparql")
  [ "$status" = 200 ] || fail "query answered $status: $1 -- $(head -c 300 "$WORK/body")"
}
update() {
  local status
  status=$(curl -sS -b "$JAR" -o "$WORK/body" -w '%{http_code}' --data-urlencode "update=$1" "$RS/sparql")
  [ "$status" = 200 ] || [ "$status" = 204 ] || fail "update answered $status: $1 -- $(head -c 300 "$WORK/body")"
}
has() { grep -q -- "$1" "$WORK/body" || fail "$2: $(head -c 400 "$WORK/body")"; }
# The single number a COUNT query returned.
number() { sed -n 's/.*"value" *: *"\([0-9]*\)".*/\1/p' "$WORK/body" | head -1; }

say "log in to ResearchSpace"
status=$(curl -sS -c "$JAR" -o /dev/null -w '%{http_code}' -d "username=$USER_NAME&password=$PASSWORD" "$RS/login")
[ "$status" = 302 ] || fail "login answered $status"

say "its system data is in the store and visible without naming graphs"
query "SELECT (COUNT(*) AS ?n) WHERE { ?s ?p ?o }"
statements=$(number)
[ "${statements:-0}" -gt 1000 ] || fail "only ${statements:-0} statements: $(cat "$WORK/body")"
query "SELECT (COUNT(DISTINCT ?g) AS ?n) WHERE { GRAPH ?g { ?s ?p ?o } }"
graphs=$(number)
[ "${graphs:-0}" -gt 100 ] || fail "only ${graphs:-0} named graphs"
printf '    %s statements in %s named graphs\n' "$statements" "$graphs"

say "ASK, CONSTRUCT and a property path"
query "ASK { ?s a <http://www.w3.org/ns/ldp#Container> }"
has true "no LDP container found"
query "CONSTRUCT { ?s a ?t } WHERE { ?s a ?t } LIMIT 3" text/turtle
[ -s "$WORK/body" ] || fail "empty CONSTRUCT result"
query "PREFIX ldp: <http://www.w3.org/ns/ldp#> SELECT (COUNT(*) AS ?n) WHERE { ?c ldp:contains+ ?x }"
[ "$(number)" -gt 0 ] || fail "no container membership found by a path query"

say "a SPARQL update through ResearchSpace"
update "INSERT DATA { GRAPH <$TEST_GRAPH> { <$TEST_GRAPH/a> <$RDFS_LABEL> \"Anna Amalia Bibliothek\" } }"
query "SELECT ?l WHERE { <$TEST_GRAPH/a> <$RDFS_LABEL> ?l }"
has "Anna Amalia Bibliothek" "the inserted statement isn't visible"

say "a resource through the LDP container API: create, find, read, delete"
status=$(curl -sS -b "$JAR" -D "$WORK/headers" -o "$WORK/body" -w '%{http_code}' -X POST \
  -H 'Content-Type: text/turtle' -H 'Slug: nrese-smoke-field' \
  --data-binary "<> a <http://www.researchspace.org/resource/system/fields/Field> ; <$RDFS_LABEL> \"created by the NRESE smoke test\" ." \
  "$RS/container?uri=$(urlencode "$CONTAINER")")
[ "$status" = 201 ] || fail "creating the resource answered $status: $(head -c 300 "$WORK/body")"
resource=$(tr -d '\r' < "$WORK/headers" | sed -n 's/^[Ll]ocation: //p')
[ -n "$resource" ] || fail "no Location for the created resource"
query "PREFIX ldp: <http://www.w3.org/ns/ldp#> ASK { <$CONTAINER> ldp:contains ?x . ?x <$RDFS_LABEL> \"created by the NRESE smoke test\" }"
has true "the container doesn't contain the new resource"
status=$(curl -sS -b "$JAR" -o "$WORK/body" -w '%{http_code}' -H 'Accept: text/turtle' "$RS/container?uri=$(urlencode "$resource")")
[ "$status" = 200 ] || fail "reading the resource answered $status"
has "created by the NRESE smoke test" "the resource's description lacks its label"
status=$(curl -sS -b "$JAR" -o "$WORK/body" -w '%{http_code}' -X DELETE "$RS/container?uri=$(urlencode "$resource")")
[ "$status" = 200 ] || [ "$status" = 204 ] || fail "deleting the resource answered $status"
query "ASK { ?s <$RDFS_LABEL> \"created by the NRESE smoke test\" }"
has false "the deleted resource is still there"

say "a keyword search as the stock templates ask it (bds:search)"
query "PREFIX bds: <http://www.bigdata.com/rdf/search#> SELECT ?s WHERE { ?s <$RDFS_LABEL> ?l . ?l bds:search \"Anna*\" }"
has "$TEST_GRAPH/a" "the keyword search doesn't find the label"

say "remove what was created"
update "DROP GRAPH <$TEST_GRAPH>"
query "ASK { GRAPH <$TEST_GRAPH> { ?s ?p ?o } }"
has false "the test graph is still there"

printf 'ok: %d steps against %s\n' "$step" "$RS"
