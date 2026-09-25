#!/usr/bin/env bash
# Fetches the W3C RDF/SPARQL test suites (w3c/rdf-tests) at a pinned commit into
# .cache/rdf-tests, where the W3C suite runners look for them (override: NRESE_W3C_TESTS).
set -euo pipefail

COMMIT=369a90d1a60c021b746df2e411da0ff36258a758
TARGET="${NRESE_W3C_TESTS:-$(dirname "$0")/../.cache/rdf-tests}"

if [ -d "$TARGET/.git" ] && [ "$(git -C "$TARGET" rev-parse HEAD)" = "$COMMIT" ]; then
  echo "rdf-tests already at $COMMIT in $TARGET"
  exit 0
fi
rm -rf "$TARGET"
git init -q "$TARGET"
git -C "$TARGET" remote add origin https://github.com/w3c/rdf-tests.git
git -C "$TARGET" sparse-checkout set sparql/sparql11
git -C "$TARGET" fetch -q --depth 1 --filter=blob:none origin "$COMMIT"
git -C "$TARGET" checkout -q FETCH_HEAD
echo "rdf-tests $COMMIT in $TARGET"
