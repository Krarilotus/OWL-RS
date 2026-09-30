#!/usr/bin/env bash
# The Jena oracle (README.md) on this machine: dump the differential tests' cases, let Jena
# answer them, compare, write the report. The dump is removed afterwards; the report stays.
#
#   benches/oracle/run.sh [report.md]      (default: benches/oracle/results/<date>.md)
set -euo pipefail
export MSYS_NO_PATHCONV=1
ROOT=$(cd "$(dirname "$0")/../.." && pwd)
native() { cygpath -m "$1" 2>/dev/null || echo "$1"; }
REPORT=${1:-$ROOT/benches/oracle/results/$(date +%Y-%m-%d).md}
DUMP=$ROOT/tmp/oracle-dump-$$
trap 'rm -rf "$DUMP"' EXIT
mkdir -p "$(dirname "$REPORT")"

cd "$ROOT"
NRESE_ORACLE_DUMP=$(native "$DUMP") scripts/cargo-guarded.sh test --locked -p nrese-sparql \
  --test native_differential_tests -- native_results_equal_spareval_on_random_queries \
  pushed_filters_equal_spareval computed_values_equal_spareval
docker image inspect nrese-bench/jena:6.2.0 >/dev/null 2>&1 ||
  docker build -q -t nrese-bench/jena:6.2.0 benches/competitors/jena >/dev/null
docker build -q -t nrese-bench/jena-oracle benches/oracle/jena >/dev/null
docker run --rm -v "$(native "$DUMP"):/cases" nrese-bench/jena-oracle /cases
python benches/oracle/compare.py "$DUMP" --report "$REPORT"
