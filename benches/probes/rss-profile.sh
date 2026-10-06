#!/usr/bin/env bash
# Resident memory over time of a perf lab load: rss-profile.sh DATA [perf_lab args...]
#
# Usage: benches/probes/rss-profile.sh DATA [perf_lab args...]   (Linux; LAB=<perf_lab binary>, default the release example)
set -u
source "$(dirname "$0")/common.sh"
cd "$SCRATCH"
LAB=${LAB:-$REPO/target/release/examples/perf_lab}
data=$1; shift
store=$SCRATCH/store-prof
rm -rf "$store"
start=$(date +%s.%N)
RUST_LOG=info "$LAB" --store "$store" --load "$data" --runs 1 --label prof "$@" > prof.log 2>&1 &
pid=$!
: > rss.log
while kill -0 "$pid" 2>/dev/null; do
  now=$(date +%s.%N)
  rss=$(awk '/VmRSS/ {print int($2/1024)}' /proc/$pid/status 2>/dev/null)
  echo "$(echo "$now - $start" | bc | cut -c1-5) ${rss:-0}" >> rss.log
  sleep 0.2
done
wait "$pid"
rm -rf "$store"
awk 'NR % 3 == 1' rss.log | tr '\n' ' ' | fold -w 200
echo
grep -vE "^\s*$" prof.log | cut -c1-220 | head -30
