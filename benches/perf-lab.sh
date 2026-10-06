#!/usr/bin/env bash
# Perf lab in Docker: builds crates/nrese-store/examples/perf_lab/ with the pinned
# toolchain and runs it on Linux against a dataset in the volume `nrese-bench-data`.
#
#   benches/perf-lab.sh <dataset> [perf_lab args...]
#   LABEL=lto benches/perf-lab.sh dbpedia-core --baseline benches/baselines/perf-lab/<file>.json
#
# Datasets and their query sets:
#   lubm-<N>                           /data/univ-bench.nt + /data/lubm-<N>.nt, benches/reasoning/queries/lubm
#   owl2bench-<p>-<N>                  /data/owl2bench-<p>-<N>.nt, benches/reasoning/queries/owl2bench
#   olympics, yago-tiny, dbpedia-core, wikidata-lexemes-*, entities-<N>
#                                      /data/<dataset>.nt, benches/competitors/queries/<family>
#
# Environment:
#   LABEL      names the build flavour and the report (default: the git commit)
#   RUSTFLAGS  passed to the build, e.g. "-C target-cpu=native"; each LABEL has its own
#              target directory, so flavours don't rebuild each other
#   CPUS       limits the container's CPUs (default: all)
#   RUST_IMAGE, RUSTUP_TOOLCHAIN, CARGO_PROFILE_RELEASE_LTO, CARGO_PROFILE_RELEASE_CODEGEN_UNITS
#              override the compiler and release profile, to measure one change at a time.
#              RUSTFLAGS="--cfg system_alloc" builds with the system allocator instead of mimalloc.
#
# The JSON report goes to benches/baselines/perf-lab/<date>-<label>-<dataset>.json. These are
# NRESE-only numbers, so they're committed as the performance history.
set -euo pipefail
export MSYS_NO_PATHCONV=1
native() { cygpath -m "$1" 2>/dev/null || echo "$1"; }
ROOT=$(native "$(cd "$(dirname "$0")/.." && pwd)")
DATASET=${1:?usage: $0 <dataset> [perf_lab args...]}
shift
LABEL=${LABEL:-$(git -C "$ROOT" rev-parse --short HEAD)}
RUST_IMAGE=${RUST_IMAGE:-rust:$(sed -n 's/^channel = "\(.*\)"/\1/p' "$ROOT/rust-toolchain.toml")-bookworm}
OUT=$ROOT/benches/baselines/perf-lab
mkdir -p "$OUT"
REPORT=$(date +%Y-%m-%d)-$LABEL-$DATASET.json

case $DATASET in
  lubm-*) inputs="--load /data/univ-bench.nt --load /data/$DATASET.nt"; queries=/src/benches/reasoning/queries/lubm ;;
  owl2bench-*) inputs="--load /data/$DATASET.nt"; queries=/src/benches/reasoning/queries/owl2bench ;;
  entities-*) inputs="--load /data/$DATASET.nt"; queries=/src/benches/competitors/queries/entities ;;
  *) inputs="--load /data/$DATASET.nt"; queries=/src/benches/competitors/queries/${DATASET%%-[0-9]*} ;;
esac

docker volume create nrese-target >/dev/null
docker run --rm -v "$ROOT:/src:ro" -v nrese-target:/target -v nrese-cargo:/usr/local/cargo/registry \
  -e RUSTFLAGS="${RUSTFLAGS:-}" ${RUSTUP_TOOLCHAIN:+-e RUSTUP_TOOLCHAIN=$RUSTUP_TOOLCHAIN} \
  -e CARGO_PROFILE_RELEASE_LTO -e CARGO_PROFILE_RELEASE_CODEGEN_UNITS -w /src "$RUST_IMAGE" \
  cargo build --release --locked -p nrese-store --example perf_lab --target-dir "/target/perf-lab-$LABEL" \
  >"$OUT/.build-$LABEL.log" 2>&1 || { tail -20 "$OUT/.build-$LABEL.log"; exit 1; }

# shellcheck disable=SC2086
docker run --rm ${CPUS:+--cpus "$CPUS"} -v nrese-target:/target:ro -v nrese-bench-data:/data:ro \
  -v "$ROOT:/src:ro" -v "$OUT:/out" "$RUST_IMAGE" \
  "/target/perf-lab-$LABEL/release/examples/perf_lab" $inputs --queries "$queries" \
  --label "$LABEL" --json "/out/$REPORT" "$@"
echo "report: benches/baselines/perf-lab/$REPORT"
