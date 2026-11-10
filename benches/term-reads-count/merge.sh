#!/usr/bin/env bash
# Fills perf's instruction count into each instruction run's JSON line (see the crate's docs,
# src/lib.rs). DIR holds one pair of files per run, named by the run's key,
# <side>-<shape>-<op>-t<threads>-r<run>:
#   KEY.json  the runner's stdout: its one line, with "instructions_u":null;
#   KEY.perf  perf's stderr: its CSV (perf stat -x,) and the runner's own stderr.
# Prints the merged lines in key order and leaves both files as they are. Refuses (exit 1) a
# line without its CSV, a CSV without its line, a line whose fields aren't its key's, and a
# CSV without a positive instructions count; every refusal is printed, then it exits.
#
#   bash merge.sh DIR > merged.jsonl
set -euo pipefail
dir=${1:?usage: merge.sh DIR}
refused=0
refuse() {
  echo "merge.sh: $1" >&2
  refused=1
}
shopt -s nullglob
for perf in "$dir"/*.perf; do
  [ -e "${perf%.perf}.json" ] || refuse "$(basename "$perf"): a CSV without its line"
done
for json in "$dir"/*.json; do
  key=$(basename "$json" .json)
  perf=${json%.json}.perf
  if [ ! -e "$perf" ]; then
    refuse "$key: a line without its CSV"
    continue
  fi
  if ! [[ $key =~ ^([A-Za-z0-9._]+)-(memory|reopened)-([a-z_]+)-t([0-9]+)-r([0-9]+)$ ]]; then
    refuse "$key: not named <side>-<shape>-<op>-t<threads>-r<run>"
    continue
  fi
  side=${BASH_REMATCH[1]} shape=${BASH_REMATCH[2]} op=${BASH_REMATCH[3]} threads=${BASH_REMATCH[4]}
  mapfile -t lines < "$json"
  if [ "${#lines[@]}" -ne 1 ]; then
    refuse "$key: ${#lines[@]} lines, not one"
    continue
  fi
  line=${lines[0]}
  if [[ $line != *"\"shape\":\"$shape\",\"op\":\"$op\",\"threads\":$threads,\"side\":\"$side\","* ]]; then
    refuse "$key: its line is another run's: $line"
    continue
  fi
  old='"instructions_u":null'
  if [[ $line != *"$old"* ]]; then
    refuse "$key: its line has no instructions_u to fill"
    continue
  fi
  # count,unit,event,...: a CPU with two core types reports a line for each.
  count=$(awk -F, '$3 ~ /instructions/ && $1 ~ /^[0-9]+$/ { n += $1 } END { if (n > 0) print n }' "$perf")
  if [ -z "$count" ]; then
    refuse "$key: its CSV has no positive instructions count"
    continue
  fi
  new="\"instructions_u\":$count"
  echo "${line/"$old"/"$new"}"
done
exit "$refused"
