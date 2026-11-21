#!/usr/bin/env bash
# Writes the W3C implementation reports (EARL, one Turtle file per suite) from one run of the
# suite runners on the pinned w3c/rdf-tests, into w3c-reports/ (or NRESE_W3C_REPORTS):
# - sparql11.ttl, sparql12.ttl: crates/nrese-sparql/tests/w3c_sparql11
# - rdf11-*.ttl, rdf12-*.ttl, one per syntax: crates/rdf/nrese-rdf-io/tests/w3c_rdf
# and the SPARQL runner's text reports beside them. Each file has one assertion per test of its
# manifests, a test the runner doesn't run reported untested with its reason, and names the
# commit it ran and the suite's commit; nothing in it is written by hand.
#
#     bash scripts/w3c-reports.sh        from a clean clone: the suites are fetched first
#
# A tree with local changes is refused (a report names the commit it ran), as is an output
# folder that isn't empty (a report from another run would stand beside this one's).
set -euo pipefail
cd "$(dirname "$0")/.."

if [ -n "$(git status --porcelain --untracked-files=no)" ]; then
  echo "w3c-reports: the tree has local changes; a report names the commit it ran" >&2
  exit 1
fi
out=${NRESE_W3C_REPORTS:-$PWD/w3c-reports}
if [ -n "$(ls -A "$out" 2> /dev/null)" ]; then
  echo "w3c-reports: $out isn't empty; move its reports away first" >&2
  exit 1
fi
mkdir -p "$out"

bash scripts/fetch-w3c-tests.sh
export NRESE_W3C_REQUIRED=1 NRESE_W3C_REPORTS="$out"
cargo test --locked -p nrese-sparql --test w3c_sparql11 -- --nocapture
cargo test --locked -p nrese-rdf-io --test w3c_rdf -- --nocapture
cp "${CARGO_TARGET_DIR:-target}"/tmp/w3c-sparql11-report.txt \
  "${CARGO_TARGET_DIR:-target}"/tmp/w3c-sparql12-report.txt "$out/"
echo "the reports, from commit $(git rev-parse HEAD), are in $out:"
ls "$out"
