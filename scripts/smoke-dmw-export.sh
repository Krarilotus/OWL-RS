#!/usr/bin/env bash
# Replays what the Datamodel Workflow does to a triple store (its WP15 export and its
# validation question D18) against a running server, with curl:
#   1. export a module and its data with provenance, each into a graph of its own
#   2. export the same version again and get identical graphs
#   3. query across the graphs: a competency question and a provenance question
#   4. change data with SPARQL Update, on the combined endpoint
#   5. store the shapes and validate: conforming data, then data that breaks them
#   6. remove what it created
#
# Usage: scripts/smoke-dmw-export.sh [server URL]   (default http://127.0.0.1:8080)
# Needs a server with writes enabled, `store.default_graph = "union"` (the exports go into
# named graphs and the questions are asked without naming them), and an empty or
# disposable dataset. Exits non-zero on the first difference and says what differed.
set -euo pipefail

SERVER="${1:-http://127.0.0.1:8080}"
FIXTURES="$(cd "$(dirname "$0")/../fixtures/integration/dmw" && pwd)"
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

MODULE_GRAPH="https://example.org/model/petitions/1.0.0"
DATA_GRAPH="https://example.org/export/petitions/2026-09-30"
INVALID_GRAPH="https://example.org/export/petitions/draft"
SHAPES_GRAPH="http://rdf4j.org/schema/rdf4j#SHACLShapeGraph"

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
graph_url() { printf '%s/dataset/data?graph=%s' "$SERVER" "$(urlencode "$1")"; }

# request <expected status> <curl arguments…>: the body goes to $WORK/body.
request() {
  local expected=$1 status
  shift
  status=$(curl -sS -o "$WORK/body" -w '%{http_code}' "$@")
  [ "$status" = "$expected" ] || fail "expected $expected, got $status for: curl $* -- $(head -c 400 "$WORK/body")"
}
put_graph() { request "$3" -X PUT -H 'Content-Type: text/turtle' --data-binary @- "$(graph_url "$1")" < "$FIXTURES/$2"; }
# The graph as sorted N-Triples: the same for the same content.
read_graph() { request 200 -H 'Accept: application/n-triples' "$(graph_url "$1")"; sort "$WORK/body" > "$2"; }
select_csv() { request 200 -H 'Accept: text/csv' --data-urlencode "query=$1" "$SERVER/dataset/sparql"; tr -d '\r' < "$WORK/body" > "$WORK/rows"; }
body_has() { grep -q -- "$1" "$WORK/body" || fail "the answer lacks '$1': $(head -c 600 "$WORK/body")"; }

say "the server is ready, and queries read all graphs"
request 200 "$SERVER/readyz"
request 200 "$SERVER/version"
body_has '"default_graph":"union"' 

say "export the module and the data, each into its own graph"
for graph in "$MODULE_GRAPH" "$DATA_GRAPH" "$INVALID_GRAPH" "$SHAPES_GRAPH"; do
  curl -sS -o /dev/null -X DELETE "$(graph_url "$graph")"
done
request 404 "$(graph_url "$DATA_GRAPH")"
put_graph "$MODULE_GRAPH" module.ttl 201
put_graph "$DATA_GRAPH" data.ttl 201
read_graph "$DATA_GRAPH" "$WORK/data-first.nt"
[ "$(wc -l < "$WORK/data-first.nt")" -eq 18 ] || fail "the data graph has $(wc -l < "$WORK/data-first.nt") statements, not 18"

say "export the same version again: the graphs are identical"
put_graph "$MODULE_GRAPH" module.ttl 200
put_graph "$DATA_GRAPH" data.ttl 200
read_graph "$DATA_GRAPH" "$WORK/data-second.nt"
cmp -s "$WORK/data-first.nt" "$WORK/data-second.nt" || fail "the re-exported data graph differs"

say "a competency question across data and module"
select_csv "PREFIX ex: <https://example.org/model/petitions#>
PREFIX rdfs: <http://www.w3.org/2000/01/rdf-schema#>
SELECT ?name ?date WHERE {
  GRAPH <$MODULE_GRAPH> { ?class rdfs:subClassOf ex:Document }
  GRAPH <$DATA_GRAPH> { ?p a ?class ; ex:petitioner/ex:name ?name ; ex:submittedOn ?date }
} ORDER BY ?date"
printf 'name,date\nJohannes Textor,1452-03-14\nMargarethe Weber,1453-11-02\n' > "$WORK/expected"
cmp -s "$WORK/rows" "$WORK/expected" || fail "competency question: $(cat "$WORK/rows")"

say "a provenance question: which source does each petition come from"
select_csv "PREFIX prov: <http://www.w3.org/ns/prov#>
PREFIX ex: <https://example.org/model/petitions#>
SELECT (COUNT(?p) AS ?petitions) ?source WHERE {
  ?p a ex:Petition ; prov:wasGeneratedBy/prov:used/prov:value ?source
} GROUP BY ?source"
printf 'petitions,source\n2,Supplik 17\n' > "$WORK/expected"
cmp -s "$WORK/rows" "$WORK/expected" || fail "provenance question: $(cat "$WORK/rows")"

say "change data with SPARQL Update on the combined endpoint"
request 204 --data-urlencode "update=PREFIX ex: <https://example.org/model/petitions#>
DELETE { GRAPH <$DATA_GRAPH> { ?person ex:name ?old } }
INSERT { GRAPH <$DATA_GRAPH> { ?person ex:name \"Johannes Textor der Ältere\" } }
WHERE { GRAPH <$DATA_GRAPH> { ?person ex:name ?old FILTER(?old = \"Johannes Textor\") } }" "$SERVER/dataset/sparql"
request 200 -H 'Accept: application/sparql-results+json' --data-urlencode "query=ASK { GRAPH <$DATA_GRAPH> { ?s ?p \"Johannes Textor der Ältere\" } }" "$SERVER/dataset/sparql"
body_has true
put_graph "$DATA_GRAPH" data.ttl 200
read_graph "$DATA_GRAPH" "$WORK/data-third.nt"
cmp -s "$WORK/data-first.nt" "$WORK/data-third.nt" || fail "the export didn't restore the data graph"

say "store the shapes; the exported data conforms"
put_graph "$SHAPES_GRAPH" shapes.ttl 201
request 200 -H 'Accept: application/json' "$SERVER/dataset/shacl"
body_has '"conforms":true'
body_has '"shapes":4'

say "data that breaks the shapes is reported, with what is wrong"
put_graph "$INVALID_GRAPH" invalid.ttl 201
request 200 -H 'Accept: application/json' "$SERVER/dataset/shacl"
body_has '"conforms":false'
body_has 'https://example.org/data/petition-3'
body_has 'MinCountConstraintComponent'
body_has 'DatatypeConstraintComponent'
body_has 'every petition has a petitioner'
[ "$(grep -o '"focusNode"' "$WORK/body" | wc -l)" -eq 3 ] || fail "expected 3 results: $(cat "$WORK/body")"
request 200 -H 'Accept: application/json' "$SERVER/dataset/shacl?graph=$(urlencode "$DATA_GRAPH")"
body_has '"conforms":true'

say "validate a draft against shapes sent with the request, storing nothing"
request 200 -H 'Accept: text/turtle' -H 'Content-Type: text/turtle' --data-binary @- "$SERVER/dataset/shacl?graph=$(urlencode "$INVALID_GRAPH")" < "$FIXTURES/shapes.ttl"
body_has 'ValidationReport'
body_has 'petition-3'

say "remove what was created"
for graph in "$MODULE_GRAPH" "$DATA_GRAPH" "$INVALID_GRAPH" "$SHAPES_GRAPH"; do
  request 204 -X DELETE "$(graph_url "$graph")"
  request 404 "$(graph_url "$graph")"
done

printf 'ok: %d steps against %s\n' "$step" "$SERVER"
