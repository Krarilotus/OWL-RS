#!/usr/bin/env bash
# Ordinary push producer/consumer. No local part or SSH fallback, including exit 75.
set -euo pipefail
export PYTHONDONTWRITEBYTECODE=1
cd "$(git rev-parse --show-toplevel)"
if type -P python >/dev/null; then exec python scripts/lib/gate-client.py "$@"; fi
exec python3 scripts/lib/gate-client.py "$@"
