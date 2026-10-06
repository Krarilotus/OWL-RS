#!/usr/bin/env bash
# The synthetic entity dataset of N triples (`entities-N`, the basics mix's generated
# tier) into the dataset volume: the harness's deterministic generator (persons with a
# type, a name, an age and a friend), written on the host and copied into
# nrese-bench-data. The same N always gives the same file (benches/datasets.toml freezes
# its hash).
#
#   benches/competitors/prepare-entities.sh 10000000
set -euo pipefail
REPO=$(cd "$(dirname "$0")/../.." && pwd)
triples=$1
name="entities-$triples"
scratch="$REPO/tmp/bench-run-entities"
mkdir -p "$scratch"
harness="${HARNESS:-$REPO/target/release/nrese-bench-harness}"
[ -x "$harness" ] || [ -x "$harness.exe" ] || \
  bash "$REPO/scripts/cargo-guarded.sh" build --release --locked --quiet \
    --manifest-path "$REPO/benches/nrese-bench-harness/Cargo.toml"
"$harness" generate --triples "$triples" --out "$scratch/$name.nt"
host=$(cd "$scratch" && pwd -W 2>/dev/null || pwd)
MSYS_NO_PATHCONV=1 docker run --rm -v nrese-bench-data:/data -v "$host:/in:ro" alpine \
  sh -c "cp /in/$name.nt /data/$name.nt.part && mv /data/$name.nt.part /data/$name.nt"
rm -rf "$scratch"
echo "$name: $triples triples into nrese-bench-data"
