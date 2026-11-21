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
# whose reports aren't all there, each naming this checkout's commit and the suite commit
# scripts/fetch-w3c-tests.sh pins.
set -euo pipefail
cd "$(dirname "$0")/.."

# Its own assignment, so a failing git stops the script (set -e) instead of reading as clean.
changes=$(git status --porcelain --untracked-files=normal)
if [ -n "$changes" ]; then
  echo "w3c-reports: the tree has changes or untracked files; a report names the commit it ran" >&2
  exit 1
fi
head=$(git rev-parse HEAD)
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

# The suites in this checkout's .cache, which the fetch owns, never a folder named from outside
# (the fetch replaces its targets).
unset NRESE_W3C_TESTS NRESE_W3C_JSONLD_TESTS
bash scripts/fetch-w3c-tests.sh
export NRESE_W3C_REQUIRED=1 NRESE_W3C_REPORTS="$out"
cargo test --locked -p nrese-sparql --test w3c_sparql11 -- --nocapture
cargo test --locked -p nrese-rdf-io --test w3c_rdf -- --nocapture
cp "${CARGO_TARGET_DIR:-target}"/tmp/w3c-sparql11-report.txt \
  "${CARGO_TARGET_DIR:-target}"/tmp/w3c-sparql12-report.txt "$out/"

for name in sparql11 sparql12 rdf12-n-triples rdf12-n-quads rdf12-turtle rdf12-trig rdf12-xml; do
  if ! grep -q "from one run of commit $head against w3c/rdf-tests $pin:" "$out/$name.ttl"; then
    echo "w3c-reports: $out/$name.ttl is missing or doesn't name commit $head and suite commit $pin" >&2
    exit 1
  fi
done
echo "the reports, from commit $head against w3c/rdf-tests $pin, are in $out:"
ls "$out"
