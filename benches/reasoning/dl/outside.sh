#!/bin/sh
# `sh benches/reasoning/dl/outside.sh OUT_DIR`, from the repository's root: the store's owl2-dl
# answers against HermiT and Openllet (crates/nrese-store/tests/it/dl/outside.rs). The
# environment: the test's own, DL_OUTSIDE_LABEL (every container's label, `fleet.job=<id>` on
# the fleet) and DL_OUTSIDE_MEMORY (each reference container's cap). The image is tagged by the
# kit's tree; identity.txt holds the commit, the image id and the test binary's hash.
set -eu
OUT=$1
mkdir -p "$OUT"
TAG=nrese-dl-outside:$(git rev-parse --short=12 HEAD:benches/reasoning/dl)
docker image inspect "$TAG" > /dev/null 2>&1 || docker build -q -t "$TAG" benches/reasoning/dl > /dev/null
git rev-parse HEAD 'HEAD^{tree}' > "$OUT/identity.txt"
echo "image $TAG $(docker image inspect -f '{{.Id}}' "$TAG")" >> "$OUT/identity.txt"
BIN=$(cargo test --locked --release -p nrese-store --test it --no-run --message-format=json \
  | sed -n 's/.*"executable":"\([^"]*\)".*/\1/p' | tail -n 1)
[ -x "$BIN" ] || { echo "no test binary" >&2; exit 4; }
echo "test $(sha256sum "$BIN")" >> "$OUT/identity.txt"
export NRESE_DL_OUT="$OUT" NRESE_DL_REFERENCE="exec docker run --rm -i --init \
  --memory ${DL_OUTSIDE_MEMORY:-2g} --label ${DL_OUTSIDE_LABEL:-dl-outside=1} --entrypoint java \
  $TAG -Xmx1500m -cp /opt/dl-runner.jar nrese.dl.Entail 30"
# 101: the test ran and found failures (summary.tsv, fail-*.ttl); anything else is the harness's.
"$BIN" --ignored --exact --nocapture dl::outside::dl_answers_agree_with_outside_reasoners \
  > "$OUT/test.log" 2>&1 || rc=$?
[ ! -f "$OUT/summary.tsv" ] ||
  awk -F'\t' '{n[$6]++} END {for (k in n) print k, n[k]}' "$OUT/summary.tsv" | sort > "$OUT/counts.txt"
exit "${rc:-0}"
