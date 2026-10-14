#!/usr/bin/env bash
# The release gate (the owner, 13 Oct 2026): the integrated release candidate against the
# pre-regression baseline and the current main, every fast case, on one host and one
# configuration, A-B-B-A. Each command is a Fleet step using this versioned runner.
# Scheduling, checkout transfer and dependencies belong to Fleet (see benches/fast/HANDOFF.md).
#
#   release-gate.sh build REV                  REV's binaries for fast.py's native runtime
#   release-gate.sh pass N SIDE REV CANDIDATE  pass N: the candidate's cases, once each, on REV's binaries
#   release-gate.sh report CANDIDATE NAME=REV ...   the passes pooled per side and reported
#
# One harness for every side: the candidate's fast suite (its cases, data, checks and
# expected outcomes); only the engine's binaries differ, each built by its own revision with
# its own pinned toolchain into $GATE/bin-<rev>/release/ (the target-directory layout
# NRESE_FAST_BIN names). A binary a revision doesn't have is missing there: the cases that
# need it fail on that side, and the report says so. A pass with its report is skipped
# (resumable); a pass's case failures are data, not the chunk's failure. GATE_CASES limits
# the cases (a smoke run of the steps; the gate itself runs them all).
set -uo pipefail
GATE=${NRESE_GATE_DIR:-$HOME/owlrs/gate}
OUT=${FLEET_OUT:-${QUEUE_RESULTS:-$GATE/results}}
mkdir -p "$GATE" "$OUT"
log() { echo "$(date '+%F %T') $*" | tee -a "$OUT/gate.log"; }
short() { git rev-parse --short=8 "$1"; }

worktree() {  # REV: a worktree of the revision, made once
  local rev=$1 dir
  dir=$GATE/src-$(short "$rev")
  if [ ! -d "$dir" ]; then
    git worktree add -q --detach "$dir" "$rev" || { echo "no worktree of $rev" >&2; exit 2; }
  fi
  echo "$dir"
}

case ${1:-} in
build)
  rev=$(git rev-parse "$2") || exit 2
  src=$(worktree "$rev")
  bin=$GATE/bin-$(short "$rev")
  export CARGO_TARGET_DIR=$GATE/target-$(short "$rev") PATH=$HOME/.cargo/bin:$PATH
  log "build $rev ($(cd "$src" && rustc -V 2>/dev/null))"
  mkdir -p "$bin/release/examples"
  missing=()
  for spec in nrese-store:perf_lab nrese-store:ql_print_check nrese-dl:tableau_consistency \
              nrese-dl:context_classify nrese-reasoner:classify; do
    crate=${spec%%:*} example=${spec#*:}
    if [ -e "$src/crates/$crate/examples/$example.rs" ] || [ -e "$src/crates/$crate/examples/$example/main.rs" ]; then
      (cd "$src" && bash scripts/cargo-guarded.sh build --release --locked --quiet -p "$crate" --example "$example") \
        || { log "build of $example failed at $rev"; exit 1; }
      cp "$CARGO_TARGET_DIR/release/examples/$example" "$bin/release/examples/"
    else
      missing+=("$example")
    fi
  done
  (cd "$src" && bash scripts/cargo-guarded.sh build --release --locked --quiet -p nrese-server) \
    || { log "build of nrese-server failed at $rev"; exit 1; }
  cp "$CARGO_TARGET_DIR/release/nrese-server" "$bin/release/"
  # A perf_lab older than the harness's --routes (main at f9b515e8): a wrapper drops the
  # argument, so the case runs; its routes go unasserted on this side (release_gate.py
  # --routes-unchecked), its answers and times are compared as on every side.
  if [ -f "$bin/release/examples/perf_lab" ] && ! grep -q -- "--routes" "$bin/release/examples/perf_lab"; then
    mv "$bin/release/examples/perf_lab" "$bin/release/examples/perf_lab.real"
    printf '%s\n' '#!/bin/sh' 'real="$(dirname "$0")/perf_lab.real"' 'n=$#; i=0' \
      'while [ "$i" -lt "$n" ]; do a=$1; shift; i=$((i + 1)); [ "$a" = --routes ] || set -- "$@" "$a"; done' \
      'exec "$real" "$@"' > "$bin/release/examples/perf_lab"
    chmod +x "$bin/release/examples/perf_lab"
    echo "perf_lab without --routes: wrapped (routes unchecked on this side)" >> "$bin/BUILD.txt.extra"
    log "perf_lab of $rev has no --routes: wrapped, routes unchecked on its side"
  fi
  echo "$rev" > "$bin/REVISION"
  { echo "revision $rev"; (cd "$src" && rustc -V); echo "missing at this revision: ${missing[*]:-none}"; } > "$bin/BUILD.txt"
  (cd "$bin" && find release -type f | sort | xargs sha256sum > SHA256SUMS)
  log "built $rev into $bin; missing: ${missing[*]:-none}"
  ;;
pass)
  n=$2 side=$3 rev=$(git rev-parse "$4") cand=$(git rev-parse "$5")
  bin=$GATE/bin-$(short "$rev")
  harness=$(worktree "$cand")
  check=(--harness-root "$harness" --harness-revision "$cand" --engine "candidate=$rev=$bin")
  [ -z "${GATE_CASES:-}" ] || check+=(--cases "$GATE_CASES")
  label=gate-$n-$side
  reports=("$OUT"/fast/*-"$label".json)
  if [ -f "${reports[0]}" ]; then
    if [ "${#reports[@]}" -eq 1 ] && python3 benches/fast/release_gate.py "${check[@]}" --check-pass "${reports[0]}"; then
      log "pass $n ($side): complete before"; exit 0
    fi
    archive=$(mktemp -d "$OUT/interrupted-$label-XXXXXX") || exit 2
    mv -- "${reports[@]}" "$archive/"
    [ ! -f "$OUT/pass-$n-$side.log" ] || mv -- "$OUT/pass-$n-$side.log" "$archive/"
    log "pass $n ($side): prior incomplete or stale evidence preserved in $archive"
  fi
  (cd "$harness" && [ "$(git rev-parse HEAD)" = "$cand" ] && [ -z "$(git status --porcelain)" ]) \
    && [ "$(cat "$bin/REVISION" 2>/dev/null)" = "$rev" ] && (cd "$bin" && sha256sum -c SHA256SUMS) \
    || { log "pass $n: clean harness or verified build unavailable"; exit 2; }
  log "pass $n: $side $rev, the cases of $cand"
  (cd "$harness" && NRESE_FAST_RUNTIME=native NRESE_FAST_BIN=$bin NRESE_FAST_REVISION=$rev \
     NRESE_FAST_DATA=$GATE/fast-data NRESE_BENCH_REPORTS=$OUT NRESE_FAST_BASELINES=$OUT/fast/compact PYTHONIOENCODING=utf-8 \
     python3 benches/fast/fast.py run --reps 1 --label "$label" ${GATE_CASES:+--cases "$GATE_CASES"}) \
     > "$OUT/pass-$n-$side.log" 2>&1
  code=$?
  log "pass $n ($side): exit $code"
  reports=("$OUT"/fast/*-"$label".json)
  [ "${#reports[@]}" -eq 1 ] && python3 benches/fast/release_gate.py "${check[@]}" --check-pass "${reports[0]}" \
    || { log "pass $n: missing or incomplete report"; exit 1; }
  ;;
report)
  prior=()
  for file in "$OUT/release-gate.md" "$OUT/release-gate.json" "$OUT/report.log"; do
    [ ! -f "$file" ] || prior+=("$file")
  done
  if [ "${#prior[@]}" -gt 0 ]; then
    archive=$(mktemp -d "$OUT/prior-report-XXXXXX") || exit 2
    mv -- "${prior[@]}" "$archive/"
  fi
  cand=$(git rev-parse "$2")
  shift 2
  passes() { ls "$OUT"/fast/*-gate-*-"$1".json 2>/dev/null | grep -v compact | paste -sd, -; }
  harness=$(worktree "$cand")
  args=(--candidate "$(passes cand)" --harness-root "$harness" --harness-revision "$cand" \
        --engine "candidate=$cand=$GATE/bin-$(short "$cand")")
  [ -z "${GATE_CASES:-}" ] || args+=(--diagnostic-passes 4 --cases "$GATE_CASES")
  for spec in "$@"; do
    name=${spec%%=*}
    [ "$name" != candidate ] || { log "candidate is a reserved baseline name"; exit 2; }
    rev=$(git rev-parse "${spec#*=}") || exit 2
    args+=(--engine "$name=$rev=$GATE/bin-$(short "$rev")")
    args+=(--baseline "$name=$(passes "$name")")
    # A baseline whose perf_lab was wrapped (no --routes): its routes go unasserted.
    [ -f "$GATE/bin-$(short "${spec#*=}")/release/examples/perf_lab.real" ] && args+=(--routes-unchecked "$name")
  done
  python3 benches/fast/release_gate.py "${args[@]}" --out "$OUT/release-gate.md" --json "$OUT/release-gate.json" \
    > "$OUT/report.log" 2>&1
  code=$?
  log "report: $( [ $code -eq 0 ] && echo passed || echo 'not passed') ($OUT/release-gate.md)"
  exit "$code"
  ;;
*)
  sed -n '2,20p' "$0"
  exit 2
  ;;
esac
