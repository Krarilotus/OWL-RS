#!/usr/bin/env bash
# The release gate (benches/PROTOCOL.md §5, "The release gate"): does a head regress, beyond
# noise, on any workload, in time or memory, against the v1.5 baseline and against main? Are
# the §0 guards green and the W3C suites' counts as required? Each command is a fleet step;
# `jobs` writes the jobs (benches/fast/release_gate.py jobs). Every command runs the code of the
# commit it is launched from, never changes not committed (only this script, scripts/lib and the
# checker start from the working tree).
#
#   release-gate.sh build REV               REV's binaries, by REV's own toolchain, into GATE_BIN/bin-REV8-IDENTITY
#   release-gate.sh prepare HARNESS         the plan, the data view and the cases' data, once, by
#                                           HARNESS's generator; WatDiv's query sets checked
#   release-gate.sh block N HARNESS LAYOUT PLAN|- base=REV main=REV cand=REV
#                                           block N: each of HARNESS's cases run base, main, cand,
#                                           cand, main, base back to back (a palindrome per case:
#                                           every side's runs centred on the same time), on each
#                                           side's build of layout LAYOUT (GATE_LAYOUTS), copied
#                                           for the block; orientation and case order from GATE_SEED
#   release-gate.sh report HARNESS SIDE=REV ...   the timed part's report (the first SIDE the candidate)
#   release-gate.sh w3c                     the W3C suites with their data required, their counts checked
#   release-gate.sh guards                  every guard docs/design/performance.md §0 names, run and passed
#   release-gate.sh pack REV ...            the revisions' verified binaries as one tar (FLEET_OUT/binaries),
#                                           for a host that builds nothing (the big host)
#   release-gate.sh unpack                  that tar (FLEET_INPUT_BINARIES) into GATE_BIN, verified again
#   release-gate.sh confirm TIMED_DIR... --out DIR   the confirmation job of each host whose run flagged
#                                           a loss or an undecided case (rg-H8-confirm, rg-H8-confirm2)
#   release-gate.sh plan-cases CASES        a confirmation's plan: just those cases (GATE_PLAN)
#   release-gate.sh plan-blocks HARNESS     each block's plan: the cases whose noise needs its block
#                                           (benches/fast/measure.toml); `block N HARNESS L PLAN ...`
#                                           runs it
#   release-gate.sh verdict TIMED_DIR... CHECKS_DIR   every part together, each host's rounds combined
#   release-gate.sh jobs CAND [OPTIONS]     the fleet jobs of a whole gate (release_gate.py jobs --help)
#   release-gate.sh clean                   the gate's worktrees and build trees, and what dead invocations
#                                           of its process space left (binaries and data stay; a live
#                                           invocation's own, and another process space's, stay)
#   release-gate.sh clean --patch-keyed [--remove]   the builds of a perf lab patched before T0, listed
#                                           (FLEET_OUT/patch-keyed-builds.tsv); with --remove their binaries
#                                           go, unless a gate used them within GATE_LIVE_HOURS (default 24)
#
# One harness for every side: HARNESS's fast suite (its cases, checks, generator and data);
# only the engine's binaries differ. A side whose perf lab predates --routes (main) runs with
# fast.py --no-routes --empty-queries, read from its CAPABILITIES. A block with every report
# complete is kept (the step resumes); an incomplete one is moved aside, kept, and run again whole.
#
# GATE (NRESE_GATE_DIR, default ~/owlrs/$GATE_NAME, gate-v2) keeps binaries, data and passes between
# steps; results go to FLEET_OUT. GATE_CASES limits the cases (a dry run), GATE_MAX_GB is the
# memory the host gives a case (default 24: a larger cap is not run, never lowered),
# GATE_MIN_BLOCKS the blocks of the round (default 2; fewer is diagnostic). GATE_BIG_GB and
# GATE_BIG_HOST: cases with caps up to it run on that Linux host instead (khaleesi-wsl; its own
# job: GATE_MIN_GB the timed host's limit, GATE_MAX_GB the big one's). No case is waived: only an owner-approved
# trade-off (benches/release-tradeoffs.toml) accepts a loss. SPARQL_DATA (default
# ~/owlrs/sparql-data) holds the SPARQL workloads' data and their generated query sets.
set -uo pipefail
cd "$(dirname "$0")/.."
export PATH=$HOME/.cargo/bin:$PATH
# Every git call of a gate reads the objects as stored, from the measurement check (release_gate.py
# measured-at, STORED_OBJECTS) to the checkout a build compiles and its identity: no replace ref,
# made before the check or after it, stands another object in, and no graft another parent.
export GIT_NO_REPLACE_OBJECTS=1 GIT_GRAFT_FILE=/dev/null
GATE=${NRESE_GATE_DIR:-$HOME/owlrs/${GATE_NAME:-gate-v2}}
# Binaries by build identity, shared by every gate (v1.5's and main's are built once):
# published whole, never replaced (scripts/lib/gate-publish.sh).
GATE_BIN=${GATE_BIN:-$HOME/owlrs/gate-v2-bin}
OUT=${FLEET_OUT:-$GATE/results}
SPARQL_DATA=${SPARQL_DATA:-$HOME/owlrs/sparql-data}
SHARED=${NRESE_FAST_SHARED:-$HOME/owlrs/fast-data-shared}
MAX_GB=${GATE_MAX_GB:-24}
# A directory the caller names relative to the launching checkout stays there: the gate runs its
# code from a checkout of its own (below; Sol, PR #60 fix 3's sol-loop S2).
anchored() { case $1 in /* | [A-Za-z]:[/\\]*) printf '%s' "$1" ;; *) printf '%s/%s' "$PWD" "$1" ;; esac; }
GATE=$(anchored "$GATE") GATE_BIN=$(anchored "$GATE_BIN") OUT=$(anchored "$OUT")
SPARQL_DATA=$(anchored "$SPARQL_DATA") SHARED=$(anchored "$SHARED")
mkdir -p "$GATE" "$GATE_BIN" "$OUT"
# A desktop's file indexer (GNOME's localsearch on phuoc-yu indexes all of $HOME) reads every
# file the gate writes, during the passes: these directories are excluded from it (Tracker's
# ignored-directories-with-content; the file's removal undoes it).
for dir in "$GATE" "$GATE_BIN"; do [ -e "$dir/.trackerignore" ] || echo "release gate: not indexed" > "$dir/.trackerignore"; done
log() { echo "$(date '+%F %T') $*" | tee -a "$OUT/gate.log"; }
short() { git rev-parse --short=8 "$1"; }
py() { PYTHONIOENCODING=utf-8 python3 "$@"; }
# Each invocation's own directory, made empty here and removed when it exits, by it alone: its
# bytecode cache and its code checkout (below). No other invocation makes or reads what is in it
# (Sol, PR #60 fix check 3: a code checkout shared by revision had no owner, and two callers met
# in its making). It lies in its process space's directory under its pid, which `clean` asks only
# there: a pid means nothing in another space, as WSL's in Git Bash on one host (Sol, PR #60 fix
# check 4). So does every checkout the gate makes (the harness's, a build's): a path another space
# uses is never one of its (Sol, R4 sol-loop S1, S5, S6). The space: the kernel's boot, pid and
# mount namespaces where Linux names them, else the system, host and runtime (Git Bash's MSYS,
# whose pids no other runtime knows).
launch=$PWD
if [ -r /proc/sys/kernel/random/boot_id ] && space=$(readlink /proc/self/ns/pid /proc/self/ns/mnt 2>/dev/null); then
  space="$(cat /proc/sys/kernel/random/boot_id) $space"
else
  space="$(uname -s) $(hostname) $(cygpath -m / 2>/dev/null)"
fi
SPACE=$GATE/run/space-$(printf '%s' "$space" | cksum | cut -d' ' -f1)
mkdir -p "$SPACE" && RUN=$(mktemp -d "$SPACE/$$-XXXXXX") || exit 2
# DIR, a checkout of this space's, removed with its registration: by git, else (its .git gone: a
# removal killed halfway) its directory, then its registration by the path, which git drops for
# a missing directory. Never `prune`: a registration git finds missing here may be another
# process space's live checkout (Sol, PR #60 R4 sol-loop S1).
drop_checkout() {
  git worktree remove --force "$1" 2>/dev/null && return
  rm -rf "$1" && git worktree remove --force "$1" 2>/dev/null
  return 0
}
finish() {
  local dir
  cd "$launch" 2>/dev/null || cd /
  for dir in "$RUN"/src-*; do [ ! -d "$dir" ] || drop_checkout "$dir"; done
  rm -rf "$RUN"
}
trap finish EXIT
# Every Python the gate starts reads and writes bytecode only there: no cache older than the run,
# in a checkout (Sol, S1 r3), beside one or in an earlier run's directory (PR #60 r1 R2), stands in
# for a source the gate checked. A prefix holds every cache, the standard library's too, so the run
# compiles that once (0.3 s; then 0.08 s a start, against 0.1 s with the installed caches and the
# tree compiled each time: job lane-d-pr60-r1-startup2-c600217b).
PYTHONPYCACHEPREFIX=$RUN/pycache
mkdir "$PYTHONPYCACHEPREFIX" || exit 2
export PYTHONPYCACHEPREFIX
# The check of every checkout the gate makes: one of the launching checkout's scripts, as this one
# is, which imports nothing of any checkout (python3 -I), so it can check the gate's own code first.
CHECKER=$PWD/benches/suite/suitekit/checkout.py

worktree() {  # REV [DIR [built]]: a clean worktree of the revision, made once, holding its stored
  # bytes; one to be built holds nothing else. DIR is this process space's (default $SPACE).
  local rev dir only=()
  rev=$(git rev-parse "$1") || return 2
  dir=${2:-$SPACE}/src-$(short "$rev")
  if [ ! -d "$dir" ]; then
    # Line ends as stored: a host's settings convert none (only .gitattributes does). -f: a
    # registration of this space's path whose directory went without git (a clean killed between
    # the two) stops nothing; no other space has this path (Sol, PR #60 R4 sol-loop S5).
    git -c core.autocrlf=false -c core.eol=lf worktree add -f -q --detach "$dir" "$rev" \
      || { echo "no worktree of $rev" >&2; return 2; }
  fi
  (cd "$dir" && [ "$(git rev-parse HEAD)" = "$rev" ] && [ -z "$(git status --porcelain)" ]) \
    || { echo "the worktree $dir is not $rev, clean" >&2; return 2; }
  # A build reads ignored files too (nrese-server's build.rs embeds apps/nrese-console/dist/): a
  # worktree to be built loses every file not in the revision, the last build's leftovers with them.
  if [ "${3:-}" = built ]; then
    git -C "$dir" clean -ffdxq || { echo "the worktree $dir can't be cleaned" >&2; return 2; }
    only=(--nothing-else)
  fi
  # HEAD and a clean status trust the index and the attributes; the bytes are the proof. Its
  # output is diagnostics: the function's own is the directory alone.
  python3 -I "$CHECKER" "$rev" "$dir" ${only[@]+"${only[@]}"} >&2 \
    || { echo "the worktree $dir doesn't hold $rev's bytes" >&2; return 2; }
  echo "$dir"
}

# The builds by identity, this gate's pointer to each revision's (bin_of), the age sweep.
. scripts/lib/gate-publish.sh
# Layout L of a revision (GATE_LAYOUTS): 1 the plain build, L > 1 the same with its functions
# linked in another order (rust-lld --shuffle-sections, seed L): a layout, not other code. Each
# layout is a build of its own identity (build_identity REV L) and this gate's pointer.
layout_bin() { bin_of "$1" "${2:-1}"; }
verified() { build_verified "$1" "$2"; }
LAYOUTS=${GATE_LAYOUTS:-1}

# The environment of a fast.py run on HARNESS's tree: native, the gate's data, nothing
# written into the tree (the compact records go to the gate).
fast_env() {
  # The prepared data: one read-only copy per host, hardlinked into every gate's fast-data
  # (fast.py prepare, NRESE_FAST_SHARED).
  export NRESE_FAST_SHARED=$SHARED
  export NRESE_FAST_RUNTIME=native NRESE_FAST_DATA=$GATE/fast-data BENCH_GENERAL_DATA=$GATE/data \
    NRESE_FAST_QUERY_SETS=$GATE/query-sets NRESE_FAST_BASELINES=$GATE/compact NRESE_BENCH_REPORTS=$GATE/reports \
    NRESE_QUIET_SLOT=${NRESE_QUIET_SLOT:-0}
  mkdir -p "$GATE/fast-data" "$GATE/data" "$GATE/query-sets" "$GATE/compact" "$GATE/reports"
}

# The plan the passes run: the gate's (prepare), or a confirmation's (plan-cases, GATE_PLAN);
# a pass of a later block runs its block's (plan-blocks: the cases whose noise needs it).
PLAN=$GATE/${GATE_PLAN:-plan.json}
planned() { py -c 'import json,sys; print(",".join(json.load(open(sys.argv[1]))["run"]))' "${1:-$PLAN}"; }
# The measurement per case (runs, warm-ups, repetitions; release_gate.py calibrate), the
# harness's, for every side alike.
MEASURE=benches/fast/measure.toml

# `clean` makes no code checkout, so that it runs when none can be made (no space, a broken one):
# what it removes runs no project Python but its locks', which where there is no flock is ns.sh's
# suitekit/lock.py, here the launching commit's bytes written alone into this run's directory.
if [ "${1:-}" = clean ]; then
  mkdir -p "$RUN/suite/suitekit" \
    && git cat-file blob HEAD:benches/suite/suitekit/lock.py > "$RUN/suite/suitekit/lock.py" \
    || { echo "release gate: no lock helper of the launching commit" >&2; exit 2; }
  NS_SUITE=$RUN/suite
  if [ "${2:-}" = --patch-keyed ]; then
    # The builds of a perf lab patched at build time (before T0, 28 Oct 2026), which no gate after
    # T0 can reuse (their identity has the patch): listed, and with --remove their binaries go.
    sweep_patch_keyed "${3:-}" | tee "$OUT/patch-keyed-builds.tsv"
    exit "${PIPESTATUS[0]}"
  fi
  # This gate's worktrees, build trees and scratch go (never the shared binaries' directory:
  # other gates build and read there); the data view, the prepared data and every result stay
  # for the gate's next run. Builds no gate used for GATE_BIN_AGE_DAYS go (sweep_builds).
  before=$(du -sb "$GATE" "$GATE_BIN" 2>/dev/null | awk '{s += $1} END {print s}')
  # What invocations of this process space that died left in their own directories goes; a live
  # one's stays, and so does every other space's, whose pids this one can't ask.
  for run in "$SPACE"/[0-9]*-*; do
    [ -d "$run" ] && [ "$run" != "$RUN" ] || continue
    pid=${run##*/}
    ! kill -0 "${pid%%-*}" 2>/dev/null || continue
    for dir in "$run"/src-*; do [ ! -d "$dir" ] || drop_checkout "$dir"; done
    rm -rf "$run"
  done
  foreign=$(( $(find "$GATE"/run/space-* -mindepth 1 -maxdepth 1 -name '[0-9]*-*' ! -path "$SPACE/*" 2>/dev/null | wc -l)
    + $(find "$GATE/run" -mindepth 1 -maxdepth 1 ! -name 'space-*' 2>/dev/null | wc -l) ))
  [ "$foreign" = 0 ] || log "clean: $foreign runs of another process space kept"
  # This space's harness and build checkouts (no other space has their paths) and build trees.
  for dir in "$SPACE"/src-* "$SPACE"/build/src-*; do [ ! -d "$dir" ] || drop_checkout "$dir"; done
  rm -rf "$SPACE"/build
  # Those of the gates before checkouts were each space's ($GATE/src-*, $GATE/build), removed as
  # they were then, by git; pycache: their bytecode, before each invocation had its own directory.
  for dir in "$GATE"/src-* "$GATE"/build/src-*; do [ -d "$dir" ] && git worktree remove --force "$dir"; done
  rm -rf "$GATE"/target-* "$GATE"/build "$GATE"/reports "$GATE"/compact "$GATE"/pycache
  # Block copies are removed by their block; what a killed one left is named, not swept.
  rmdir "$GATE/instances" 2>/dev/null || { [ -d "$GATE/instances" ] \
    && log "clean: block copies left by an interrupted block, not removed: $(ls "$GATE/instances" | tr '\n' ' ')"; }
  swept=$(sweep_builds | tr '\n' ' ')
  [ -z "$swept" ] || log "clean: unused builds removed: $swept"
  # No `git worktree prune` (drop_checkout): what is removed here deregistered itself.
  # Shared prepared data no gate has linked for two days (its last link's removal set its ctime).
  [ -d "$SHARED/blobs" ] && find "$SHARED/blobs" -type f -links 1 -cmin +2880 -delete
  after=$(du -sb "$GATE" "$GATE_BIN" 2>/dev/null | awk '{s += $1} END {print s}')
  log "clean: $(( (before - after) / 1048576 )) MiB reclaimed; kept the binaries, the data view, the prepared data, the results"
  exit
fi

# The gate's own code runs from a checkout of the commit it is launched from, made from the
# repository's objects into this run's directory and checked as every checkout of the gate is
# (worktree), never from the launching checkout: files there beside the commit's, an untracked
# suitekit/cases/__init__.py or a csv.py before the standard library's, ran in the validator (Sol,
# PR #60 fix checks 1 and 2), and no rule of what may lie there holds for every caller. From here
# every relative path, and every Python the gate starts, is that checkout's or a harness
# checkout's; only the launching checkout's shell scripts (this one, scripts/lib), the checker and
# objects are read. Launched from a checkout with changes not committed (a commit hook's), the gate
# runs its commit's Python, never the changes. The commands that take a caller's paths (jobs,
# confirm, verdict) run in the launching checkout, their Python by path from this one.
CODE=$(worktree HEAD "$RUN") || { echo "release gate: no checked checkout of the launching commit" >&2; exit 2; }
cd "$CODE" || exit 2
# The libraries' Python too (ns.sh's suitekit/lock.py where there is no flock): ns.sh, sourced
# above, named the launching checkout's (Sol, PR #60 fix 3's sol-loop S1).
NS_SUITE=$CODE/benches/suite

case ${1:-} in
build)
  rev=$(git rev-parse "$2") || exit 2
  # The commit built and timed for this side: the revision, or its measurement commit, a
  # reviewed commit on it that changes only its perf lab (benches/fast/measurement-sides.toml).
  # Any other is refused; the side's REVISION and pointer stay the revision's.
  at=$(py benches/fast/release_gate.py measured-at "$rev") || { log "build $rev: refused"; exit 2; }
  export PATH=$HOME/.cargo/bin:$PATH
  # The gate's flags replace a host's (its ~/.cargo/config.toml rustflags), before the identity
  # is taken: the identity hashes the flags the build gets.
  export RUSTFLAGS=$GATE_RUSTFLAGS
  unset CARGO_ENCODED_RUSTFLAGS  # it would take precedence over RUSTFLAGS
  # Each layout's build by its identity: one there from before counts, the others are built.
  todo=()
  for L in $(seq 1 "$LAYOUTS"); do
    lbin=$(identity_dir "$at" "$L")
    if build_verified "$lbin" "$rev"; then point "$rev" "$lbin" "$L"; else todo+=("$L"); fi
  done
  bin=$(identity_dir "$at" 1)
  if [ ${#todo[@]} -eq 0 ]; then
    cp "$bin/BUILD.txt" "$OUT/build-$(short "$rev").txt"
    log "build $rev: verified binaries of its identity there from before ($bin, $LAYOUTS layouts)"; exit 0
  fi
  # The source and build trees are this gate's own; the build is published under its identity.
  mkdir -p "$SPACE/build" && src=$(worktree "$at" "$SPACE/build" built) || exit 2
  export CARGO_TARGET_DIR=$SPACE/build/target-$(short "$at")
  log "build $rev at $at ($(cd "$src" && rustc -V 2>/dev/null)); layouts to build: ${todo[*]}"
  missing=()
  if [ "${todo[0]}" = 1 ]; then
  part=$bin.partial-$$
  rm -rf "$part" && mkdir -p "$part/release/examples"
  for spec in nrese-store:perf_lab nrese-store:ql_print_check nrese-dl:tableau_consistency \
              nrese-dl:context_classify nrese-reasoner:classify; do
    crate=${spec%%:*} example=${spec#*:}
    if [ -e "$src/crates/$crate/examples/$example.rs" ] || [ -e "$src/crates/$crate/examples/$example/main.rs" ]; then
      (cd "$src" && bash scripts/cargo-guarded.sh build --release --locked --quiet -p "$crate" --example "$example") \
        || { log "build of $example failed at $rev"; exit 1; }
      cp "$CARGO_TARGET_DIR/release/examples/$example" "$part/release/examples/"
    else
      missing+=("$example")
    fi
  done
  (cd "$src" && bash scripts/cargo-guarded.sh build --release --locked --quiet -p nrese-server) \
    || { log "build of nrese-server failed at $rev"; exit 1; }
  cp "$CARGO_TARGET_DIR/release/nrese-server" "$part/release/"
  # What the built perf lab can do, from its source: route reports (--routes), whether it
  # requires --queries (main's, f9b515e8: neither, and no samples per run), and whether its
  # timed window starts no thread (the watchdog; PROTOCOL.md §5).
  routes=1 empty=0 batch=0 timing=spawn
  git grep -q -e '"--routes"' "$at" -- crates/nrese-store/examples || routes=0
  git grep -q -e 'queries DIR is required' "$at" -- crates/nrese-store/examples && empty=1
  git grep -q -e '"--batch"' "$at" -- crates/nrese-store/examples && batch=1
  git grep -q -e 'mod perf_lab_watchdog' "$at" -- crates/nrese-store/examples && timing=watchdog
  printf 'routes=%s\nempty_queries=%s\nbatch=%s\ntiming=%s\n' "$routes" "$empty" "$batch" "$timing" > "$part/CAPABILITIES"
  echo "$rev" > "$part/REVISION"
  { echo "revision $rev"; [ "$at" = "$rev" ] || echo "measurement commit $at (its parent: $rev)";
    (cd "$src" && rustc -V); echo "missing at this revision: ${missing[*]:-none}";
    cat "$part/CAPABILITIES"; } > "$part/BUILD.txt"
  lld=$(lld_of "$part") || { log "build $rev: not linked by the toolchain's LLD"; exit 1; }
  echo "linker $lld" >> "$part/BUILD.txt"
  (cd "$part" && find release -type f | sort | xargs sha256sum > SHA256SUMS)
  { build_identity "$at" 1; echo "linker $lld"; } > "$part/IDENTITY"
  publish "$part" "$bin" || { log "build $rev: $bin is there but doesn't verify"; exit 1; }
  point "$rev" "$bin" 1
  fi
  cp "$bin/BUILD.txt" "$OUT/build-$(short "$rev").txt"
  # Layouts 2..K: the timed tools linked with their functions in another order (seed L); the
  # rest copied from layout 1. A linker that isn't LLD refuses the option: the build fails.
  for L in "${todo[@]}"; do
    [ "$L" -gt 1 ] || continue
    lbin=$(identity_dir "$at" "$L")
    lpart=$lbin.partial-$$
    rm -rf "$lpart" && cp -a "$bin" "$lpart" && rm -rf "$lpart/SHA256SUMS" "$lpart/IDENTITY" "$lpart/.refs"
    for spec in nrese-store:perf_lab nrese-dl:tableau_consistency nrese-dl:context_classify nrese-reasoner:classify; do
      crate=${spec%%:*} example=${spec#*:}
      [ -e "$bin/release/examples/$example" ] || continue
      (cd "$src" && RUSTFLAGS=$(layout_flags "$L") \
         bash scripts/cargo-guarded.sh build --release --locked --quiet -p "$crate" --example "$example") \
        || { log "build of $example, layout $L, failed at $rev"; exit 1; }
      cp "$CARGO_TARGET_DIR/release/examples/$example" "$lpart/release/examples/"
    done
    echo "$L" > "$lpart/LAYOUT"
    lld=$(lld_of "$lpart") || { log "build $rev, layout $L: not linked by the toolchain's LLD"; exit 1; }
    echo "layout $L: the timed tools linked with --shuffle-sections=.text*=$L" >> "$lpart/BUILD.txt"
    (cd "$lpart" && find release -type f | sort | xargs sha256sum > SHA256SUMS)
    { build_identity "$at" "$L"; echo "linker $lld"; } > "$lpart/IDENTITY"
    cmp -s "$bin/release/examples/perf_lab" "$lpart/release/examples/perf_lab" \
      && { log "layout $L of $rev is byte-identical to layout 1: the shuffle didn't take"; exit 1; }
    publish "$lpart" "$lbin" || { log "build $rev, layout $L: $lbin is there but doesn't verify"; exit 1; }
    point "$rev" "$lbin" "$L"
    log "built layout $L of $rev into $lbin"
  done
  # The binaries stay; their build tree and source worktree go (a rebuild is rare).
  rm -rf "$CARGO_TARGET_DIR" && git worktree remove --force "$src"
  log "built $rev into $bin; missing: ${missing[*]:-none}; $(tr '\n' ' ' < "$bin/CAPABILITIES")"
  ;;

prepare)
  harness=$(git rev-parse "$2") || exit 2
  tree=$(worktree "$harness") || exit 2
  bin=$(bin_of "$harness")
  (cd "$bin" && sha256sum -c --quiet SHA256SUMS) || { log "the harness's binaries aren't verified ($bin)"; exit 2; }
  general=$(cd "$tree" && py -c 'import sys; sys.path.insert(0, "benches/suite")
from suitekit import hosts; print(hosts.input_path("general") or "")')
  fast_env
  # The plan: what this host runs at the cases' declared caps. Replaced only whole and only
  # when made: a failed plan left the passes of another job in this directory an empty one
  # (aa3-phuoc-timed, 19 Oct 2026).
  # The family's baselines: GATE_BASELINES (default base,main; an A-B-B-A of two sides: base).
  (cd "$tree" && py benches/fast/release_gate.py plan --max-gb "$MAX_GB" --min-gb "${GATE_MIN_GB:-0}" \
     --baselines "${GATE_BASELINES:-base,main}" \
     ${GATE_BIG_GB:+--big-gb "$GATE_BIG_GB" --big-host "${GATE_BIG_HOST:-khaleesi-wsl}"} ${GATE_CASES:+--cases "$GATE_CASES"}) \
    > "$GATE/plan.json.partial" && mv "$GATE/plan.json.partial" "$GATE/plan.json" \
    || { log "prepare: no plan (release_gate.py plan failed)"; exit 2; }
  cp "$GATE/plan.json" "$OUT/plan.json"
  cases=$(planned)
  log "plan: $(py -c 'import json,sys; d=json.load(open(sys.argv[1])); print(len(d["run"]), "cases;", len(d["not_run"]), "not run;", len(d["elsewhere"]), "on the big host")' "$GATE/plan.json")"
  # The data view: each dataset a planned case reads, from the host's general inputs or the
  # SPARQL data; checked against its freeze (datasets.toml) and recorded either way.
  (cd "$tree" && py - "$cases" "$general" "$SPARQL_DATA" "$GATE/data" > "$OUT/inputs.txt") <<'EOF' || exit 2
import hashlib, os, subprocess, sys, tomllib
from pathlib import Path
sys.path.insert(0, "benches/suite")
from suitekit import cases as registry
wanted, general, sparql, view = sys.argv[1].split(","), Path(sys.argv[2]), Path(sys.argv[3]), Path(sys.argv[4])
frozen = {d["file"]: d for d in tomllib.loads(Path("benches/datasets.toml").read_text())["dataset"]}
files = sorted({spec[len("volume:"):] for c in registry.load() if c["name"] in wanted
                for spec in [c["data"], *c.get("extra", []), *(c["data"][7:].split("+") if c["data"].startswith("concat:") else [])]
                if spec.startswith("volume:")})
def digest(path):
    with open(path, "rb") as f:
        return hashlib.file_digest(f, "sha256").hexdigest()
failed = False
for name in files:
    source = next((d / name for d in (general, sparql) if (d / name).is_file()), None)
    if source is None:
        print(f"MISSING {name}: neither in {general} nor in {sparql}"); failed = True; continue
    target, sha = view / name, digest(source)
    entry = frozen.get(name, {})
    status = "frozen" if sha == entry.get("sha256") else f"NOT the freeze ({entry.get('sha256', 'none')[:12]})"
    target.parent.mkdir(parents=True, exist_ok=True)
    if entry.get("n_triples") or name.startswith("bsbm-"):
        # Not N-Triples as frozen (BSBM's Turtle doubles): converted once per source, which
        # also checks a source that needs no conversion.
        stamp = target.with_name(name + ".source-sha256")
        if not (target.is_file() and stamp.is_file() and stamp.read_text().strip() == sha):
            target.unlink(missing_ok=True)
            done = subprocess.run([sys.executable, "-I", "benches/sparql/nt-numbers.py", str(source), str(target)],
                                  capture_output=True, text=True)
            if done.returncode:
                print(f"FAILED converting {name}: {done.stderr.strip()[-300:]}"); failed = True; continue
            stamp.write_text(sha)
            print(f"converted {name}: {done.stderr.strip().splitlines()[0]}")
        out = digest(target)
        want = (entry.get("n_triples") or {}).get("sha256")
        status += ("; converted as pinned" if out == want else f"; converted {out[:12]}, pin {str(want)[:12]}")
    else:
        if target.is_symlink() or target.exists():
            target.unlink()
        target.symlink_to(source)
    print(f"{name}: {source} sha256 {sha} {status}")
sys.exit(1 if failed else 0)
EOF
  cp "$OUT/inputs.txt" "$GATE/inputs.txt" && tee -a "$OUT/gate.log" < "$OUT/inputs.txt"
  # Generated query sets kept with the data (never committed): checked file by file
  # against benches/sparql/queries.sha256, then the gate reads them from there.
  for set in $(cd "$tree" && py -c 'import sys; sys.path.insert(0, "benches/suite")
from suitekit import cases as r
wanted = sys.argv[1].split(",")
print(" ".join(sorted({c["queries"][1:] for c in r.load() if c["name"] in wanted and c.get("queries", "").startswith("@watdiv")})))' "$cases"); do
    (cd "$SPARQL_DATA/queries" && grep -E "  $set/" "$tree/benches/sparql/queries.sha256" | sha256sum -c --quiet -) \
      && [ "$(ls "$SPARQL_DATA/queries/$set" | wc -l)" = "$(grep -cE "  $set/" "$tree/benches/sparql/queries.sha256")" ] \
      || { log "the query set $set differs from its pins"; exit 2; }
    ln -sfn "$SPARQL_DATA/queries/$set" "$GATE/query-sets/$set"
    log "query set $set: $(ls "$SPARQL_DATA/queries/$set" | wc -l) files match their pins"
  done
  # The cases' generated data, by the harness's generator, for every side alike.
  (cd "$tree" && NRESE_FAST_BIN=$bin NRESE_FAST_REVISION=$harness py benches/fast/fast.py prepare --cases "$cases") \
    > "$OUT/prepare.log" 2>&1
  code=$?
  tail -3 "$OUT/prepare.log" | tee -a "$OUT/gate.log"
  cp "$GATE/fast-data/PREPARED.json" "$OUT/" 2>/dev/null
  exit $code
  ;;

block)
  n=$2 layout=$4
  harness=$(git rev-parse "$3") || exit 2
  # A block's plan (plan-blocks), else the gate's.
  plan=$PLAN
  if [ "${5:--}" != - ]; then
    plan=$GATE/$5
    [ -f "$plan" ] || { log "block $n: no block plan $plan (plan-blocks first)"; exit 2; }
  fi
  shift 5
  tree=$(worktree "$harness") || exit 2
  # Each side: its build of this layout, verified, and what its perf lab can do (CAPABILITIES).
  # A main that is an earlier side's build (a confirmation job's main that is its base) is timed
  # once, as that one (the report treats it so); every other side always runs: an A-B-B-A's A'
  # (benches/suite/abba.py) is A's build by design.
  sides=() names=() timed=()
  for spec in "$@"; do
    name=${spec%%=*}
    rev=$(git rev-parse "${spec#*=}") || exit 2
    bin=$(layout_bin "$rev" "$layout")
    if [ "$name" = main ] && printf '%s\n' "${timed[@]}" | grep -qxF -- "$bin"; then
      log "block $n: $name is the same build as another side ($(basename "$bin")): timed once"; continue
    fi
    [ "$name" = cand ] || timed+=("$bin")
    build_verified "$bin" "$rev" || { log "block $n: no verified build of $rev, layout $layout"; exit 2; }
    routes=1 empty_queries=0 timing=spawn
    # shellcheck disable=SC1091
    . "$bin/CAPABILITIES"
    flags=""
    [ "$routes" = 1 ] || flags+="no-routes,"
    [ "$empty_queries" = 1 ] && flags+="empty-queries,"
    sides+=(--side "$name:$bin:$rev:${flags}timing=${timing:-spawn}")
    names+=("$name")
  done
  mkdir -p "$OUT/passes"
  # Its reports: each side's two runs of the palindrome (fast.py block), written together at its
  # end: six for base, main and cand, four for an A-B-B-A of two sides.
  per=$(( 2 * ${#names[@]} ))
  first=$(( per * (n - 1) + 1 ))
  complete() {  # how many of the block's reports are complete
    local f r c=0
    for f in $(seq "$first" $((first + per - 1))); do
      for r in "$OUT/passes/pass-$f-"*.json; do
        py -c 'import json,sys; sys.exit(0 if json.load(open(sys.argv[1])).get("finished_ns") else 1)' "$r" 2>/dev/null \
          && c=$((c + 1))
      done
    done
    echo "$c"
  }
  # The block's state: its attempt (1, or 2 after a disturbed first) and the attempt's stage,
  # replaced whole (written aside, renamed) before what it records is started, signalled or
  # moved. A crash at any point resumes the attempt it was in (not a new one), an attempt above 2
  # never starts, and reports count only once their attempt's verdict is recorded (Sol, 20 Oct
  # 2026: a restart accepted a disturbed block, or ran a third attempt).
  state=$OUT/passes/block-$n.state
  # Each state is a prerequisite of what follows it: written aside, synced, renamed over the old
  # one, the directory synced; if any of it fails the step ends there (Sol, 20 Oct 2026: a failed
  # rename went unnoticed and the block ran a third time).
  set_state() {
    py -c 'import os, sys
path, text = sys.argv[1], f"attempt={sys.argv[2]}\nstage={sys.argv[3]}\n"
with open(path + ".new", "w", encoding="utf-8") as f:
    f.write(text)
    f.flush()
    os.fsync(f.fileno())
os.replace(path + ".new", path)
if os.name != "nt":  # a directory is synced on POSIX; Windows can not open one to, and journals its entries
    fd = os.open(os.path.dirname(path), os.O_RDONLY)
    try:
        os.fsync(fd)
    finally:
        os.close(fd)' "$state" "$1" "$2" \
      || { log "block $n: its state (attempt $1, $2) couldn't be saved ($state): stopped"; exit 2; }
  }
  attempt=1 stage=start
  if [ -f "$state" ]; then
    attempt=$(sed -n 's/^attempt=//p' "$state") stage=$(sed -n 's/^stage=//p' "$state")
    case "$attempt:$stage" in
      [12]:start | [12]:running | [12]:ran | 1:archiving | [12]:accepted) ;;
      *) log "block $n: no attempt of this block is started past the second, nor from an unreadable state ($state)"; exit 2 ;;
    esac
  fi
  # What an earlier watcher left suspended (it died) is resumed before anything else; what can't
  # be settled stops the step, its record kept.
  if [ "${GATE_QUIET:-1}" != 0 ] && [ -d /proc ] && [ -f "$GATE/quiet/frozen" ]; then
    python3 "$tree/scripts/lib/quiet-watch.py" "$GATE/quiet" --resume \
      || { log "block $n: an earlier watcher's suspensions couldn't be resumed ($GATE/quiet/frozen)"; exit 2; }
  fi
  if [ "$stage" = accepted ]; then
    [ "$(complete)" = "$per" ] && { log "block $n: complete before (attempt $attempt)"; exit 0; }
    log "block $n: accepted before, but its reports are gone"; exit 2
  fi
  opts=()
  [ -f "$tree/$MEASURE" ] && opts+=(--measure "$MEASURE")
  # Batched timing (perf_lab --batch) only when every side's perf lab has it (jobs: GATE_BATCH).
  [ "${GATE_BATCH:-0}" = 1 ] && opts+=(--batch)
  # The round's seed (GATE_SEED, else drawn once and kept with its results): each square of
  # blocks' orders (fast.py palindrome()) and the cases' order in a block drawn from it, recorded
  # in every pass.
  [ -s "$OUT/seed" ] || echo "${GATE_SEED:-$(od -An -N8 -tx8 /dev/urandom | tr -d ' ')}" > "$OUT/seed"
  opts+=(--seed "$(cat "$OUT/seed")")
  fast_env
  # A timed block: prepared data only (fast.py never makes any while it times).
  export NRESE_TIMED_GATE=1
  # A plan that can't be read (empty, half written) is no block, never every case.
  cases=$(planned "$plan") && [ -n "$cases" ] || { log "block $n: the plan $plan can't be read"; exit 2; }
  log "block $n: ${names[*]}, layout $layout, on the cases of $harness, plan $(basename "$plan")"
  attempt() {  # one run of the block, watched; its reports in $OUT/passes, its verdict beside them
    local copies watcher=""
    # Every side timed from a copy of its build made for this attempt, removed after it (fast.py
    # block_copies(): a cached instance's offset is the block's, not the round's; lks-aa-20). The
    # directory is made here for it alone, and only it is removed, also if fast.py dies.
    mkdir -p "$GATE/instances" && copies=$(mktemp -d "$GATE/instances/block-$n-XXXXXX") || exit 2
    trap 'rm -rf "$copies"; finish' EXIT
    # The host kept quiet while the block runs (scripts/lib/quiet-watch.py, as the measurement
    # queue's timed chunks: the job's own other busy work suspended, anything else recorded as
    # disturbing it); its verdict kept with the block. GATE_QUIET=0: unwatched.
    if [ "${GATE_QUIET:-1}" != 0 ] && [ -d /proc ]; then
      # What an earlier block's watcher left suspended (it died) is resumed before this block's
      # state replaces its record; what can't be settled stops the block, never erased.
      if [ -f "$GATE/quiet/frozen" ]; then
        python3 "$tree/scripts/lib/quiet-watch.py" "$GATE/quiet" --resume \
          || { log "block $n: an earlier watcher's suspensions couldn't be resumed ($GATE/quiet/frozen)"; exit 2; }
      fi
      rm -rf "$GATE/quiet" && mkdir -p "$GATE/quiet"
      export NRESE_QUIET_TOKEN="gate-$n-$$"
      python3 "$tree/scripts/lib/quiet-watch.py" "$GATE/quiet" $$ "$NRESE_QUIET_TOKEN" &
      watcher=$!
    fi
    (cd "$tree" && py benches/fast/fast.py block --block "$n" --layout "$layout" --cases "$cases" \
       --out "$OUT/passes" "${sides[@]}" "${opts[@]}" --fresh-copies "$copies") > "$OUT/passes/block-$n.log" 2>&1
    code=$?
    rm -rf "$copies" && trap finish EXIT
    if [ -n "$watcher" ]; then
      kill -TERM "$watcher" 2>/dev/null
      wait "$watcher"
      watched=$?
      cp "$GATE/quiet/watch.log" "$OUT/passes/quiet-block-$n.log" 2>/dev/null
      cp "$GATE/quiet/verdict.json" "$OUT/passes/quiet-block-$n.json" 2>/dev/null
      log "block $n: $(tail -1 "$GATE/quiet/watch.log" 2>/dev/null)"
      # Something the watcher suspended stayed suspended: the block stops, its record kept.
      [ "$watched" = 0 ] || { log "block $n: the watcher left suspensions ($GATE/quiet/frozen)"; exit 1; }
    else
      printf '{"verdict": "unwatched"}\n' > "$OUT/passes/quiet-block-$n.json"
    fi
    tail -1 "$OUT/passes/block-$n.log"
  }
  verdict() {  # the attempt's recorded verdict (quiet, disturbed, unwatched); empty when none is valid
    py -c 'import json, sys
try:
    v = json.load(open(sys.argv[1])).get("verdict")
except Exception:
    v = ""
print(v if v in ("quiet", "disturbed", "unwatched") else "")' "$OUT/passes/quiet-block-$n.json"
  }
  aside() {  # KIND: this block's reports and verdict moved aside, as KIND-* (whatever is still there)
    local stamp f r
    stamp=$(date +%s)
    for f in $(seq "$first" $((first + per - 1))); do
      for r in "$OUT/passes/pass-$f-"*.json; do
        if [ -f "$r" ]; then
          mv "$r" "$OUT/passes/$1-$(basename "$r" .json)-$stamp.json" \
            || { log "block $n: $r couldn't be moved aside: stopped"; exit 2; }
        fi
      done
    done
    for r in "$OUT/passes/quiet-block-$n.json" "$OUT/passes/quiet-block-$n.log"; do
      if [ -f "$r" ]; then
        mv "$r" "${r%.*}-$1-$stamp.${r##*.}" || { log "block $n: $r couldn't be moved aside: stopped"; exit 2; }
      fi
    done
  }
  # A disturbed block (work not the job's own busy too often, quiet-watch.py) is left out of the
  # statistics, both sides (the disturbance is the host's, not a side's: dropping it is unbiased),
  # its reports kept aside as disturbed-*; it runs once more. A repeat disturbed again is kept,
  # and the report says so (the orchestrator's policy, 20 Oct 2026).
  while :; do
    case $stage in
      start | running)
        # The attempt's run, whole: a new one, or one a crash interrupted (resumed, not counted
        # again): what an interrupted run left goes aside first.
        aside interrupted
        set_state "$attempt" running
        attempt
        # The cases' failures are data, the report's to judge; a block without all its reports isn't.
        [ "$(complete)" = "$per" ] || { log "block $n: not all $per reports (exit $code)"; exit 1; }
        [ -n "$(verdict)" ] || { log "block $n: attempt $attempt recorded no verdict"; exit 1; }
        if [ "$attempt" = 2 ]; then
          py -c 'import json, sys
p = sys.argv[1]; v = json.load(open(p)); v["repeat"] = True; json.dump(v, open(p, "w"))' "$OUT/passes/quiet-block-$n.json" \
            || { log "block $n: its repeat's verdict couldn't be marked: stopped"; exit 2; }
        fi
        set_state "$attempt" ran
        stage=ran
        ;;
      ran)
        found=$(verdict)
        if [ -z "$found" ] || [ "$(complete)" != "$per" ]; then
          log "block $n: attempt $attempt has no recorded verdict or not all its reports: run again (the same attempt)"
          stage=running
          continue
        fi
        if [ "$found" = disturbed ] && [ "$attempt" = 1 ]; then
          set_state 1 archiving
          stage=archiving
          continue
        fi
        [ "$found" = disturbed ] && log "block $n: its repeat disturbed too: kept, the report lists it"
        set_state "$attempt" accepted
        log "block $n: done (attempt $attempt, $found, fast.py exit ${code:-before})"
        exit 0
        ;;
      archiving)
        aside disturbed
        log "block $n: disturbed: left out of the statistics (kept as disturbed-*), run once more"
        set_state 2 start
        attempt=2 stage=start
        ;;
    esac
  done
  ;;

report)
  harness=$(git rev-parse "$2") || exit 2
  shift 2
  tree=$(worktree "$harness") || exit 2
  args=() candidate=
  for spec in "$@"; do
    name=${spec%%=*}
    rev=$(git rev-parse "${spec#*=}") || exit 2
    [ -n "$candidate" ] || candidate=$name
    args+=(--side "$name=$rev")
  done
  required=$(printf '%s\n' "$@" | sed 1d | cut -d= -f1 | paste -sd, -)
  for key in not_run elsewhere; do
    listed=$(py -c 'import json,sys; [print(f"{k}={v}") for k, v in json.load(open(sys.argv[1]))[sys.argv[2]].items()]' \
      "$PLAN" "$key")
    while IFS= read -r line; do [ -z "$line" ] || args+=("--${key/_/-}" "$line"); done <<< "$listed"
  done
  (cd "$tree" && py benches/fast/release_gate.py report --passes "$OUT/passes" --candidate "$candidate" "${args[@]}" \
     --require "$required" --harness-revision "$harness" --min-blocks "${GATE_MIN_BLOCKS:-2}" --family "$GATE/plan.json" \
     --inputs "$GATE/inputs.txt" \
     --context "host=${GATE_HOST:-}" --context "round=${GATE_ROUND:-0}" --context "prefix=${GATE_PREFIX:-}" \
     --context "gate_name=${GATE_NAME:-}" --context "max_gb=$MAX_GB" --context "ram_gb=${GATE_RAM_GB:-$MAX_GB}" \
     --context "layouts=$LAYOUTS" \
     --out "$OUT/release-gate.md" --json "$OUT/release-gate.json") > "$OUT/report.log" 2>&1
  code=$?
  log "timed part: $( [ $code -eq 0 ] && echo passed || echo 'not passed') ($OUT/release-gate.md)"
  # The verdict is the gate's; this step only fails when no report could be written.
  [ -f "$OUT/release-gate.json" ] || exit 1
  ;;

w3c)
  export CARGO_TARGET_DIR=${CARGO_TARGET_DIR:-$GATE/target-checks} NRESE_W3C_REQUIRED=1
  mkdir -p "$OUT/w3c"
  bash scripts/fetch-w3c-tests.sh > "$OUT/w3c/fetch.log" 2>&1 || { tail -20 "$OUT/w3c/fetch.log"; exit 1; }
  # The OWL 2 test cases (the DL suites' data), pinned.
  f=.cache/owl-test/all.rdf sum=5383f1ddf4cf2f03703a2f886f41d4e5bc375633a1cfa94a03254fd89330f8bb
  mkdir -p .cache/owl-test
  if ! echo "$sum  $f" | sha256sum -c --quiet - 2>/dev/null; then
    curl -sSfL -o "$f.partial" https://www.w3.org/2009/11/owl-test/all.rdf && echo "$sum  $f.partial" | sha256sum -c --quiet - \
      && mv "$f.partial" "$f" || { log "the OWL 2 test cases don't match their pin"; exit 1; }
  fi
  export NRESE_W3C_OWL_TESTS=$PWD/$f NRESE_W3C_OUT=$OUT/w3c/tableau-w3c.tsv
  for kit in nrese-sparql:w3c_sparql11 nrese-sparql:geosparql_compliance nrese-shacl:w3c_shacl \
             nrese-store:w3c_owl2_rl nrese-dl:tableau_w3c nrese-owl:w3c_owl2 nrese-reasoner:w3c_n3_reasoner; do
    crate=${kit%%:*} test=${kit#*:}
    bash scripts/cargo-guarded.sh test --release --locked -p "$crate" --test "$test" -- --nocapture \
      > "$OUT/w3c/$test.log" 2>&1
    log "w3c $test: $(grep -E '^test result' "$OUT/w3c/$test.log" | tail -1)"
  done
  # A failed kit or a count short of its requirement is the verdict's (w3c.json); this step
  # fails only when the data or the counts couldn't be had.
  py benches/fast/release_gate.py w3c "$OUT/w3c" --expected benches/fast/w3c-expected.toml --json "$OUT/w3c.json" \
    > "$OUT/w3c/counts.txt"
  [ -f "$OUT/w3c.json" ] || exit 1
  log "w3c: $(py -c 'import json,sys; d=json.load(open(sys.argv[1])); print("passed" if d["passed"] else "NOT passed: " + "; ".join(d["errors"]))' "$OUT/w3c.json")"
  ;;

guards)
  export CARGO_TARGET_DIR=${CARGO_TARGET_DIR:-$GATE/target-checks}
  mkdir -p "$OUT/guards"
  py benches/fast/release_gate.py guards --list-names > "$OUT/guards/names.txt" || exit 2
  # shellcheck disable=SC2046
  bash scripts/cargo-guarded.sh test --release --locked --workspace --no-fail-fast --lib --bins --tests -- \
    $(cat "$OUT/guards/names.txt") > "$OUT/guards/test.log" 2>&1
  py benches/fast/release_gate.py guards --log "$OUT/guards/test.log" --json "$OUT/guards.json" | tee -a "$OUT/gate.log"
  [ -f "$OUT/guards.json" ] || exit 1  # the guards' verdict is guards.json's
  ;;

verdict)
  # The timed parts' results directories (the timed host's, the big host's), then the checks'.
  shift
  cd "$launch" || exit 2
  args=()
  while [ $# -gt 1 ]; do
    for f in "$1"/*/release-gate.json "$1"/release-gate.json; do [ -f "$f" ] && args+=(--timed "$f"); done
    shift
  done
  w3c=$(ls "$1"/w3c.json "$1"/*/w3c.json 2>/dev/null | head -1)
  guards=$(ls "$1"/guards.json "$1"/*/guards.json 2>/dev/null | head -1)
  py "$CODE/benches/fast/release_gate.py" verdict "${args[@]}" --w3c "$w3c" --guards "$guards" --out "$OUT/verdict.md"
  ;;

pack)
  # The binaries of every revision, verified, with their modes, for a host that builds nothing.
  shift
  dirs=()
  for spec in "$@"; do
    rev=$(git rev-parse "$spec") || exit 2
    bin=$(bin_of "$rev")
    dirs+=("$(basename "$bin")")
    for L in $(seq 1 "$LAYOUTS"); do
      verified "$(layout_bin "$rev" "$L")" "$rev" "$L" || { log "pack: no verified layout $L of $rev"; exit 2; }
      dirs+=("$(basename "$(layout_bin "$rev" "$L")")")
    done
  done
  mkdir -p "$OUT/binaries"
  # shellcheck disable=SC2046
  tar -C "$GATE_BIN" -cf "$OUT/binaries/gate-bin.tar" $(printf '%s\n' "${dirs[@]}" | sort -u) \
    || { log "pack: tar failed"; exit 1; }
  (cd "$OUT/binaries" && sha256sum gate-bin.tar > gate-bin.tar.sha256) || exit 1
  log "pack: $(printf '%s\n' "${dirs[@]}" | sort -u | tr '\n' ' ')into $OUT/binaries/gate-bin.tar"
  ;;

unpack)
  # The binaries are Linux ELF: on any other system (native Windows: khaleesi, not khaleesi-wsl)
  # nothing is unpacked and the job fails, never runs them some other way.
  [ "$(uname -s)" = Linux ] || { log "unpack: $(uname -s) is not Linux: the gate's binaries can't run here"; exit 2; }
  src=${FLEET_INPUT_BINARIES:?the binaries of the build job, its input named binaries}
  tarball=$(find "$src" -name gate-bin.tar | head -1)
  [ -n "$tarball" ] && (cd "$(dirname "$tarball")" && sha256sum -c --quiet gate-bin.tar.sha256) \
    || { log "unpack: no verified gate-bin.tar in $src"; exit 2; }
  # Into a private directory, then each build published under its identity like a build.
  aside=$GATE_BIN/.unpack-$$
  rm -rf "$aside" && mkdir -p "$aside" && tar -C "$aside" -xpf "$tarball" || { log "unpack: tar failed"; exit 2; }
  for bin in $(tar -tf "$tarball" | cut -d/ -f1 | sort -u); do
    (cd "$aside/$bin" && sha256sum -c --quiet SHA256SUMS) || { log "unpack: $bin doesn't verify"; exit 2; }
    for f in $(cut -d' ' -f3- "$aside/$bin/SHA256SUMS"); do
      [ -x "$aside/$bin/$f" ] || { log "unpack: $bin/$f lost its executable mode"; exit 2; }
      [ "$(head -c4 "$aside/$bin/$f" | od -An -tx1 | tr -d ' ')" = 7f454c46 ] \
        || { log "unpack: $bin/$f is not an ELF binary"; exit 2; }
    done
    grep -q '^linker LLD ' "$aside/$bin/IDENTITY" 2>/dev/null || { log "unpack: $bin has no build identity"; exit 2; }
    ! patch_keyed "$aside/$bin" || { log "unpack: $bin is a build of a perf lab patched before T0: build it again"; exit 2; }
    rev=$(cat "$aside/$bin/REVISION")
    publish "$aside/$bin" "$GATE_BIN/$bin" || { log "unpack: $GATE_BIN/$bin is there but doesn't verify"; exit 2; }
    point "$rev" "$GATE_BIN/$bin" "$(cat "$GATE_BIN/$bin/LAYOUT" 2>/dev/null || echo 1)"
    log "unpack: $bin verified ($rev)"
  done
  rm -rf "$aside"
  ;;

plan-blocks)
  # Each block's plan (release_gate.py blocks): the first blocks the gate's whole plan, each
  # later one the cases whose measured noise needs more passes (benches/fast/measure.toml).
  harness=$(git rev-parse "$2") || exit 2
  tree=$(worktree "$harness") || exit 2
  (cd "$tree" && py benches/fast/release_gate.py blocks --plan "$PLAN" --measure "$MEASURE" --out "$GATE" \
    --square "${GATE_SQUARE:?plan-blocks needs GATE_SQUARE, the timed sides (release_gate.py jobs sets it)}" \
    --min-blocks "${GATE_MIN_BLOCKS:-2}") \
    | tee -a "$OUT/gate.log" || exit 2
  cp "$GATE"/plan-block-*.json "$OUT/"
  ;;

plan-cases)
  # A confirmation runs just its cases, all prepared by the gate's own prepare before.
  [ -n "${GATE_PLAN:-}" ] || { log "plan-cases needs GATE_PLAN"; exit 2; }
  py - "$2" "$GATE/plan.json" "$PLAN" <<'EOF' || exit 2
import json, sys
cases, full, target = sys.argv[1].split(","), json.load(open(sys.argv[2])), sys.argv[3]
unknown = [c for c in cases if c not in full["run"]]
if unknown:
    sys.exit(f"not in the gate's plan (not prepared here): {', '.join(unknown)}")
json.dump({"run": cases, "not_run": {}, "elsewhere": {}}, open(target, "w"), indent=1)
EOF
  cp "$PLAN" "$OUT/"
  log "confirmation round ${GATE_ROUND:-?}: $2"
  ;;

confirm)
  # From the first run's results (and earlier confirmations'), each host's next confirmation.
  shift
  cd "$launch" || exit 2
  reports=() rest=()
  while [ $# -gt 0 ]; do
    case $1 in
      --out|--checkout) rest+=("$1" "$2"); shift 2 ;;
      *) for f in "$1"/release-gate.json "$1"/*/release-gate.json; do [ -f "$f" ] && reports+=("$f"); done; shift ;;
    esac
  done
  [ ${#reports[@]} -gt 0 ] || { echo "no release-gate.json in the given directories" >&2; exit 2; }
  # The jobs' repository: the launching checkout, unless the caller names one (a later --checkout wins).
  py "$CODE/benches/fast/release_gate.py" confirm --checkout "$launch" --reports "${reports[@]}" "${rest[@]}"
  ;;

jobs)
  shift
  cd "$launch" || exit 2
  # As confirm's: the launching checkout, unless the caller names one.
  py "$CODE/benches/fast/release_gate.py" jobs --checkout "$launch" "$@"
  ;;

*)
  # The help: the head comment up to its paragraph after the commands.
  awk 'NR > 1 && /^# One harness/ {exit} NR > 1' "$0"
  exit 2
  ;;
esac
