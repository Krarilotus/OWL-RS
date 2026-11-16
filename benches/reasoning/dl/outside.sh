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
# First the check against its planted faults: one it misses stops the run.
env -u NRESE_DL_STEPS "$BIN" --ignored dl::outside::planted_ > "$OUT/planted.log" 2>&1 || { echo "planted fault missed" >&2; exit 5; }
# 101: the test ran and found failures (summary.tsv, fail-*.ttl); anything else is the harness's.
"$BIN" --ignored --exact --nocapture dl::outside::dl_answers_agree_with_outside_reasoners \
  > "$OUT/test.log" 2>&1 || rc=$?
# Per outcome; the cases, and those with a query judged (an inconsistent case tests the write
# alone); the rows set aside, where the two references differ, to be read by hand; the writes
# the store declined as undecided (class: HermiT's word on the data; outcome: Openllet's).
S=$OUT/summary.tsv
[ ! -f "$S" ] || { awk -F'\t' '{n[$6]++} $5 ~ /^(declined|undecided)-/ {n[$5]++} END {for (k in n) print k, n[k]}' "$S" | sort
  awk -F'\t' '!c[$1]++ {all++} $4 != "write" && $5 !~ /^(reference-(undecided|gap)|not-judged)$/ && !q[$1]++ {n++}
    END {print "cases", all; print "cases with a query judged", n + 0}' "$S"
  awk -F'\t' '$5 == "reference-gap" {s = s " " $1} END {print "reference-gap, set aside whole, seeds:" s}' "$S"
  echo "not judged: an inconsistent state after the first commit that the store accepted"
  echo "failures not written (unwritten.tsv, with why): $(wc -l < "$OUT/unwritten.tsv")"
  # The certification: answers marked unsound and wrong, unsound and right, unmarked and wrong.
  awk -F'\t' -v OFS='\t' '{s[$1]; u[$1] += $5 == "unsound-wrong"; r[$1] += $5 == "unsound-right"
    w[$1] += $5 == "wrong"} END {for (k in s) print k, u[k], r[k], w[k]}' "$S" | sort -n > "$OUT/marks.tsv"
  awk -F'\t' '{u += $2; r += $3; w += $4} END {print "marked unsound and wrong", u + 0
    print "marked unsound and right", r + 0; print "not marked and wrong (must be 0)", w + 0}' "$OUT/marks.tsv"
  # Key rows by outcome; in a run of 100 cases or more each control needs 5 passes by both references.
  awk -F'\t' '$4 ~ /^(anon|named|mixed)-/ {n[$4 " " $6]++} END {for (k in n) print "key row", k, n[k]}' "$S" | sort
  awk -F'\t' '!c[$1]++ {all++} $4 ~ /^(named|mixed)-/ {s[$4]; p[$4] += $6 == "pass" && $5 != "reference-undecided"}
    END {for (r in s) if (all >= 100 && p[r] < 5) print "control not established for " r}' "$S"
  awk -F'\t' '$6 == "set-aside"' "$S" > "$OUT/set-aside.tsv"
  awk -F'\t' '$5 ~ /^declined/' "$S" > "$OUT/declined.tsv"; } > "$OUT/counts.txt"
! grep -q '^control not established' "$OUT/counts.txt" || { echo "a control not established" >&2; exit 7; }
exit "${rc:-0}"
