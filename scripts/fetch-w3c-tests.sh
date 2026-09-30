#!/usr/bin/env bash
# Fetches the W3C test suites at pinned commits into .cache, where the suite runners look
# for them:
# - RDF/SPARQL (w3c/rdf-tests) into .cache/rdf-tests (override: NRESE_W3C_TESTS)
# - SHACL (w3c/data-shapes) into .cache/data-shapes (the runner's override,
#   NRESE_W3C_SHACL_TESTS, names the suite's `tests` directory)
set -euo pipefail

CACHE="$(dirname "$0")/../.cache"

# fetch <repository> <commit> <target directory> <sparse path>
fetch() {
  local repository=$1 commit=$2 target=$3 path=$4
  if [ -d "$target/.git" ] && [ "$(git -C "$target" rev-parse HEAD)" = "$commit" ]; then
    echo "$repository already at $commit in $target"
    return
  fi
  rm -rf "$target"
  git init -q "$target"
  git -C "$target" remote add origin "https://github.com/$repository.git"
  git -C "$target" sparse-checkout set "$path"
  git -C "$target" fetch -q --depth 1 --filter=blob:none origin "$commit"
  git -C "$target" checkout -q FETCH_HEAD
  echo "$repository $commit in $target"
}

fetch w3c/rdf-tests 369a90d1a60c021b746df2e411da0ff36258a758 \
  "${NRESE_W3C_TESTS:-$CACHE/rdf-tests}" sparql/sparql11
fetch w3c/data-shapes d556a1fc90c5d5ffdf388124cbb687cf43c5a35e \
  "$CACHE/data-shapes" data-shapes-test-suite/tests

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
