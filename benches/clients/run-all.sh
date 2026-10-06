#!/usr/bin/env bash
# Every client kit against NRESE, end to end (the connectors' contract):
#   rdf4j    RDF4J's Java client (HTTPRepository, RemoteRepositoryManager): GraphDB
#            tooling, ResearchSpace and other RDF4J applications
#   jena     Apache Jena's RDFConnection, result formats, the Graph Store Protocol
#   rdflib   rdflib's SPARQLUpdateStore and Dataset, the Graph Store Protocol
#   dmw      the Datamodel Workflow's export, questions, updates and validation
#            (scripts/smoke-dmw-export.sh)
#
#   benches/clients/run-all.sh [path/to/nrese-server] [kit...]
#
# Each kit gets a fresh in-memory server of its own. Needs Java 21+ (RDF4J, Jena; Maven is
# downloaded to TOOLS if missing), Python 3 with rdflib, curl. Prints a summary; exits 1 if
# a kit failed.
set -uo pipefail
here=$(cd "$(dirname "$0")" && pwd)
root=$(cd "$here/../.." && pwd)
server=${1:-$root/target/release/nrese-server}
shift || true
kits=("$@")
[ ${#kits[@]} -eq 0 ] && kits=(rdf4j jena rdflib dmw)
tools=${TOOLS:-$HOME/.cache/nrese-tools}
port=${PORT:-18093}
base="http://127.0.0.1:$port"

maven() {
  if ! command -v mvn >/dev/null; then
    local version=3.9.16
    if [ ! -x "$tools/apache-maven-$version/bin/mvn" ]; then
      mkdir -p "$tools"
      curl -sSL "https://repo1.maven.org/maven2/org/apache/maven/apache-maven/$version/apache-maven-$version-bin.tar.gz" \
        | tar -xz -C "$tools"
    fi
    PATH="$tools/apache-maven-$version/bin:$PATH"
  fi
  (cd "$1" && { [ -d lib ] || mvn -q dependency:copy-dependencies -DoutputDirectory=lib; })
}

pid=
start() {   # extra environment as arguments
  env NRESE_BIND_ADDR=127.0.0.1:$port NRESE_STORE_MODE=in-memory RUST_LOG=warn "$@" "$server" &
  pid=$!
  for _ in $(seq 1 150); do curl -sf "$base/readyz" >/dev/null && return 0; sleep 0.2; done
  echo "the server didn't start"; return 1
}
stop() { [ -n "$pid" ] && kill "$pid" 2>/dev/null; wait "$pid" 2>/dev/null; pid=; }
trap stop EXIT

declare -A result
for kit in "${kits[@]}"; do
  echo "== $kit"
  case $kit in
    rdf4j)
      # Its own script runs the store's default graph and the union.
      if PORT=$port "$here/rdf4j/run.sh" "$server"; then result[$kit]=pass; else result[$kit]=FAIL; fi
      continue ;;
    jena)
      maven "$here/jena"
      start && (cd "$here/jena" && java -cp "lib/*" JenaClientTest.java "$base") ;;
    rdflib)
      start && python "$here/rdflib/rdflib_client_test.py" "$base" ;;
    dmw)
      start NRESE_DEFAULT_GRAPH=union && "$root/scripts/smoke-dmw-export.sh" "$base" ;;
    *)
      echo "unknown kit $kit"; false ;;
  esac
  if [ $? -eq 0 ]; then result[$kit]=pass; else result[$kit]=FAIL; fi
  stop
done
echo
failed=0
for kit in "${kits[@]}"; do
  printf '%-8s %s\n' "$kit" "${result[$kit]:-FAIL}"
  [ "${result[$kit]:-FAIL}" = pass ] || failed=1
done
exit $failed
