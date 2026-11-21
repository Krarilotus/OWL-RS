#!/usr/bin/env bash
# Writes the W3C implementation reports (EARL, one Turtle file per suite) from one run of the
# suite runners on the pinned w3c/rdf-tests, into w3c-reports/ (or NRESE_W3C_REPORTS):
# - sparql11.ttl, sparql12.ttl: crates/nrese-sparql/tests/w3c_sparql11
# - rdf12-*.ttl, one per RDF 1.2 syntax: crates/rdf/nrese-rdf-io/tests/w3c_rdf
# and the SPARQL runner's text reports beside them. Each file has one assertion per test of its
# manifests, a test the runner doesn't run reported untested with its reason, and names the
# commit it ran and the suite's commit; nothing in it is written by hand.
#
#     bash scripts/w3c-reports.sh        from a clean clone: the suites are fetched first
#
# Refused: a tree with changes or untracked files (a report names the commit it ran), an output
# folder that isn't empty (a report from another run would stand beside this one's), and a run
# whose reports don't all name the suite commit scripts/fetch-w3c-tests.sh pins.
set -euo pipefail
cd "$(dirname "$0")/.."

if [ -n "$(git status --porcelain)" ]; then
  echo "w3c-reports: the tree has changes or untracked files; a report names the commit it ran" >&2
  exit 1
fi
out=${NRESE_W3C_REPORTS:-$PWD/w3c-reports}
if [ -n "$(ls -A "$out" 2> /dev/null)" ]; then
  echo "w3c-reports: $out isn't empty; move its reports away first" >&2
  exit 1
fi
mkdir -p "$out"
# Absolute: cargo runs each crate's tests in that crate's directory.
out=$(cd "$out" && pwd)
pin=$(sed -n 's|^fetch w3c/rdf-tests \([0-9a-f]\{40\}\).*|\1|p' scripts/fetch-w3c-tests.sh)
[ -n "$pin" ] || { echo "w3c-reports: no rdf-tests pin in scripts/fetch-w3c-tests.sh" >&2; exit 1; }

# The suites in this checkout's .cache, which the fetch owns, never a folder named from outside.
unset NRESE_W3C_TESTS
bash scripts/fetch-w3c-tests.sh
export NRESE_W3C_REQUIRED=1 NRESE_W3C_REPORTS="$out"
cargo test --locked -p nrese-sparql --test w3c_sparql11 -- --nocapture
cargo test --locked -p nrese-rdf-io --test w3c_rdf -- --nocapture
cp "${CARGO_TARGET_DIR:-target}"/tmp/w3c-sparql11-report.txt \
  "${CARGO_TARGET_DIR:-target}"/tmp/w3c-sparql12-report.txt "$out/"

unpinned=$(grep -L "against w3c/rdf-tests $pin:" "$out"/*.ttl || true)
if [ -n "$unpinned" ]; then
  echo "w3c-reports: these reports don't name the pinned suite commit $pin:" >&2
  echo "$unpinned" >&2
  exit 1
fi
echo "the reports, from commit $(git rev-parse HEAD) against w3c/rdf-tests $pin, are in $out:"
ls "$out"
