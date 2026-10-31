#!/usr/bin/env bash
# M1, the hop micro-costs (pg_hops m1): per variant and run count, one process started after the
# page cache is emptied (every phase once: a cold process, not each phase's cold cost, since the
# step inputs are read before the phases and the phases warm one another), one warm process (every phase
# --reps times, in a seeded order), then `perf stat` per phase against the `none` baseline
# (the same process without a phase: open, updates, inputs). Work counters are exact and the
# same in every process; times and hardware counters are this host's. Not an LDBC benchmark
# result.
#
#   bash benches/property-graphs/pg-m1.sh SF [a] [b]     # results to $FLEET_OUT
# Environment: SEED (1), REPS (5), PERF_REPS (3), COLD_CACHE (the host's command; empty: no
# cold process, said so in the results); MODE=counters: the work counters and answers only
# (one process per store, every phase once, no cold process, no perf): a job that times nothing.
set -euo pipefail
A=${SNB_ASSETS:-$HOME/nrese-assets/ldbc-snb-20261030}
here=$(cd "$(dirname "$0")" && pwd)
root=$(cd "$here/../.." && pwd)
sf=$1; shift
variants=${*:-a b}
out=$FLEET_OUT
seed=${SEED:-1}
reps=${REPS:-5}
perf_reps=${PERF_REPS:-3}
cold=${COLD_CACHE-sudo -n /usr/local/sbin/owlrs-drop-caches}
counters=false
if [ "${MODE:-}" = counters ]; then counters=true; reps=1; cold=""; fi
export CARGO_TARGET_DIR=${CARGO_TARGET_DIR:-$PWD/target-job}
events=task-clock,cycles,instructions,cache-references,cache-misses,L1-dcache-load-misses,branch-misses,dTLB-load-misses
phases=(none a.out.scan.random a.out.cursor.random a.out.cursor.asc a.in.scan.random a.in.cursor.random
  a.in.cursor.asc b.fwd.scan b.fwd.cursor.batched b.bwd.scan b.bwd.cursor.batched b.step.hash
  b.step.reifier.scan b.step.reifier.cursor.sorted b.step.reifier.cursor.found b.step.value.scan
  b.step.value.cursor.sorted b.step.value.cursor.found b.step.decode c.bfs.scan c.bfs.cursor.sorted)
{
  echo "host $(hostname) $(uname -r) $(nproc) cpus"
  grep -m1 'model name' /proc/cpuinfo || true
  cat /sys/devices/system/cpu/cpu0/cpufreq/scaling_governor 2>/dev/null || true
  rustc -V; cargo -V; perf --version || true
  echo "rev $(git -C "$root" rev-parse HEAD 2>/dev/null || echo unknown)"
  echo "dirty $(git -C "$root" status --porcelain 2>/dev/null | wc -l)"
  echo "RUSTFLAGS=${RUSTFLAGS:-} profile=release seed=$seed reps=$reps perf_reps=$perf_reps"
  echo "events $events"
} > "$out/env.txt"
cargo build --release --locked -p nrese-engine --example pg_hops > "$out/build.log" 2>&1
bin=$CARGO_TARGET_DIR/release/examples/pg_hops
sha256sum "$bin" >> "$out/env.txt"
d=$A/rdf/sf$sf
(cd "$d" && sha256sum --check SHA256SUMS) > "$out/inputs-sf$sf.txt"
cp "$d/SHA256SUMS" "$out/SHA256SUMS-sf$sf"
for v in $variants; do
  base=$PWD/store-sf$sf-$v
  rm -rf "$base" "$base-r4"
  "$bin" load "$d/snb-$v.nt" "$base" --load-memory-gb "${LOAD_GB:-8}" > "$out/load-sf$sf-$v.json"
  # Under updates: a copy, its three commits left in the log (m1 never writes a store).
  cp -a "$base" "$base-r4"
  "$bin" update "$base-r4" --seed "$seed" > "$out/update-sf$sf-$v.json"
  for runs in 1 4; do
    tag=sf$sf-$v-r$runs
    store=$base
    [ "$runs" = 4 ] && store=$base-r4
    if $counters; then
      "$bin" m1 "$store" --runs "$runs" --seed "$seed" --reps 1 > "$out/m1-$tag-counters.json"
      continue
    fi
    if [ -n "$cold" ] && $cold; then
      "$bin" m1 "$store" --runs "$runs" --seed "$seed" --reps 1 > "$out/m1-$tag-cold.json"
    else
      echo "no cold process: the page cache could not be emptied ($cold)" > "$out/m1-$tag-cold.txt"
    fi
    "$bin" m1 "$store" --runs "$runs" --seed "$seed" --reps "$reps" > "$out/m1-$tag-warm.json"
    for p in "${phases[@]}"; do
      perf stat -x, -e "$events" -o "$out/perf-$tag-$p.csv" \
        "$bin" m1 "$store" --runs "$runs" --seed "$seed" --reps "$perf_reps" --only "$p" > /dev/null
    done
  done
  rm -rf "$base" "$base-r4"
done
