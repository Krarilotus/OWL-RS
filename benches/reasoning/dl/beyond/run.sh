#!/bin/sh
# Inputs outside OWL 2 DL (docs/design/owl2-dl.md §16), as one queue job: the DL-safe rule
# campaigns (cases and NRESE's answers by `rules_campaign`, then HermiT's and Openllet's by
# the runner's nrese.dl.Scope), and NRESE's consistency, classification and justifications
# of the ORE ontologies the reader finds diagnostics in (`dl_beyond --survey`'s list).
#
#   sh run.sh BIN_DIR SURVEY_TSV
#
# BIN_DIR holds `dl_beyond` and `rules_campaign` (built on the build host); SURVEY_TSV is
# `dl_beyond --survey`'s output over the corpus. The runner: DL_RUNNER_JAR and
# DL_RUNNER_CLASSES (nrese.dl.Scope compiled with --release 21), JAVA. Results in
# $QUEUE_RESULTS (rules/, ore/).
set -u
BIN=$1
SURVEY=$2
R=${QUEUE_RESULTS:?a queue job's results directory}
JAVA=${JAVA:-java}
CP=$DL_RUNNER_JAR:$DL_RUNNER_CLASSES
mkdir -p "$R/rules" "$R/ore/tax"
# The campaigns: seed and flags.
for c in "1 --no-data" "2" "3 --built-ins" "4 --clean" "5 --clean --no-data"; do
  set -- $c
  s=$1
  shift
  nice -n 10 "$BIN/rules_campaign" --count 1000 --seed "$s" "$@" \
    --cases "$R/rules/r$s-cases.tsv" --out "$R/rules/r$s-nrese.tsv" || exit 1
  while true; do
    nice -n 10 "$JAVA" -Xmx4g -cp "$CP" nrese.dl.Scope \
      "$R/rules/r$s-cases.tsv" "$R/rules/r$s-refs.tsv" hermit,openllet 20
    rc=$?
    [ "$rc" -eq 0 ] && break
    # 3: a timeout; the reasoner may still run, so a fresh JVM goes on.
    [ "$rc" -eq 3 ] || exit "$rc"
  done
done
# The ORE ontologies with reader diagnostics.
FILES=$(awk -F'\t' '$4!=0 {print $1}' "$SURVEY")
nice -n 10 "$BIN/dl_beyond" --decide --verbose --timeout 300 $FILES > "$R/ore/decide.txt"
nice -n 10 "$BIN/dl_beyond" --classify --timeout 300 --max-tests 3000 --tax "$R/ore/tax" \
  $FILES > "$R/ore/classify.tsv"
for f in $FILES; do
  n=$(basename "$f" .owl)
  nice -n 10 "$BIN/dl_beyond" --justify --timeout 300 "$f" > "$R/ore/$n-justification.ofn"
done
