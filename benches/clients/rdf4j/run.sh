#!/usr/bin/env bash
# RDF4J's Java client against NRESE: starts an in-memory server, fetches RDF4J's client
# libraries with Maven (a Maven of its own is downloaded to TOOLS if `mvn` is missing) and
# runs Rdf4jClientTest.java. Needs Java 21.
#
#   benches/clients/rdf4j/run.sh [path/to/nrese-server]
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
server=${1:-target/release/nrese-server}
tools=${TOOLS:-$HOME/.cache/nrese-tools}
port=${PORT:-18091}
if ! command -v mvn >/dev/null; then
  version=3.9.16
  if [ ! -x "$tools/apache-maven-$version/bin/mvn" ]; then
    mkdir -p "$tools"
    curl -sSL "https://repo1.maven.org/maven2/org/apache/maven/apache-maven/$version/apache-maven-$version-bin.tar.gz" \
      | tar -xz -C "$tools"
  fi
  PATH="$tools/apache-maven-$version/bin:$PATH"
fi
(cd "$here" && [ -d lib ] || mvn -q dependency:copy-dependencies -DoutputDirectory=lib)
# Twice: the store's own default graph, and the union of all graphs (RDF4J's semantics).
for default_graph in default union; do
  NRESE_BIND_ADDR=127.0.0.1:$port NRESE_STORE_MODE=in-memory NRESE_DEFAULT_GRAPH=$default_graph     RUST_LOG=warn "$server" &
  pid=$!
  until curl -sf "http://127.0.0.1:$port/readyz" >/dev/null; do sleep 0.2; done
  echo "== default graph: $default_graph"
  (cd "$here" && java -cp "lib/*" Rdf4jClientTest.java "http://127.0.0.1:$port" $default_graph) || { kill $pid; exit 1; }
  kill $pid; wait $pid 2>/dev/null || true
done
