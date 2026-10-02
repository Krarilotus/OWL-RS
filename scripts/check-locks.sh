#!/usr/bin/env bash
# Checks that every Cargo workspace of the repository has a current lock file: the main
# workspace, the fuzz targets' (fuzz/) and the separate ones under benches/ (which depend
# on workspace crates, so a new dependency there changes their locks too). CI builds them
# all with --locked; this finds a stale lock in seconds, before a push.
#
# Offline first (the local registry cache answers it); online only if that can't. A lock
# counts as stale only when cargo says it needs updating: any other failure (a network
# hiccup while refreshing the index) is reported as what it is, not as a stale lock.
#
#   scripts/check-locks.sh          check
#   scripts/check-locks.sh --fix    update the stale ones (only what changed)
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
failed=0
for manifest in "$ROOT/Cargo.toml" "$ROOT"/benches/*/Cargo.toml "$ROOT/fuzz/Cargo.toml"; do
  grep -q '^\[workspace\]' "$manifest" 2>/dev/null || [ "$manifest" = "$ROOT/Cargo.toml" ] || continue
  name=${manifest#"$ROOT"/}
  if cargo metadata --locked --offline --format-version 1 --manifest-path "$manifest" >/dev/null 2>&1; then
    continue
  fi
  if error=$(cargo metadata --locked --format-version 1 --manifest-path "$manifest" 2>&1 >/dev/null); then
    continue
  fi
  if grep -qE "cannot update the lock file|needs to be updated" <<<"$error"; then
    if [ "${1:-}" = "--fix" ]; then
      cargo metadata --format-version 1 --manifest-path "$manifest" >/dev/null
      echo "updated: $name"
    else
      echo "stale lock file: $name (scripts/check-locks.sh --fix)" >&2
      failed=1
    fi
  else
    echo "cargo metadata failed for $name (not a stale lock):" >&2
    sed 's/^/  /' <<<"$error" | head -5 >&2
    failed=1
  fi
done
exit $failed
