#!/usr/bin/env bash
# Fails if a product build has a test-only feature: nrese-engine's `fault-points` (fault points
# for other crates' tests, DUR-4) and `witness` (work counters for other crates' tests, REAS-3)
# are turned on only by dev-dependencies, and the workspace's resolver keeps dev-dependency
# features out of normal builds. This checks it on the
# server's normal dependency graph, for every target, from the lock file alone (no build).
#
#   scripts/check-features.sh
set -euo pipefail

# A native Windows path under Git Bash (`pwd -W`), as scripts/check-locks.sh.
ROOT="$(cd "$(dirname "$0")/.." && { pwd -W 2>/dev/null || pwd; })"
tree=$(cargo tree --locked --manifest-path "$ROOT/Cargo.toml" -p nrese-server -e normal,features \
  --target all --prefix none)
if grep -qE 'fault-points|witness' <<<"$tree"; then
  echo "the server's normal build has a test-only feature of nrese-engine (fault-points, witness):" >&2
  grep -E 'fault-points|witness' <<<"$tree" | sort -u >&2
  exit 1
fi
echo "no test-only feature in the server's normal build"
