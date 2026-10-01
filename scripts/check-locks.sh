#!/usr/bin/env bash
# Checks that every Cargo workspace of the repository has a current lock file: the main
# workspace and the separate ones under benches/ (which depend on workspace crates, so a
# new dependency there changes their locks too). CI builds them all with --locked; this
# finds a stale lock in seconds, before a push.
#
#   scripts/check-locks.sh          check
#   scripts/check-locks.sh --fix    update the stale ones (only what changed)
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
stale=0
for manifest in "$ROOT/Cargo.toml" "$ROOT"/benches/*/Cargo.toml; do
  grep -q '^\[workspace\]' "$manifest" 2>/dev/null || [ "$manifest" = "$ROOT/Cargo.toml" ] || continue
  if cargo metadata --locked --format-version 1 --manifest-path "$manifest" >/dev/null 2>&1; then
    continue
  fi
  if [ "${1:-}" = "--fix" ]; then
    cargo metadata --format-version 1 --manifest-path "$manifest" >/dev/null
    echo "updated: ${manifest#"$ROOT"/}"
  else
    echo "stale lock file: ${manifest#"$ROOT"/} (scripts/check-locks.sh --fix)" >&2
    stale=1
  fi
done
exit $stale
