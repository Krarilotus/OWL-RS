#!/usr/bin/env bash
# The gate, in three tiers. A test runs where it matters: each crate owns its tests, so a
# change runs the tests of the crates it touches, and the crates that depend on them run
# their fast tests (push). Slow suites (conformance, fuzz campaigns) are declared by their
# crate (`[package.metadata.nrese] slow-tests`) and run when that crate changed, or at a
# milestone. Benchmarks never run here.
#
# Usage: scripts/check.sh commit   pre-commit hook, here, in seconds: fmt; for the files
#                                  changed against HEAD (staged or not), their lock files
#                                  and Docker names. Nothing is built, linted or tested: a
#                                  branch may hold unchecked commits; a tree lands only
#                                  when its push tier passed for that exact tree, and its
#                                  Windows inputs passed the Windows check (below).
#        scripts/check.sh push     pre-push hook (.githooks/pre-push): the crates a pushed
#                                  commit changes in the pushed range (every crate when the
#                                  root manifest, lock file or toolchain changed:
#                                  changed_crates, below), on the fleet only
#                                  (scripts/lib/fleet-gate.sh). fmt; clippy on them; all their
#                                  tests, the dependents' fast tests; random tests on new
#                                  seeds; the lock check; each of them and their dependents
#                                  compiled alone (crates_alone); nrese-exec's unsafe code under Miri
#                                  and its doc tests when it changed (scripts/miri.sh); the
#                                  suite contracts the files reach, by what each declares it
#                                  reads (benches/suite/tests/contracts.py), the crate scan its
#                                  own step. By
#                                  hand: HEAD, from its upstream (else from where it left
#                                  origin's refactor/engine-v2), so a push then finds the pass;
#                                  NRESE_PUSH_REF names the ref it will go to, as the hook's
#                                  does (a merge check: refs/heads/refactor/engine-v2, or a
#                                  release branch refs/heads/release/<name>, which makes its
#                                  job a merge gate on the fleet). A value the
#                                  caller's environment already holds is that caller's word.
#        scripts/check.sh quick    before a review, in minutes, on the fleet only
#                                  (scripts/lib/fleet-gate.sh quick): one commit (NRESE_PUSH_COMMIT,
#                                  else HEAD) on its branch's files, from where it left origin's
#                                  refactor/engine-v2 (else NRESE_PUSH_BASE), the crates by
#                                  changed_crates as push. The push tier's own steps, fewer: fmt;
#                                  clippy on them; their unit tests and probes (their `it` binary's
#                                  probes:: module); the generated-views drift tests, always; one
#                                  seed of their random tests (push's first); the state explorer's
#                                  targets (scripts/lib/explorer-targets.txt); the guards that need
#                                  no build; the suite contracts by push's rule. Its pass is its own tier's (scripts/lib/gate-key.sh):
#                                  it never stands for a push pass, lands nothing, and says nothing
#                                  of the crates depending on them.
#        scripts/check.sh windows [COMMIT]
#                                  the merge check's Windows part, on the fleet only
#                                  (scripts/lib/fleet-gate.sh windows): COMMIT (default HEAD)
#                                  built whole, clippy on it all, all its tests and doc tests,
#                                  the suite contracts its range reaches (from NRESE_PUSH_BASE,
#                                  else from where it left origin's refactor/engine-v2; all of
#                                  them without one: windows_contracts), the console, by its own
#                                  tier here, which its own scripts/lib/windows-job.sh runs on a
#                                  Windows host. A commit whose inputs (scripts/lib/gate-key.sh
#                                  windows_inputs) passed before with the same contracts or all
#                                  of them, under the Python runtime it declares (gate_runtime),
#                                  carries that pass: "carried from <commit>".
#                                  NRESE_PUSH_REF as for push: the merge check's Windows part
#                                  for refactor/engine-v2 or a release branch is a merge gate,
#                                  a package's isn't.
#        scripts/check.sh nightly [COMMIT]
#                                  on the line's head after changes land, on the fleet only (scripts/lib/fleet-gate.sh
#                                  nightly): COMMIT (default HEAD) with every test in its full
#                                  form (NRESE_CHECK_TIER=nightly), the forms the other tiers
#                                  cut down, and a receipt of one outcome per test of the
#                                  tree's inventory (scripts/lib/nightly-receipt.py). Its steps
#                                  are listed, each with what it guards, where the tier runs
#                                  (below). A red nightly run on the line stays an open
#                                  blocker: no release is cut while one is open. The release
#                                  gate is to run these forms itself on the commit it
#                                  releases, never resting on another commit's nightly result
#                                  (a requirement it doesn't meet yet). Its pass is its own tier's
#                                  (scripts/lib/gate-key.sh): it never stands for a push pass,
#                                  nor a push pass for it, and it lands nothing. Beside it, a job of its
#                                  own runs `nightly-outside` (below): the outside reasoners' check,
#                                  which needs Docker; its verdict is its own job's.
#        scripts/check.sh [all]    milestones and CI: everything, both Cargo workspaces,
#                                  doc tests, cargo-deny, the console.
#        scripts/check.sh crates FILE...
#                                  the workspace crates the push tier lints and tests for
#                                  these changed files, one per line; nothing else runs.
#
# Tests run under cargo-nextest (every test of every binary in parallel, with the features of
# the crates the run selects): the fleet's gate jobs install the pinned one
# (scripts/lib/gate-tools.txt; scripts/lib/gate-job.sh, scripts/lib/windows-job.sh), and the
# gate jobs give no verdict without it (NRESE_NEXTEST_REQUIRED). A run without it elsewhere
# uses cargo test, crate by crate. So the push tier checks that each crate it tests compiles
# alone, with its own features (crates_alone).
# Builds go through scripts/cargo-guarded.sh. Every step runs; the summary lists
# each one's exit code.
#
# Exit: 0 passed, 1 failed; 75 no verdict: the fleet gave none (push, windows), or the PC's
# guard held or stopped a build (scripts/cargo-guarded.sh's 75), which isn't the tree's
# failure; 2 a usage error, or nothing to check; 3 no verdict: cargo didn't run the required
# cargo-nextest.
#
# The W3C suites run with their test data required (NRESE_W3C_REQUIRED=1, the data fetched
# first by scripts/fetch-w3c-tests.sh): a suite without its data fails instead of passing by
# skipping. NRESE_W3C_OPTIONAL=1 opts out for a local run without the data; such a pass isn't
# remembered as the tier's.
set -u
cd "$(dirname "$0")/.."
mode=${1:-all}
# Warnings fail the gate through Cargo (build.warnings), not through a lint flag: a flag
# changes the build's fingerprint, so the gate and plain clippy runs would not share
# artifacts.
export CARGO_BUILD_WARNINGS=deny
[ "$mode" = --changed ] && mode=commit
guarded=scripts/cargo-guarded.sh
declare -a results=()
step() {
  local name=$1
  shift
  echo "=== $name"
  local start=$SECONDS
  "$@"
  local code=$?
  results+=("$code  $name ($((SECONDS - start)) s)")
  echo "=== $name: exit $code"
}
# Where the run's job requires a version (NRESE_NEXTEST_REQUIRED, scripts/lib/gate-job.sh and
# scripts/lib/windows-job.sh), cargo
# must run exactly that one, asked right before the tests; any other, or none, ends the run with 3, no
# verdict: never cargo test or another runner in its place. Elsewhere (a developer's run) any
# cargo-nextest whose version command succeeds is used.
nextest() {
  local got
  [ -n "${NRESE_NEXTEST_REQUIRED:-}" ] || { cargo nextest --version > /dev/null 2>&1; return; }
  got=$(cargo nextest --version 2> /dev/null) || got=""
  [ "$(printf '%s\n' "$got" | tr -d '\r' | awk 'NR == 1 { print $2 }')" = "$NRESE_NEXTEST_REQUIRED" ] && return 0
  echo "cargo-nextest $NRESE_NEXTEST_REQUIRED is required here; cargo runs ${got:-none}: no verdict" >&2
  exit 3
}
# Python under either name: Windows has `python`, some Linux hosts only `python3`. A missing
# interpreter fails the gate; it must never shrink the gate to nothing.
python() {
  if type -P python > /dev/null; then command python "$@"
  elif type -P python3 > /dev/null; then command python3 "$@"
  else echo "the gate needs python (or python3)" >&2; return 127; fi
}

# The workspace crates a change reaches, which the push tier lints and tests
# (benches/suite/tests/test_changed_crates.py). A file under a crate's directory (crates/) reaches
# that crate. The root manifest (its [workspace.package], [workspace.dependencies], profiles and
# lints), the root lock file, the toolchain file, Cargo's build configuration and clippy's
# configuration reach every crate of the workspace: a change to one of them is every crate
# changed. (D16-637: a version bump changed only such files, and the push tier printed "changed:
# none" and ran no crate's clippy or tests.) The lock files of the other Cargo projects (fuzz/,
# benches/*) pin only those projects' builds. It stays in this file: check.sh needs no other file
# to start, and the suite's fixtures copy it alone or stub the scripts it names.
EVERY_CRATE='^(Cargo\.toml|Cargo\.lock|rust-toolchain(\.toml)?|\.cargo/config(\.toml)?|clippy\.toml)$'

changed_crates() {
  if printf '%s\n' "$@" | grep -qE "$EVERY_CRATE"; then
    workspace_packages
  else
    crates_of "$@"
  fi
}

# Whether none of the files can reach a workspace crate in changed_crates (nothing under crates/,
# no workspace-wide file): added to a push tier's files, they leave its workspace builds, lints and
# tests as they were (one cargo run over the same crates; cargo resolves features across the crates
# a run selects, so a run over more crates isn't the same check of each) and only add steps of
# their own (side_steps). A pass covers a narrower push of its commit only across such files
# (scripts/lib/gate-key.sh gate_covering; benches/suite/tests/test_push_tier_monotone.py).
reaches_no_crate() { ! printf '%s\n' "$@" | grep -qE "$EVERY_CRATE|^crates/"; }

# Whether COMMIT's own check.sh, which its fleet gate runs, draws its seeds from the full commit id
# (run_seeds), so its pass names the same random cases on every host. Its pass may cover a narrower
# push; a commit whose runner abbreviates the id keeps exact reuse only.
stable_seeds_at() {
  git show "$1:scripts/check.sh" 2> /dev/null | grep -qE '^[[:space:]]+local base=\$\(\( 16#\$\{head:0:6\} '
}

# The workspace crates (directories with a Cargo.toml under crates/) that `files` touch.
crates_of() {
  local file dir name
  declare -A seen=()
  for file in "$@"; do
    dir=$(dirname "$file")
    while [ "$dir" != . ] && [ "$dir" != / ]; do
      if [ -f "$dir/Cargo.toml" ] && [[ "$dir" == crates/* ]]; then
        name=$(sed -n 's/^name = "\(.*\)"/\1/p' "$dir/Cargo.toml" | head -1)
        [ -n "$name" ] && [ -z "${seen[$name]:-}" ] && { seen[$name]=1; echo "$name"; }
        break
      fi
      dir=$(dirname "$dir")
    done
  done
}

# The workspace's packages, by name.
workspace_packages() {
  cargo metadata --no-deps --format-version 1 --offline \
    | python -c "import json,sys; print('\n'.join(p['name'] for p in json.load(sys.stdin)['packages']))" \
    | tr -d '\r'
}

if [ "$mode" = crates ]; then shift; changed_crates "$@"; exit; fi

# The workspace crates depending on any of the given ones (directly or not, dev-
# dependencies included: their tests must still compile), the given ones excluded. Prints every one
# found, then fails (2) where cargo couldn't tell for one of them: never "none" for a lookup that
# failed (quick fails on it; push runs the ones found).
dependents_of() {
  local name found="" tree code=0
  declare -A given=()
  for name in "$@"; do given[$name]=1; done
  for name in "$@"; do
    if tree=$(cargo tree -i "$name" --workspace --prefix none -e normal,build,dev --offline 2>/dev/null); then
      found+=$tree$'\n'
    else
      echo "cargo tree can't tell what depends on $name" >&2
      code=2
    fi
  done
  printf '%s' "$found" | sed 's/ (\*)//; s/ .*//' | sort -u | while read -r dep; do
    [ -n "$dep" ] && [ -z "${given[$dep]:-}" ] && echo "$dep"
  done
  return $code
}

package_args() { local name; for name in "$@"; do printf -- '-p %s ' "$name"; done; }

# Clippy on every target of the given crates: the push and quick tiers' one line. A dependent's code
# didn't change, so its lints can't either.
lint() {
  # shellcheck disable=SC2046
  "$guarded" clippy --locked $(package_args "$@") --all-targets
}

# Tests of `all` crates, with the slow binaries of `fast_only` left out. Python's output is
# read without carriage returns: Windows writes them, and cargo rejects names with them.
# The quick tier's narrower runs, named first: `--lib`, the crates' unit tests alone; `--in TARGET
# FILTER`, the tests of their test binary TARGET whose names hold FILTER; `--empty pass|fail`, what
# a run that selects no test is, the same under nextest (--no-tests) and cargo test.
run_tests() {
  local -a all=() fast_only=() only=()
  local into=all arg target="" name="" empty=""
  while :; do
    case ${1:-} in
      --lib) only=(--lib); shift ;;
      --in) target=$2 name=$3; shift 3 ;;
      --empty) empty=$2; shift 2 ;;
      *) break ;;
    esac
  done
  for arg in "$@"; do
    if [ "$arg" = -- ]; then into=fast_only; continue; fi
    if [ $into = all ]; then all+=("$arg"); else fast_only+=("$arg"); fi
  done
  [ ${#all[@]} -eq 0 ] && return 0
  if nextest; then
    local filter=""
    [ ${#fast_only[@]} -gt 0 ] && filter=$(python scripts/lib/test-targets.py exclude "${fast_only[@]}" | tr -d '\r')
    if [ -n "$target" ]; then
      local -a binaries=()
      for arg in "${all[@]}"; do binaries+=("binary_id($arg::$target)"); done
      filter="($(IFS='|'; echo "${binaries[*]}")) & test($name)"
    fi
    # shellcheck disable=SC2046
    "$guarded" nextest run --locked --no-fail-fast ${empty:+--no-tests=$empty} $(package_args "${all[@]}") \
      "${only[@]}" ${filter:+-E "$filter"}
  else
    local code=0 line
    declare -A fast=()
    for arg in "${fast_only[@]}"; do fast[$arg]=1; done
    for arg in "${all[@]}"; do
      if [ -n "$target" ]; then
        cargo_test "$empty" --locked --no-fail-fast -p "$arg" --test "$target" -- "$name" || code=$?
      elif [ ${#only[@]} -gt 0 ]; then
        cargo_test "$empty" --locked --no-fail-fast -p "$arg" "${only[@]}" || code=$?
      elif [ -n "${fast[$arg]:-}" ]; then
        line=$(python scripts/lib/test-targets.py fast "$arg") || return 2
        line=$(printf '%s' "$line" | tr -d '\r')
        [ -n "$line" ] || continue
        # shellcheck disable=SC2086
        "$guarded" test --locked --no-fail-fast $line || code=$?
      else
        "$guarded" test --locked --no-fail-fast -p "$arg" --lib --bins --tests || code=$?
      fi
    done
    return $code
  fi
}

# The nightly tier's steps (below), under nextest only: its receipt (scripts/lib/nightly-receipt.py)
# needs nextest's inventory and reports. Each run uses nextest's `nightly` profile (.config/nextest.toml:
# no retries, no fail-fast, a JUnit report), with the check tier NRESE_CHECK_TIER=nightly, which gives
# every test its full form; the inventory, the receipt and the reports go to $receipts.
#
# The inventory, pinned before any filter: every test of the given crates, #[ignore]d ones marked.
inventory() {
  nextest || { echo "the nightly tier runs under cargo-nextest only (its inventory and reports)" >&2; return 2; }
  # shellcheck disable=SC2046
  "$guarded" nextest list --locked --ignore-default-filter --run-ignored all --message-format json \
    $(package_args "$@") > "$receipts/inventory.json"
}
# STEP CMD...: CMD in the nightly profile and tier, its JUnit report kept as $receipts/STEP.junit.xml.
reported() {
  local name=$1 junit code
  shift
  # nextest's store is the workspace's target/nextest, wherever CARGO_TARGET_DIR puts the builds.
  junit=$(cargo metadata --format-version 1 --no-deps --offline \
    | python -c "import json,sys; print(json.load(sys.stdin)['workspace_root'])" | tr -d '\r') || return 2
  junit=$junit/target/nextest/nightly/junit.xml
  # An earlier step's report left there would stand for this step's.
  rm -f -- "$junit" && [ ! -e "$junit" ] || { echo "$name: the earlier report $junit can't be removed" >&2; return 2; }
  (export NEXTEST_PROFILE=nightly NRESE_CHECK_TIER=nightly; "$@")
  code=$?
  cp -- "$junit" "$receipts/$name.junit.xml" || { [ $code -ne 0 ] || code=2; }
  return $code
}
# CRATE TARGET NAME: the #[ignore]d test NAME (its path in the binary) of CRATE's test binary TARGET,
# alone; a run that selects no test fails.
ignored_test() {
  "$guarded" nextest run --locked --no-fail-fast --no-tests=fail --run-ignored only -p "$1" \
    -E "binary_id($1::$2) & test(=$3)"
}
# The known open faults (scripts/lib/known-faults.tsv), alone: each listed #[ignore]d test by its exact
# name. Its exit is 0 or nextest's 100 (tests ran, some failed): the receipt judges each outcome. Anything
# else (no build, no test selected) fails the step.
known_faults() {
  local filter code=0
  filter=$(awk -F'\t' '!/^#/ && NF { split($1, t, " "); f = f (f ? " | " : "") "(binary_id(" t[1] ") & test(=" t[2] "))" }
    END { print f }' scripts/lib/known-faults.tsv) || return 2
  "$guarded" nextest run --locked --no-fail-fast --no-tests=fail --run-ignored only --workspace -E "$filter" || code=$?
  [ $code -eq 0 ] || [ $code -eq 100 ]
}
# NAME FIRST CASES: benches/reasoning/dl/outside.sh over seeds FIRST.. (CASES of them) into $out/NAME,
# its exit kept in $out/NAME/exit for the counts (scripts/lib/outside-counts.py), which judge it. Within
# what is left of the outside check's 30 minutes ($outside_by): the run, its 20 s kill grace and the
# removal of the containers it leaves (labelled with the job, removed by that label alone, 25 s each,
# and 5 s more to kill, for the listing and the removal) all end 10 s before it. No time left is exit
# 124, as a timeout's.
outside_block() {
  local left=$((outside_by - SECONDS)) code=0 left_over
  mkdir -p "$out/$1"
  if [ $left -gt 120 ]; then
    NRESE_FUZZ_SEED=$2 NRESE_FUZZ_CASES=$3 NRESE_DL_WORKERS=3 DL_OUTSIDE_LABEL=fleet.job=${FLEET_JOB:-none} \
      timeout --kill-after=20 $((left - 90)) sh benches/reasoning/dl/outside.sh "$out/$1" || code=$?
  else
    code=124
  fi
  echo "$code" > "$out/$1/exit"
  [ $code -ne 124 ] && [ $code -ne 137 ] || echo "$1: over the outside check's 30 minutes" >&2
  [ ! -f "$out/$1/counts.txt" ] || sed 's/^/  /' "$out/$1/counts.txt"
  left_over=$(timeout -k 5 25 docker ps -aq --filter "label=fleet.job=${FLEET_JOB:-none}")
  # shellcheck disable=SC2086
  [ -z "$left_over" ] || { echo "$1: removing its containers left: $left_over"; timeout -k 5 25 docker rm -f $left_over > /dev/null; }
  return 0
}
# STEP=SELECTION...: the receipt of the run (scripts/lib/nightly-receipt.py), with the gate's own
# fields: the commit, its tree, tracked changes, the build identity (scripts/lib/gate-key.sh), the host,
# the job and its attempt (scripts/lib/gate-job.sh).
receipt() {
  # shellcheck source=lib/gate-key.sh
  . scripts/lib/gate-key.sh
  RECEIPT_COMMIT=$(git rev-parse HEAD) RECEIPT_TREE=$(git rev-parse 'HEAD^{tree}') \
    RECEIPT_DIRTY=$(git status --porcelain --untracked-files=no) RECEIPT_IDENTITY=$(gate_identity HEAD) \
    RECEIPT_HOST=${FLEET_HOST:-$(hostname)} RECEIPT_JOB=${FLEET_JOB:-} \
    RECEIPT_ATTEMPT=$(cat "${FLEET_OUT:-/nonexistent}/attempt" 2> /dev/null) \
    python scripts/lib/nightly-receipt.py "$receipts" "$@"
}

# EMPTY ARGS...: cargo test ARGS; with EMPTY fail, a run in which no test ran fails (4, as nextest's).
cargo_test() {
  local empty=$1 out code=0
  shift
  [ "$empty" = fail ] || { "$guarded" test "$@"; return; }
  out=$("$guarded" test "$@" 2>&1) || code=$?
  printf '%s\n' "$out"
  [ $code -ne 0 ] || grep -qE '^test result: [A-Za-z]+\. [1-9]' <<< "$out" \
    || { echo "no test ran: cargo test $*" >&2; code=4; }
  return $code
}

# The random tests of the given crates (scripts/lib/fuzz-targets.txt) with a few more seeds
# than the one they run with otherwise: COUNT of them (push 3; quick 1, the first of push's), drawn
# from the commit, so each push tries new ones and a failure replays (NRESE_FUZZ_SEED=<seed>). The
# fuzz campaign runs many more.
run_seeds() {
  local count=$1 n seeds="" head
  shift
  # The first six hex digits of the full id: an abbreviation (--short) grows where another object
  # shares the prefix, so the same commit would draw other seeds on another host (stable_seeds_at
  # reads this line).
  head=$(git rev-parse HEAD) || return 2
  local base=$(( 16#${head:0:6} % 100000 + 1000 ))
  for ((n = 0; n < count; n++)); do seeds+=" $((base + n))"; done
  run_targets "random tests" scripts/lib/fuzz-targets.txt "$seeds" "$@"
}

# The tests a target list names (LIST: crate:target:filter, target `lib` for the unit tests) of the
# given crates: once for each seed of SEEDS, or once as they are for none. WHAT names them in a failure.
run_targets() {
  local what=$1 list=$2 seeds=$3
  shift 3
  local -A wanted=()
  local -a crates=() targets=() filters=() exprs=()
  local name crate target filter seed binary code=0 expr out i how content
  # A list that can't be read fails: it never selects nothing.
  content=$(cat -- "$list") || { echo "$what: can't read the list $list" >&2; return 2; }
  for name in "$@"; do wanted[$name]=1; done
  while IFS=: read -r crate target filter; do
    [ -n "${wanted[$crate]:-}" ] || continue
    crates+=("$crate") targets+=("$target") filters+=("$filter")
    binary=$crate
    [ "$target" = lib ] || binary="$crate::$target"
    exprs+=("(binary_id($binary)${filter:+ & test($filter)})")
  done < <(printf '%s\n' "$content" | grep -v '^#' | tr -d '\r' | grep .)
  [ ${#exprs[@]} -gt 0 ] || return 0
  expr=$(IFS='|'; echo "${exprs[*]}")
  for seed in ${seeds:-none}; do
    local -a with=()
    how=""
    [ "$seed" = none ] || { with=("NRESE_FUZZ_SEED=$seed"); how="NRESE_FUZZ_SEED=$seed "; }
    if nextest; then
      # One run per seed: every listed test of every given crate in parallel.
      # shellcheck disable=SC2046
      if ! out=$(env "${with[@]}" "$guarded" nextest run --locked --no-fail-fast \
          $(package_args "${crates[@]}") -E "$expr" 2>&1); then
        echo "$what fail${how:+ with ${how% }}:"
        printf '%s\n' "$out" | grep -E '^ +FAIL|panicked at' -A1 | grep -v '^--' | sort -u | head -20
        code=1
      fi
      continue
    fi
    for i in "${!crates[@]}"; do
      local kind=(--test "${targets[i]}")
      [ "${targets[i]}" = lib ] && kind=(--lib)
      # shellcheck disable=SC2086
      if ! env "${with[@]}" "$guarded" test --locked -q -p "${crates[i]}" "${kind[@]}" -- ${filters[i]} > /dev/null 2>&1; then
        echo "$what fail: ${how}cargo test -p ${crates[i]} ${kind[*]} -- ${filters[i]}"
        code=1
      fi
    done
  done
  return $code
}

# The crates whose state-explorer targets (scripts/lib/explorer-targets.txt) a change of the given
# crates runs: each listed crate that is one of them or depends on one (dependents_of). Fails (2)
# where the list can't be read or the dependents can't be told.
explorer_crates() {
  local list=scripts/lib/explorer-targets.txt dependents content
  local -a listed=()
  content=$(cat -- "$list") || { echo "can't read the list $list" >&2; return 2; }
  mapfile -t listed < <(printf '%s\n' "$content" | grep -v '^#' | tr -d '\r' | grep . | cut -d: -f1 | sort -u)
  [ ${#listed[@]} -gt 0 ] || return 0
  dependents=$(dependents_of "$@") || return 2
  printf '%s\n' "$@" "$dependents" | sort -u | grep -Fx -f <(printf '%s\n' "${listed[@]}")
  return 0
}

# The given crates with probes: a `probes` module in their `it` test binary (tests/it/probes/ or
# tests/it/probes.rs), where a reviewer's probe that found a fault is filed as the crate's test. A
# crate at any depth under crates/, as crates_of finds them.
probe_crates() {
  local manifest dir name
  while IFS= read -r manifest; do
    dir=${manifest%/Cargo.toml}
    [ -d "$dir/tests/it/probes" ] || [ -f "$dir/tests/it/probes.rs" ] || continue
    name=$(sed -n 's/^name = "\(.*\)"/\1/p' "$manifest" | head -1)
    printf '%s\n' "$@" | grep -qFx -- "$name" && echo "$name"
  done < <(find crates -name Cargo.toml)
  return 0
}

# The W3C suites' data, required unless NRESE_W3C_OPTIONAL (the release gate's first W3C part,
# 13 Oct 2026, skipped every suite for want of it and passed).
w3c_data() {
  if [ -n "${NRESE_W3C_OPTIONAL:-}" ]; then
    unset NRESE_W3C_REQUIRED
    echo "=== W3C suites optional (NRESE_W3C_OPTIONAL): a suite without its data skips"
  else
    export NRESE_W3C_REQUIRED=1
    step "W3C data" bash scripts/fetch-w3c-tests.sh
  fi
}

# A private bench host's names (the overlay NRESE_BENCH_PRIVATE names, benches/hosts.toml's header) are
# refused where they would be published: by the commit tier for the staged change, and on the pusher's machine
# for the commits a push publishes, before the fleet's tier, whose remembered pass knows neither the overlay
# nor the messages (scripts/lib/private_terms.py). With no overlay there is nothing to check and nothing is read.
private_terms() {
  if [ -z "${NRESE_BENCH_PRIVATE:-}" ]; then
    echo "private terms: no overlay configured (NRESE_BENCH_PRIVATE), nothing to check"
    return 0
  fi
  python scripts/lib/private_terms.py "$@"
}

# The steps for files outside the Rust workspace: the guards, then what tests them.
side_steps() {
  local files=("$@")
  guards "${files[@]}"
  # The suite contracts these files reach, by what each declares it reads (the gate's own scripts
  # and hooks too: test_push_gate, test_full_gate, test_commit_tier, ...).
  local -a reached
  mapfile -t reached < <(printf '%s\n' "${files[@]}" | reached_contracts)
  suite_contracts "${reached[@]}"
  if printf '%s\n' "${files[@]}" | grep -qE '^benches/nrese-bench-harness/'; then
    harness
  fi
  if printf '%s\n' "${files[@]}" | grep -qE '^apps/nrese-console/'; then
    console
  fi
}
# The suite contracts of the given module names, the crate scan (crate_scan) its own step: every
# contract when none is given, none for `none` (benches/suite/tests/contracts.py select's answers).
suite_contracts() {
  local name
  local -a rest=()
  if [ $# -eq 0 ]; then
    step "crate scan" python benches/suite/tests/isolation.py crate_scan
    step "suite contracts" python benches/suite/tests/isolation.py discover -s benches/suite/tests
    return
  fi
  for name in "$@"; do
    case $name in
      none) ;;
      crate_scan) step "crate scan" python benches/suite/tests/isolation.py crate_scan ;;
      *) rest+=("$name") ;;
    esac
  done
  [ ${#rest[@]} -eq 0 ] || step "suite contracts" python benches/suite/tests/isolation.py "${rest[@]}"
}
# The suite contracts the files on stdin (one per line) reach, one per line, by what each declares it
# reads (benches/suite/tests/contracts.py; with COMMIT, COMMIT's own declarations): nothing for every
# contract, which is also the answer where the selector fails.
reached_contracts() {
  local out
  out=$(python benches/suite/tests/contracts.py select ${1:+--commit "$1"}) || return 0
  printf '%s\n' "$out" | tr -d '\r' | grep .
  return 0
}
# The suite contracts the Windows part of COMMIT runs: those its range reaches, by COMMIT's own
# selector and declarations, from NRESE_PUSH_BASE, else from where it left origin's
# refactor/engine-v2 (as push). Nothing, so every contract, without a base, where COMMIT has no
# selector, or where the range or the selector fails.
windows_contracts() {
  local commit=$1 base files selector out
  base=${NRESE_PUSH_BASE:-$(git merge-base "$commit" refs/remotes/origin/refactor/engine-v2 2> /dev/null)}
  [ -n "$base" ] && files=$(gate_files diff --no-renames --name-only "$base" "$commit" 2> /dev/null) || return 0
  selector=$(mktemp) || return 0
  if git show "$commit:benches/suite/tests/contracts.py" > "$selector" 2> /dev/null \
      && out=$(printf '%s\n' "$files" | python "$selector" select --commit "$commit"); then
    printf '%s\n' "$out" | tr -d '\r' | grep .
  fi
  rm -f -- "$selector"
  return 0
}
# Of those, the guards that build nothing (the quick tier's too).
guards() {
  local files=("$@")
  if printf '%s\n' "${files[@]}" | grep -qE '^(crates/nrese-(server/src/|store/src/mutation/)|scripts/(check|check-effect)\.sh$)'; then
    step "effectful work" bash scripts/check-effect.sh
  fi
  if printf '%s\n' "${files[@]}" | grep -qE '(^|/)Cargo\.(toml|lock)$'; then
    step locks bash scripts/check-locks.sh
    # No test-only feature in a product build (scripts/check-features.sh, DUR-4).
    step features bash scripts/check-features.sh
  fi
  # Names the changed Python can't resolve, which a test replacing a whole function never runs
  # (scripts/check-python-names.py).
  local py
  mapfile -t py < <(printf '%s\n' "${files[@]}" | grep -E '^(benches|scripts)/.*\.py$')
  if [ ${#py[@]} -gt 0 ]; then
    step "python names" python scripts/check-python-names.py "${py[@]}"
  fi
  # Docker names only from the job namespace (scripts/check-names.sh).
  if printf '%s\n' "${files[@]}" | grep -qE '^(benches|scripts)/'; then
    step names bash scripts/check-names.sh
  fi
}
# No run of spaces inside a product string: a string continued on the next line without its `\`
# keeps that line's indentation, and the message reaches its reader with it.
message_strings() { ! git grep -n -E "^\s*\"[^\"]*[a-z,;:)] {6,}[a-z(]" -- "crates/*/src/*.rs" ":!*tests*"; }
harness() {
  local manifest=benches/nrese-bench-harness/Cargo.toml
  step "fmt (bench harness)" cargo fmt --manifest-path $manifest --all --check
  step "clippy (bench harness)" "$guarded" clippy --locked --manifest-path $manifest \
    --all-targets
  step "test (bench harness)" "$guarded" test --locked --manifest-path $manifest --no-fail-fast
}
# The console's typecheck and tests, wherever the tier runs. The dependencies come from the
# console's lockfile first, every time (`npm ci` replaces node_modules: the Linux gate's worktree
# keeps an earlier tree's), and a host that can't install them fails that step by
# name; the console is never passed unchecked (benches/suite/tests/test_console_step.py).
console() {
  step "console dependencies" npm --prefix apps/nrese-console ci --no-audit --no-fund
  [ "${results[-1]%% *}" = 0 ] || return 0
  step "console typecheck" npm --prefix apps/nrese-console run typecheck
  step "console tests" npx --prefix apps/nrese-console vitest run --root apps/nrese-console
}

# A full tier uses the declared cargo-deny release; installing it belongs to prerequisites.
deny_version() {
  local want got
  want=$(sed -n 's/^cargo-deny //p' scripts/lib/gate-tools.txt | tr -d '\r')
  got=$(cargo deny --version 2> /dev/null) || {
    echo "cargo-deny unavailable; required ${want:-missing pin} (scripts/lib/gate-tools.txt)" >&2
    return 1
  }
  got=$(printf '%s\n' "$got" | awk 'NR == 1 { print $2 }' | tr -d '\r')
  if [ -z "$want" ] || [ "$got" != "$want" ]; then
    echo "cargo-deny version ${got:-unknown}; required ${want:-missing pin} (scripts/lib/gate-tools.txt)" >&2
    return 1
  fi
}

# Each crate compiled alone, with only the features it and its dev-dependencies turn on: a crate's
# tests run in one run with other crates' (nextest), which unifies features, so a crate that compiles
# only because another turns a feature on would pass there and fail `cargo test -p` for its developer.
crates_alone() {
  local name code=0 got
  for name in "$@"; do
    "$guarded" check --locked -p "$name" --lib --bins --tests && continue
    got=$?
    # The guard's 75 (no verdict) stands over a later crate's failure, as in the summary.
    [ $code -eq 75 ] || code=$got
  done
  return $code
}

# Whether nrese-exec differs from where this branch left refactor/engine-v2, committed or
# not (Miri's step): the upstream or the last commit alone would miss earlier commits of a
# branch that has none. Without that history, it is taken to differ.
exec_changed() {
  local base
  base=$(git merge-base HEAD origin/refactor/engine-v2 2>/dev/null) || return 0
  ! git diff --quiet "$base" -- crates/nrese-exec ||
    [ -n "$(git ls-files --others --exclude-standard -- crates/nrese-exec)" ]
}

# The files a commit or push changes. A merge commit: a file the same as on one side was
# tested on that side; new is what differs from both (resolved conflicts, files both sides
# changed). A push: the pushed range, from the base the remote has to the pushed commit
# (push_base, push_commit); never the work tree, which a push doesn't send. Without a range it
# fails: an empty list would gate nothing. A rename is listed by both its names (what was read
# from where it left changed too), and a path as it is, not quoted (gate_files).

gate_files() { git -c core.quotePath=false "$@"; }
changed_files() {
  if [ "$1" = commit ] && git rev-parse -q --verify MERGE_HEAD > /dev/null; then
    comm -12 <(gate_files diff --no-renames --name-only --cached HEAD | sort) \
      <(gate_files diff --no-renames --name-only --cached MERGE_HEAD | sort)
  elif [ "$1" = commit ]; then
    gate_files diff --no-renames --name-only HEAD
  else
    [ -n "${push_base:-}" ] && [ -n "${push_commit:-}" ] || return 2
    gate_files diff --no-renames -O/dev/null --name-only "$push_base" "$push_commit"  # git's order, as the host's
    return
  fi
  gate_files ls-files --others --exclude-standard
}

# A push gates one commit, from the base the remote has: .githooks/pre-push names both
# (NRESE_PUSH_COMMIT, NRESE_PUSH_BASE); by hand, HEAD from its upstream, else from where it left
# origin's refactor/engine-v2. Never the last commit alone.
if [ "$mode" = push ] && [ -z "${NRESE_ON_BUILD_HOST:-}" ]; then
  push_commit=$(git rev-parse -q --verify "${NRESE_PUSH_COMMIT:-HEAD}^{commit}") \
    || { echo "no commit ${NRESE_PUSH_COMMIT:-HEAD} to gate" >&2; exit 2; }
  push_base=${NRESE_PUSH_BASE:-$(git rev-parse -q --verify '@{upstream}' 2> /dev/null \
    || git merge-base "$push_commit" refs/remotes/origin/refactor/engine-v2 2> /dev/null)}
  [ -n "$push_base" ] || { echo "no base for the push range: no upstream, and no origin/refactor/engine-v2" >&2; exit 2; }
  # What this push publishes, checked here on every push before the fleet's tier, whose remembered pass
  # knows neither the overlay nor the messages (private_terms): the range's commits and the branch.
  private_terms push "$push_base" "$push_commit" "${NRESE_PUSH_REF:-}" || exit $?
  # A measurement commit (refs/measure/*, benches/PROTOCOL.md §5) is a commit on an old side's tree,
  # which this tier isn't for: its one check is the measurement rule against the declared sides, the
  # side on a branch of the remote the push goes to (NRESE_PUSH_REMOTE, from the hook).
  if [[ ${NRESE_PUSH_REF:-} == refs/measure/* ]]; then
    python -B benches/fast/release_gate.py measurement-ref "$NRESE_PUSH_REF" "$push_commit" \
      --remote "${NRESE_PUSH_REMOTE:-}"
    exit $?
  fi
fi
# The quick check checks one commit too (NRESE_PUSH_COMMIT, else HEAD), on its branch's files: from
# NRESE_PUSH_BASE, else from where it left origin's refactor/engine-v2, never from its upstream: a
# review reads the branch's earlier commits too, pushed or not.
if [ "$mode" = quick ] && [ -z "${NRESE_ON_BUILD_HOST:-}" ]; then
  push_commit=$(git rev-parse -q --verify "${NRESE_PUSH_COMMIT:-HEAD}^{commit}") \
    || { echo "no commit ${NRESE_PUSH_COMMIT:-HEAD} to check" >&2; exit 2; }
  push_base=${NRESE_PUSH_BASE:-$(git merge-base "$push_commit" refs/remotes/origin/refactor/engine-v2 2> /dev/null)}
  [ -n "$push_base" ] || { echo "no base for the branch: no origin/refactor/engine-v2 (fetch it)" >&2; exit 2; }
  # Its job runs the commit's own gate-job.sh: one from before its quick mode (its entries under the
  # cache's quick/, out of every older sweep) would run push's in its place, or have them swept.
  git show "$push_commit:scripts/lib/gate-job.sh" 2> /dev/null | grep -qF 'home=$cache/quick' \
    || { echo "$(git rev-parse --short "$push_commit") has no quick check (scripts/lib/gate-job.sh): it can't be checked" >&2
         exit 2; }
fi
# The Windows part checks one commit, whole: COMMIT, else HEAD, by its own tier, so a commit from
# before the tier can't be checked.
if [ "$mode" = windows ] && [ -z "${NRESE_ON_BUILD_HOST:-}" ]; then
  push_commit=$(git rev-parse -q --verify "${2:-HEAD}^{commit}") || { echo "no commit ${2:-HEAD} to check" >&2; exit 2; }
  git cat-file -e "$push_commit:scripts/lib/windows-job.ps1" 2> /dev/null \
    || { echo "$(git rev-parse --short "$push_commit") has no Windows tier (scripts/lib/windows-job.ps1): it can't be checked" >&2; exit 2; }
fi
# The nightly run's outside check runs in its own fleet job only (scripts/lib/fleet-gate.sh nightly).
if [ "$mode" = nightly-outside ] && [ -z "${NRESE_ON_BUILD_HOST:-}" ]; then
  echo "nightly-outside runs in its fleet job only: scripts/check.sh nightly submits it" >&2
  exit 2
fi
# The nightly run checks one commit, whole, by its own check.sh, so a commit from before the tier
# can't be checked.
if [ "$mode" = nightly ] && [ -z "${NRESE_ON_BUILD_HOST:-}" ]; then
  push_commit=$(git rev-parse -q --verify "${2:-HEAD}^{commit}") || { echo "no commit ${2:-HEAD} to check" >&2; exit 2; }
  git show "$push_commit:scripts/check.sh" 2> /dev/null | grep -qx '  nightly)' \
    || { echo "$(git rev-parse --short "$push_commit") has no nightly tier (scripts/check.sh): it can't be checked" >&2; exit 2; }
fi

# The push tier, the quick check, the Windows part and the nightly run run on the fleet only
# (scripts/lib/fleet-gate.sh), for the commit's own tree and the range's files (Windows: the suite
# contracts it runs, none listed, all, and everything else runs; nightly: none, everything runs, in
# its full form). A pass is a verified fleet result, remembered by content
# (scripts/lib/gate-key.sh: the tier, the tree, the files, the toolchain), and only check.sh writes
# it. No verdict, no pass (no push): nothing runs here instead.
if [[ $mode =~ ^(windows|push|quick|nightly)$ ]] && [ -z "${NRESE_ON_BUILD_HOST:-}" ]; then
  # shellcheck source=lib/gate-key.sh
  . scripts/lib/gate-key.sh
  list=$(mktemp)
  where=$(mktemp)
  trap 'rm -f "$list" "$where"' EXIT
  if [ "$mode" = push ] || [ "$mode" = quick ]; then
    changed_files push > "$list" || { echo "the range $push_base..$push_commit isn't all here: fetch first" >&2; exit 2; }
  elif [ "$mode" = windows ]; then
    # The Windows part's list is the suite contracts it runs (windows_contracts): none listed, all.
    # The nightly run's stays empty: nothing selects what it runs.
    windows_contracts "$push_commit" > "$list"
  fi
  tree=$(git rev-parse "$push_commit^{tree}")
  list_hash=$(git hash-object "$list")
  identity=$(gate_identity "$push_commit") || { echo "the gate can't name its toolchain and flags (above): no $mode pass" >&2; exit 2; }
  # The Windows part's pass belongs to the commit's inputs, not its tree: a later commit with the
  # same inputs carries it (scripts/lib/gate-key.sh windows_inputs). Either record names the commit
  # first.
  keyed=$tree on="$push_commit "
  if [ "$mode" = windows ]; then
    keyed=inputs-$(windows_inputs "$push_commit") || { echo "no inputs of $push_commit to key its pass" >&2; exit 2; }
    # And the Python runtime the commit declares for the suite: a pass under another, or one from
    # before the tier declared one, is never carried (scripts/lib/gate-key.sh gate_runtime).
    runtime=$(gate_runtime "$push_commit") \
      || { echo "$(git rev-parse --short "$push_commit") declares no Python runtime for the suite" \
             "(scripts/lib/gate-tools.txt python-windows): it can't be checked" >&2; exit 2; }
    identity="$identity $(gate_python "$runtime")"
  fi
  key=$(gate_key "$mode" "$keyed" "$list_hash" "$identity")
  # A Windows pass that ran every suite contract (its list empty) stands for any selection of them.
  if record=$(gate_pass "$key") || { [ "$mode" = windows ] && [ -s "$list" ] \
      && record=$(gate_pass "$(gate_key "$mode" "$keyed" "$(git hash-object --stdin < /dev/null)" "$identity")"); }; then
    if [ "$mode" = windows ]; then
      echo "=== windows: $(git rev-parse --short "$push_commit") carried from $record (the same inputs)"
    else
      echo "=== $mode: $(git rev-parse --short "$push_commit") passed on the fleet before (its tree, these files)"
    fi
    exit 0
  fi
  # A pass of this commit on more files covers these when its other files reach no crate: its
  # workspace builds, lints and tests were these files' own, and the other files only added steps
  # (scripts/lib/gate-key.sh gate_covering, reaches_no_crate above), and its runner's seeds are the
  # same on every host (stable_seeds_at).
  if [ "$mode" = push ] && stable_seeds_at "$push_commit" && record=$(gate_covering "$mode" "$keyed" "$list" "$identity" "$push_commit" reaches_no_crate); then
    echo "=== $mode: $(git rev-parse --short "$push_commit") passed on the fleet before (this commit, these files and others reaching no crate)"
    exit 0
  fi
  bash scripts/lib/fleet-gate.sh "$mode" "$list" "$tree" "$list_hash" "$identity" "$where" "$push_commit" "${push_base:-}"
  case $? in
    0) [ "$mode" != push ] || gate_list_keep "$list" "$list_hash"
       gate_record "$key" "$on$(cat "$where")"; exit 0 ;;
    1) exit 1 ;;
    *) echo "no fleet verdict for $(git rev-parse --short "$push_commit") (tree $tree): no $mode pass; the $mode" \
         "tier runs on the fleet only" >&2
       exit 75 ;;
  esac
fi

# The commit tier runs here, always: it builds nothing, so no build host or guard applies to it.
# For the other tiers this machine selects the changed files. With a build host
# (scripts/lib/remote.sh), the host runs every step on a snapshot of this working tree; without
# one, or with NRESE_BUILD_HOST=local, this script runs them here. Either way a passed tier is
# remembered.
if [ "$mode" != commit ] && [ -z "${NRESE_ON_BUILD_HOST:-}" ]; then
  build_host=${NRESE_BUILD_HOST:-$(git config --get nrese.buildHost)}
  # A machine's guard marks it busy (low memory or disk); then a local build goes to the build host.
  if [ "$build_host" = local ] && [ -f "${NRESE_GUARD_BUSY:-$HOME/owlrs-guard/BUSY}" ]; then
    echo "this machine is busy ($(cat "${NRESE_GUARD_BUSY:-$HOME/owlrs-guard/BUSY}")); building on the build host" >&2
    build_host=$(git config --get nrese.buildHost)
  fi
  list=$(mktemp)
  [ "$mode" = all ] || changed_files "$mode" > "$list"
  # A tier that passed for this exact tree and file list doesn't run again. So the gate can
  # run before `git push`, whose hook then needn't hold the remote's connection open for it.
  key="$mode${NRESE_W3C_OPTIONAL:+ w3c-optional} $(bash scripts/lib/remote.sh tree) $(git hash-object "$list")"
  passed="$(git rev-parse --git-common-dir)/nrese-gate-passed"
  if grep -qxF "$key" "$passed" 2> /dev/null; then
    echo "=== $mode: passed before for this tree and these files"
    rm -f "$list"
    exit 0
  fi
  if [ -n "$build_host" ] && [ "$build_host" != local ]; then
    bash scripts/lib/remote.sh check "$mode" "$list"
  else
    NRESE_ON_BUILD_HOST=1 NRESE_FILES_FROM="$list" bash scripts/check.sh "$mode"
  fi
  code=$?
  [ $code -eq 0 ] && echo "$key" >> "$passed"
  rm -f "$list"
  exit $code
fi

case "$mode" in
  commit)
    # Checks of a few seconds, with no build: rustfmt, and for the files changed against HEAD
    # their lock files, Docker names and the bench harness's format. The compile, the lints and
    # the tests run on the fleet before anything lands: the push tier for the pushed commit's exact
    # tree, the merge check's Windows part for its Windows inputs (scripts/lib/gate-key.sh
    # windows_inputs; process review 20 Oct 2026, §3A).
    mapfile -t files < <(changed_files commit)
    step "private terms" private_terms commit
    # A refusal ends the tier: a later step's output (rustfmt's diff) could show the line it refused.
    [ "${results[-1]%% *}" = 0 ] || exit "${results[-1]%% *}"
    step fmt cargo fmt --all --check
    step "message strings" message_strings
    if printf '%s\n' "${files[@]}" | grep -qE '(^|/)Cargo\.(toml|lock)$'; then
      step locks bash scripts/check-locks.sh
      step features bash scripts/check-features.sh
    fi
    # Docker names only from the job namespace (scripts/check-names.sh).
    if printf '%s\n' "${files[@]}" | grep -qE '^(benches|scripts)/'; then
      step names bash scripts/check-names.sh
    fi
    if printf '%s\n' "${files[@]}" | grep -qE '^benches/nrese-bench-harness/'; then
      step "fmt (bench harness)" cargo fmt --manifest-path benches/nrese-bench-harness/Cargo.toml --all --check
    fi
    ;;
  push)
    # A push tier on a build host runs the list its gate sent (gate-job.sh, remote.sh), never one
    # it selects itself: here it has no pushed range.
    if [ -z "${NRESE_FILES_FROM:-}" ]; then
      echo "check.sh push on a build host needs the gate's file list (NRESE_FILES_FROM): not run" >&2
      exit 2
    fi
    mapfile -t files < "$NRESE_FILES_FROM"
    mapfile -t changed < <(changed_crates "${files[@]}")
    mapfile -t dependents < <([ ${#changed[@]} -gt 0 ] && dependents_of "${changed[@]}")
    echo "=== changed: ${changed[*]:-none}; depending on them: ${dependents[*]:-none}"
    step fmt cargo fmt --all --check
    if [ ${#changed[@]} -gt 0 ]; then
      # Lints where code changed: every target of the changed crates. What can break in a
      # dependent is compiling against the new API, and the dependents' tests are built and run,
      # which checks every target of theirs.
      step clippy lint "${changed[@]}"
      w3c_data
      step test run_tests "${changed[@]}" "${dependents[@]}" -- "${dependents[@]}"
      step seeds run_seeds 3 "${changed[@]}"
      step locks bash scripts/check-locks.sh
      step features bash scripts/check-features.sh
      step "crates alone" crates_alone "${changed[@]}" "${dependents[@]}"
    fi
    # nrese-exec's unsafe primitives under Miri (scripts/miri.sh), and its doc tests: the
    # compile-fail rows that keep their misuse unwritable (a settled region grows).
    if exec_changed; then
      step miri bash scripts/miri.sh
      step "doc tests (nrese-exec)" "$guarded" test --locked --doc -p nrese-exec
    fi
    side_steps "${files[@]}"
    ;;
  quick)
    # The push tier's own steps on the gate's file list, fewer (the header): what finds a mechanical
    # fault of the changed crates in minutes. Not the dependents' tests, the changed crates' other
    # integration tests, Miri, the W3C suites, the bench harness or the console. The suite contracts
    # the files reach, by push's rule (reached_contracts): a change to the gate's own scripts is checked
    # before review.
    if [ -z "${NRESE_FILES_FROM:-}" ]; then
      echo "check.sh quick on a build host needs the gate's file list (NRESE_FILES_FROM): not run" >&2
      exit 2
    fi
    mapfile -t files < "$NRESE_FILES_FROM"
    mapfile -t changed < <(changed_crates "${files[@]}")
    echo "=== changed: ${changed[*]:-none}"
    step fmt cargo fmt --all --check
    step "message strings" message_strings
    # A crate without unit tests or probes passes those steps (nothing selected); a build or test
    # failure fails them.
    if [ ${#changed[@]} -gt 0 ]; then
      step clippy lint "${changed[@]}"
      step "unit tests" run_tests --empty pass --lib "${changed[@]}"
      mapfile -t probed < <(probe_crates "${changed[@]}")
      [ ${#probed[@]} -eq 0 ] || step probes run_tests --empty pass --in it probes:: "${probed[@]}"
    fi
    # The committed generated views against the code that generates them (nrese-server's
    # generated_views_tests.rs): always, as a changed document can drift as well as the code; and
    # failing where that test is gone.
    step "generated views" run_tests --empty fail --in it generated_views nrese-server
    if [ ${#changed[@]} -gt 0 ]; then
      step seeds run_seeds 1 "${changed[@]}"
      if ! found=$(explorer_crates "${changed[@]}"); then
        step explorer false  # its targets couldn't be told (above): never a pass that ran none
      elif [ -n "$found" ]; then
        mapfile -t explored <<< "$found"
        step explorer run_targets "explorer tests" scripts/lib/explorer-targets.txt "" "${explored[@]}"
      fi
      # Push's checks that build no test, on every crate change as push has them: the lock files, the
      # test-only features, and each changed crate compiled on its own (push's dependents are built by
      # its test step, which quick doesn't run).
      step locks bash scripts/check-locks.sh
      step features bash scripts/check-features.sh
      step "crates alone" crates_alone "${changed[@]}"
    fi
    guards "${files[@]}"
    mapfile -t reached < <(printf '%s\n' "${files[@]}" | reached_contracts)
    suite_contracts "${reached[@]}"
    ;;
  windows)
    # Inside its fleet job only, which readies the Windows host (scripts/lib/windows-job.sh).
    [ -n "${FLEET_OUT:-}" ] || { echo "check.sh windows runs inside its fleet job only (scripts/lib/windows-job.sh): not run" >&2; exit 2; }
    mapfile -t everything < <(workspace_packages)
    [ ${#everything[@]} -gt 0 ] || { echo "no workspace packages found" >&2; exit 2; }
    w3c_data
    step build "$guarded" build --locked --workspace --all-targets
    # Lints of the code only Windows compiles (cfg(windows)), which no Linux tier sees: the push
    # tier lints the changed crates on Linux alone (PR #68).
    step clippy "$guarded" clippy --locked --workspace --all-targets
    step test run_tests "${everything[@]}"
    step "doc tests" "$guarded" test --locked --workspace --doc --no-fail-fast
    # The suite contracts its range reaches, as its gate sent them (NRESE_CONTRACTS, from
    # windows_contracts; scripts/lib/gate-key.sh gate_contracts_list); none sent: all of them.
    # shellcheck source=lib/gate-key.sh
    . scripts/lib/gate-key.sh
    mapfile -t reached < <(gate_contracts_list)
    suite_contracts "${reached[@]}"
    # The console's typecheck and tests, with the gate's own Node (scripts/lib/windows-job.sh).
    console
    ;;
  nightly)
    # Inside its fleet job (scripts/lib/fleet-gate.sh nightly): the runs too long for a landing, one
    # job for all of them, under nextest (the helpers above). Each step guards:
    # - inventory: every workspace test, #[ignore]d ones marked, listed before anything is filtered:
    #   what the receipt holds each step to.
    # - test (full forms): every workspace crate's tests not #[ignore]d, unselected, each in its full
    #   form (NRESE_CHECK_TIER=nightly): the cases, sweeps and sizes the default forms cut down, so a
    #   fault that shows only beyond them is still found. No retries and no fail-fast, so the log names
    #   every failing test. The W3C data is required, as for push.
    # - count past 64 bits: a COUNT past 2^64 (the n³ statements of 2.7 M equal identities) fails as
    #   the engine's range limit and never answers a wrapped number (nrese-sparql's
    #   a_count_past_64_bits_fails_as_a_range_limit, #[ignore]d for its size: alone, by its exact name).
    # - known open faults: the tests of scripts/lib/known-faults.tsv, each #[ignore]d and failing while
    #   its fault is there: alone, by exact name; none listed, no step.
    # - receipt: one outcome per test of the inventory, each step's selection whole: no test it
    #   selected without an outcome, none with two, none outside the inventory; a known open fault
    #   that fails counted and named apart, one that passes red.
    # A later full run (the status sweep's, the whole suite contracts') is a step here, not a job, with
    # its selection in the receipt.
    mapfile -t everything < <(workspace_packages)
    [ ${#everything[@]} -gt 0 ] || { echo "no workspace packages found" >&2; exit 2; }
    receipts=${FLEET_OUT:-$(mktemp -d)}/receipt
    mkdir -p "$receipts" || exit 2
    w3c_data
    step inventory inventory "${everything[@]}"
    step "test (full forms)" reported full-forms run_tests "${everything[@]}"
    count=equality_answer_tests::a_count_past_64_bits_fails_as_a_range_limit
    step "count past 64 bits" reported count ignored_test nrese-sparql it "$count"
    known=()
    if grep -q '^[^#]' scripts/lib/known-faults.tsv; then
      step "known open faults" reported known known_faults
      known=(known=known)
    fi
    step receipt receipt full-forms=not-ignored "count=exact:nrese-sparql::it/$count" "${known[@]}"
    ;;
  nightly-outside)
    # The nightly run's outside check, a fleet job of its own beside it (scripts/lib/fleet-gate.sh
    # nightly submits it; its references run in Docker, which the gate's job never reaches): the store's
    # owl2-dl answers against HermiT and Openllet (benches/reasoning/dl/outside.sh, its planted faults
    # first) on generated OWL 2 DL, in two blocks, each run to its end; the last step judges both:
    # - outside (fixed seeds): seeds 0..1999, the same every night.
    # - outside (fresh seeds): 1,000 more, from the commit and the day, clear of 0..4999.
    # - outside counts (scripts/lib/outside-counts.py): each block's cases compared and dropped by reason.
    #   It fails on a block whose run didn't end cleanly (a seed missing, a planted fault missed, a crash,
    #   a timeout); on a fixed seed's difference both references hold against the store; and on an empty
    #   run, fewer than half the fixed block's cases compared. A fresh seed's difference is listed for a
    #   reader (outside/outside.json, new_disagreements), never a failure. Left out of the comparison,
    #   counted: a key over a property that isn't simple (outside the references' OWL 2 DL), cases where
    #   the references split, on a row or a key control (neither is an oracle then), or give no answer,
    #   writes the store declines as undecided.
    # Budget: 20 minutes, a cold build included (3,000 cases took 7 min, the release build 6); both blocks
    # end within 30 minutes of the first, their cleanup included, or fail.
    out=${FLEET_OUT:-$(mktemp -d)}/outside
    mkdir -p "$out" || exit 2
    outside_by=$((SECONDS + 1800))
    first=$(printf '%s %s' "$(git rev-parse HEAD)" "$(date -u +%F)" | sha256sum | cut -c1-6)
    first=$((5000 + 1000 * (16#$first % 1000000)))
    step "outside (fixed seeds)" outside_block fixed 0 2000
    step "outside (fresh seeds)" outside_block fresh "$first" 1000
    step "outside counts" python scripts/lib/outside-counts.py "$out" 2000 "$first" 1000
    ;;
  all)
    step fmt cargo fmt --all --check
    step clippy "$guarded" clippy --locked --workspace --all-targets
    mapfile -t everything < <(workspace_packages)
    [ ${#everything[@]} -gt 0 ] || { echo "no workspace packages found" >&2; exit 2; }
    w3c_data
    step test run_tests "${everything[@]}"
    step "doc tests" "$guarded" test --locked --workspace --doc
    harness
    step locks bash scripts/check-locks.sh
    step features bash scripts/check-features.sh
    suite_contracts
    step names bash scripts/check-names.sh
    step "cargo-deny version" deny_version
    step deny cargo deny --all-features check
    step "deny (bench harness)" cargo deny --manifest-path benches/nrese-bench-harness/Cargo.toml \
      --config deny.toml --all-features check
    console
    ;;
  *)
    echo "usage: scripts/check.sh commit|push|quick|windows [COMMIT]|nightly [COMMIT]|nightly-outside|all" >&2
    exit 2
    ;;
esac

echo
echo "=== summary"
failed=0
for result in "${results[@]}"; do
  echo "  $result"
  case ${result%% *} in
    0) ;;
    75) failed=75 ;;
    *) [ $failed -eq 75 ] || failed=1 ;;
  esac
done
[ $failed -ne 75 ] || echo "the PC's guard held or stopped a build (exit 75): no verdict, not the tree's failure"
exit $failed
