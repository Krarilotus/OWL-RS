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
