#!/usr/bin/env bash
# Fetches the W3C test suites at pinned commits into .cache, where the suite runners look
# for them:
# - RDF/SPARQL (w3c/rdf-tests) into .cache/rdf-tests (override: NRESE_W3C_TESTS): the
#   SPARQL 1.1 and 1.2 suites, and the RDF 1.1 and 1.2 syntax suites (N-Triples, N-Quads, Turtle,
#   TriG, RDF/XML) for nrese-rdf-io
# - JSON-LD 1.1 (w3c/json-ld-api) into .cache/json-ld-api (override: NRESE_W3C_JSONLD_TESTS)
# - SHACL (w3c/data-shapes) into .cache/data-shapes (the runner's override,
#   NRESE_W3C_SHACL_TESTS, names the suite's `tests` directory)
# - Notation3 (w3c/N3) into .cache/n3
# - RDF Dataset Canonicalization (w3c/rdf-canon) into .cache/rdf-canon
set -euo pipefail

CACHE="$(dirname "$0")/../.cache"

# fetch <repository> <commit> <target directory> <sparse path>...
fetch() {
  local repository=$1 commit=$2 target=$3
  shift 3
  if [ -d "$target/.git" ] && [ "$(git -C "$target" rev-parse HEAD)" = "$commit" ]; then
    # Already there: make sure every path is checked out (missing blobs are fetched), byte
    # for byte (a checkout made with core.autocrlf rewrites line ends inside test data).
    git -C "$target" config core.autocrlf false
    git -C "$target" sparse-checkout set "$@"
    # Rewrite the sparse paths from local objects only (a partial clone fetches what it
    # lacks over the network for anything wider).
    local path
    for path in "$@"; do
      rm -rf "${target:?}/$path"
    done
    git -C "$target" checkout -q -- "$@"
    echo "$repository already at $commit in $target"
    return
  fi
  rm -rf "$target"
  git init -q "$target"
  git -C "$target" remote add origin "https://github.com/$repository.git"
  git -C "$target" config core.autocrlf false
  git -C "$target" sparse-checkout set "$@"
  git -C "$target" fetch -q --depth 1 --filter=blob:none origin "$commit"
  git -C "$target" checkout -q FETCH_HEAD
  echo "$repository $commit in $target"
}

fetch w3c/rdf-tests 369a90d1a60c021b746df2e411da0ff36258a758 \
  "${NRESE_W3C_TESTS:-$CACHE/rdf-tests}" sparql/sparql11 sparql/sparql12 rdf/rdf11 rdf/rdf12
fetch w3c/json-ld-api ffdb326121ea89b7b8280e76a5caea923834bcef \
  "${NRESE_W3C_JSONLD_TESTS:-$CACHE/json-ld-api}" tests
fetch w3c/data-shapes d556a1fc90c5d5ffdf388124cbb687cf43c5a35e \
  "$CACHE/data-shapes" data-shapes-test-suite/tests
# Notation3 (W3C N3 Community Group, w3c/N3) into .cache/n3: the parser and reasoner
# manifests the N3 runners of nrese-rdf-io and nrese-reasoner read.
fetch w3c/N3 1d29dea774d3e5a97883ebbc8ab033a176ac11b5 "$CACHE/n3" tests
# RDFC-1.0 (W3C Recommendation): the tests nrese-rdf's canonicaliser runs.
fetch w3c/rdf-canon 15619df2fda7a4ca88308733789b6774517f9638 "$CACHE/rdf-canon" tests

# The OWL 2 test cases (approved by the OWL Working Group; one RDF/XML file, pinned by its
# checksum): crates/nrese-store/tests/w3c_owl2_rl runs the OWL 2 RL ones.
OWL_TESTS="$CACHE/owl-test/all.rdf"
OWL_TESTS_SHA256=5383f1ddf4cf2f03703a2f886f41d4e5bc375633a1cfa94a03254fd89330f8bb
if [ -f "$OWL_TESTS" ] && echo "$OWL_TESTS_SHA256  $OWL_TESTS" | sha256sum -c --quiet - 2>/dev/null; then
  echo "OWL 2 test cases already in $OWL_TESTS"
else
  mkdir -p "$(dirname "$OWL_TESTS")"
  curl -sSfL -o "$OWL_TESTS.partial" https://www.w3.org/2009/11/owl-test/all.rdf
  echo "$OWL_TESTS_SHA256  $OWL_TESTS.partial" | sha256sum -c --quiet -
  mv "$OWL_TESTS.partial" "$OWL_TESTS"
  echo "OWL 2 test cases in $OWL_TESTS"
fi

# The GeoSPARQL compliance benchmark's dataset, queries and answers (OpenLink Software,
# GPL-2.0: fetched, never copied into this repository):
# crates/nrese-sparql/tests/geosparql_compliance runs them.
fetch OpenLinkSoftware/GeoSPARQLBenchmark 879e0746e74c1327f74e57366104bcf1a6d63fb1 \
  "$CACHE/geosparql-benchmark" src/main/resources
