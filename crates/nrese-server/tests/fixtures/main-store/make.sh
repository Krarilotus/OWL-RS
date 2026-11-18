#!/usr/bin/env bash
# Writes this fixture: a data directory as `main`'s server leaves it, with every kind of
# state it keeps, and main's answers to `queries.tsv` (upgrade_tests.rs opens it with v2).
#
#   make.sh MAIN_NRESE_SERVER [PORT]
#
# MAIN_NRESE_SERVER: a `nrese-server` built from `main` (d9a4b24 for the fixture as
# committed). Replaces `data/`, `backup/` and `expected/` here. The data directory gets:
# - a checkpoint and the reasoning marker (an offline bulk load under owl2-rl), then a WAL
#   tail no checkpoint covers (inserts and a delete over HTTP; the server is stopped hard,
#   as a crash or a kill would, so nothing more is written);
# - archived WAL segments (`store.wal_archive`), an image backup (copied to `backup/`);
# - the default repository's changed settings (`repository.json`) and namespaces;
# - repositories: `rdfs` (its own reasoning), `rules` (owl2-rl and N3 rules), `ql`
#   (owl2-ql), `horst` (owl-horst), `dotted.1`,
#   and two whose ids main accepts and v2's create refuses (`.staging`, `.trash-kept`);
# - the access state: settings, a role, users (a local login, an administrator, a user
#   named as identity providers name them, `auth0|u1`), a workspace with a member, and
#   saved queries in a workspace and in a personal space (main can't save one in the
#   personal space of `auth0|u1`: a 500, '|' can't stand in an IRI).
# Sessions and the result cache live in memory only and leave nothing behind.
set -euo pipefail
BIN=$(cd "$(dirname "$1")" && pwd)/$(basename "$1")
PORT=${2:-18731}
HERE=$(cd "$(dirname "$0")" && pwd)
WORK=$(mktemp -d)
TOKEN=fixture-admin
URL=http://127.0.0.1:$PORT

rm -rf "$HERE/data" "$HERE/backup" "$HERE/expected"
mkdir -p "$HERE/expected"
cat > "$WORK/config.toml" <<EOF
[server]
bind_addr = "127.0.0.1:$PORT"
deployment_posture = "internal-authenticated"
[store]
mode = "on-disk"
data_dir = "$(cd "$HERE" && pwd -W 2>/dev/null || pwd)/data"
wal_archive = true
[reasoner]
mode = "owl2-rl"
[auth]
mode = "bearer-static"
[auth.bearer_static]
admin_token = "$TOKEN"
read_token = "fixture-read"
EOF
export NRESE_LOG=warn RUST_LOG=warn

# The checkpoint: an offline bulk load, reasoned.
"$BIN" load --config "$WORK/config.toml" "$HERE/input/base.ttl" "$HERE/input/graph.nq"

"$BIN" --config "$WORK/config.toml" > "$WORK/server.log" 2>&1 &
PID=$!
for _ in $(seq 1 100); do
  curl -sf "$URL/readyz" > /dev/null && break
  sleep 0.2
done
curl -sf "$URL/readyz" > /dev/null || { cat "$WORK/server.log"; exit 1; }

call() { # METHOD PATH [BODY [CONTENT-TYPE]]
  local out
  out=$(curl -s -w '\n%{http_code}' -X "$1" "$URL$2" -H "Authorization: Bearer $TOKEN" \
    ${3:+--data-binary "$3"} -H "Content-Type: ${4:-application/json}")
  case "${out##*$'\n'}" in
    2*) ;;
    *) echo "$1 $2 -> $out" >&2; kill -9 "$PID"; exit 1 ;;
  esac
}
update() { call POST "$1/update" "$2" application/sparql-update; }

# The WAL tail of the default repository: inserts, into a named graph too, and a delete.
update /dataset "PREFIX : <http://e/> INSERT DATA { :fay a :Student ; :memberOf :dept1 . :dept2 :subOrganisationOf :school1 . GRAPH :g1 { :gus a :Faculty } }"
update /dataset "PREFIX : <http://e/> DELETE DATA { :cat a :Faculty }"

# The default repository's settings and namespaces.
call PATCH /api/v1/repositories/nrese '{"title": "Main store"}'
call PUT /api/v1/repositories/nrese/namespaces/ex 'http://e/' text/plain

# Repositories.
call PUT /api/v1/repositories/rdfs '{"title": "RDFS repository", "reasoning": "rdfs"}'
update /api/v1/repositories/rdfs "PREFIX : <http://e/> PREFIX rdfs: <http://www.w3.org/2000/01/rdf-schema#> INSERT DATA { :Student rdfs:subClassOf :Person . :hal a :Student }"
call PUT /api/v1/repositories/rdfs/namespaces/e 'http://e/' text/plain
call PUT /api/v1/repositories/rules '{"title": "With rules", "reasoning": "owl2-rl"}'
# curl reads a body starting with `@` from that file.
call PUT "/api/v1/repositories/rules/rules?name=rules.n3" "@$HERE/input/rules.n3" text/n3
update /api/v1/repositories/rules "PREFIX : <http://e/> PREFIX rdfs: <http://www.w3.org/2000/01/rdf-schema#> INSERT DATA { :Student rdfs:subClassOf :Person . :ivy a :Student ; :memberOf :d . :d :subOrganisationOf :u }"
# owl2-ql: v2 answers through existentials there (the QL rewriting, `auto`) and relates
# every individual to itself by a reflexive property; owl-horst: unchanged rules.
call PUT /api/v1/repositories/ql '{"title": "QL repository", "reasoning": "owl2-ql"}'
update /api/v1/repositories/ql "PREFIX : <http://e/> PREFIX owl: <http://www.w3.org/2002/07/owl#> PREFIX rdfs: <http://www.w3.org/2000/01/rdf-schema#> INSERT DATA { :Employee rdfs:subClassOf [ a owl:Restriction ; owl:onProperty :worksFor ; owl:someValuesFrom :Organisation ] . :knows a owl:ReflexiveProperty . :jo a :Employee . :kim :worksFor :acme }"
call PUT /api/v1/repositories/horst '{"title": "Horst repository", "reasoning": "owl-horst"}'
update /api/v1/repositories/horst "PREFIX : <http://e/> PREFIX owl: <http://www.w3.org/2002/07/owl#> INSERT DATA { :partOf a owl:TransitiveProperty . :a :partOf :b . :b :partOf :c . :p owl:inverseOf :q . :a :p :z }"
for id in dotted.1 .staging .trash-kept; do
  call PUT "/api/v1/repositories/$id" "{\"title\": \"Repository $id\"}"
  update "/api/v1/repositories/$id" "INSERT DATA { <http://e/in> <http://e/repository> \"$id\" }"
done

# The access state.
call PUT /api/v1/access/settings '{"reason": "fixture", "enforced": false, "fallback": "allow", "inferred": "visible", "users_create_workspaces": true}'
call PUT /api/v1/access/roles/reader '{"reason": "fixture", "read": ["http://e/g1", "http://e/open/*"], "default_graph": "read", "service": false}'
call PUT /api/v1/access/users/alice '{"reason": "fixture", "roles": ["reader"], "password": "correct horse battery"}'
call PUT /api/v1/access/users/root2 '{"reason": "fixture", "admin": true}'
call PUT "/api/v1/access/users/auth0%7Cu1" '{"reason": "fixture", "roles": ["reader"]}'
call PUT /api/v1/access/workspaces/team '{"reason": "fixture", "title": "The team", "repository": ""}'
call PUT /api/v1/access/workspaces/team/members/alice '{"reason": "fixture", "level": "editor"}'
call PUT /api/v1/queries/team/people '{"query": "SELECT ?x WHERE { ?x a <http://e/Person> }", "title": "People", "description": "Everyone", "repository": "nrese"}'
call PUT "/api/v1/queries/~alice/mine" '{"query": "SELECT * WHERE { ?s ?p ?o } LIMIT 3", "title": "Mine"}'

# An image backup of the default repository.
call POST /ops/api/admin/dataset/image

# Main's answers and settings, before it stops.
while IFS=$'\t' read -r repo name query; do
  path=/api/v1/repositories/$repo/query
  curl -sf "$URL$path" -H "Authorization: Bearer $TOKEN" -H "Accept: text/tab-separated-values" \
    --data-urlencode "query=$query" | { read -r header; echo "$header"; sort; } \
    > "$HERE/expected/$repo-$name.tsv"
done < "$HERE/queries.tsv"
curl -sf "$URL/readyz" > "$HERE/expected/readyz.json"
for path in /api/v1/repositories /api/v1/access /api/v1/queries \
    /api/v1/repositories/nrese /api/v1/repositories/rdfs /api/v1/repositories/rules \
    /api/v1/repositories/nrese/namespaces /api/v1/repositories/rdfs/namespaces \
    /api/v1/queries/team/people "/api/v1/queries/~alice/mine"; do
  file=$(echo "$path" | sed 's#^/api/v1/##; s#[/~%]#_#g')
  curl -sf "$URL$path" -H "Authorization: Bearer $TOKEN" > "$HERE/expected/$file.json"
done

# Stopped hard: the WAL tail stays as written.
kill -9 "$PID"
wait "$PID" 2> /dev/null || true
mv "$HERE/data/backups/"* "$HERE/backup" 2> /dev/null || true
rmdir "$HERE/data/backups" 2> /dev/null || true
rm -rf "$WORK"
echo "fixture written: $(du -sh "$HERE/data" | cut -f1) of data"
