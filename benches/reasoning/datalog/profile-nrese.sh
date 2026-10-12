#!/usr/bin/env bash
# A CPU profile of NRESE materialising one Datalog case: where the rule evaluation's time
# goes (the attribution of a loss in the kit's comparison). perf samples `nrese-server load`
# as a host process, on a host that allows profiling (hosts.toml: Phuoc-Yu). Times from a
# profiled run are not comparable with the kit's: perf adds its own overhead.
#
#   profile-nrese.sh CASE FACTS OUT        e.g. cspa-httpd ~/…/cspa-httpd/facts.nt "$QUEUE_RESULTS"
#
# NRESE_BIN names the server. OUT gets the program (program.n3), the load log, perf.data,
# self.txt (functions by their own samples) and inclusive.txt (by samples under them).
set -euo pipefail
case=$1
facts=$2
out=$3
here=$(cd "$(dirname "$0")" && pwd)
[ -x "${NRESE_BIN:-}" ] || { echo "set NRESE_BIN to the nrese-server binary" >&2; exit 2; }
mkdir -p "$out"
python3 "$here/datalog.py" translate "$case" "$out/program" > /dev/null
store=$(mktemp -d "${TMPDIR:-/tmp}/nrese-profile-store.XXXXXX")
trap 'rm -rf "$store"' EXIT
NRESE_STORE_MODE=on-disk NRESE_DATA_DIR="$store" NRESE_REASONING_MODE=custom \
  NRESE_REASONING_RULES="$out/program/program.n3" RUST_LOG=info \
  perf record -F 99 -g -o "$out/perf.data" -- "$NRESE_BIN" load "$facts" > "$out/load.log" 2>&1
perf report -i "$out/perf.data" --no-children --sort symbol --stdio --percent-limit 0.5 \
  > "$out/self.txt" 2> /dev/null
perf report -i "$out/perf.data" --children --sort symbol --stdio --percent-limit 2 -g none \
  > "$out/inclusive.txt" 2> /dev/null
grep -E "inferred stack|bulk load complete" "$out/load.log" | sed -e 's/\x1b\[[0-9;]*m//g' | tail -3
