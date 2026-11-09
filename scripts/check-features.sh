#!/usr/bin/env bash
# Fails if a product build has a test-only feature: nrese-engine's `fault-points` (fault points
# for other crates' tests, DUR-4) is turned on only by nrese-store's dev-dependency, and the
# workspace's resolver keeps dev-dependency features out of normal builds. This checks it on the
# server's normal dependency graph, for every target, from the lock file alone (no build).
#
#   scripts/check-features.sh
set -euo pipefail

# A native Windows path under Git Bash (`pwd -W`), as scripts/check-locks.sh.
ROOT="$(cd "$(dirname "$0")/.." && { pwd -W 2>/dev/null || pwd; })"
tree=$(cargo tree --locked --manifest-path "$ROOT/Cargo.toml" -p nrese-server -e normal,features \
  --target all --prefix none)
if grep -q 'fault-points' <<<"$tree"; then
  echo "the server's normal build has nrese-engine's test-only feature fault-points:" >&2
  grep 'fault-points' <<<"$tree" | sort -u >&2
  exit 1
fi
echo "no test-only feature in the server's normal build"
