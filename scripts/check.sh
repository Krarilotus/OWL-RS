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
#                                  branch may hold unchecked commits, and only checked
#                                  trees land (the push tier and the merge check, below).
#        scripts/check.sh push     pre-push hook (.githooks/pre-push): the crates a pushed
#                                  commit changes in the pushed range, on the fleet only
#                                  (scripts/lib/fleet-gate.sh). fmt; clippy on them; all their
#                                  tests, the dependents' fast tests; random tests on new
#                                  seeds; the lock check; nrese-exec's unsafe code under Miri
#                                  and its doc tests when it changed (scripts/miri.sh). By
#                                  hand: HEAD, from its upstream (else from where it left
#                                  origin's refactor/engine-v2), so a push then finds the pass.
#        scripts/check.sh windows [COMMIT]
#                                  the merge check's Windows part, on the fleet only
#                                  (scripts/lib/fleet-gate.sh windows): COMMIT (default HEAD)
#                                  built whole, all its tests and doc tests, the suite
#                                  contracts, by its own tier here, which its own
#                                  scripts/lib/windows-job.sh runs on a Windows host. A commit
#                                  whose inputs (scripts/lib/gate-key.sh windows_inputs)
#                                  passed before carries that pass: "carried from <commit>".
#        scripts/check.sh [all]    milestones and CI: everything, both Cargo workspaces,
#                                  doc tests, cargo-deny, the console.
#
# Tests run under cargo-nextest when it is installed (every test of every binary in
# parallel; `cargo install --locked cargo-nextest`), else under cargo test, crate by
# crate. Builds go through scripts/cargo-guarded.sh. Every step runs; the summary lists
# each one's exit code.
#
# Exit: 0 passed, 1 failed; 75 no verdict: the fleet gave none (push, windows), or the PC's
# guard held or stopped a build (scripts/cargo-guarded.sh's 75), which isn't the tree's
# failure; 2 a usage error, or nothing to check.
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
nextest() { cargo nextest --version > /dev/null 2>&1; }
# Python under either name: Windows has `python`, some Linux hosts only `python3`. A missing
# interpreter fails the gate; it must never shrink the gate to nothing.
python() {
  if type -P python > /dev/null; then command python "$@"
  elif type -P python3 > /dev/null; then command python3 "$@"
  else echo "the gate needs python (or python3)" >&2; return 127; fi
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

# The workspace crates depending on any of the given ones (directly or not, dev-
# dependencies included: their tests must still compile), the given ones excluded.
dependents_of() {
  local name
  declare -A given=()
  for name in "$@"; do given[$name]=1; done
  for name in "$@"; do
    cargo tree -i "$name" --workspace --prefix none -e normal,build,dev --offline 2>/dev/null \
      | sed 's/ (\*)//; s/ .*//'
  done | sort -u | while read -r dep; do
    [ -n "$dep" ] && [ -z "${given[$dep]:-}" ] && echo "$dep"
  done
}

package_args() { local name; for name in "$@"; do printf -- '-p %s ' "$name"; done; }

# The workspace's packages, by name.
workspace_packages() {
  cargo metadata --no-deps --format-version 1 --offline \
    | python -c "import json,sys; print('\n'.join(p['name'] for p in json.load(sys.stdin)['packages']))" \
    | tr -d '\r'
}

# Tests of `all` crates, with the slow binaries of `fast_only` left out. Python's output is
# read without carriage returns: Windows writes them, and cargo rejects names with them.
run_tests() {
  local -a all=() fast_only=()
  local into=all arg
  for arg in "$@"; do
    if [ "$arg" = -- ]; then into=fast_only; continue; fi
    if [ $into = all ]; then all+=("$arg"); else fast_only+=("$arg"); fi
  done
  [ ${#all[@]} -eq 0 ] && return 0
  if nextest; then
    local filter=""
    [ ${#fast_only[@]} -gt 0 ] && filter=$(python scripts/lib/test-targets.py exclude "${fast_only[@]}" | tr -d '\r')
    # shellcheck disable=SC2046
    "$guarded" nextest run --locked --no-fail-fast $(package_args "${all[@]}") \
      ${filter:+-E "$filter"}
  else
    local code=0 line
    declare -A fast=()
    for arg in "${fast_only[@]}"; do fast[$arg]=1; done
    for arg in "${all[@]}"; do
      if [ -n "${fast[$arg]:-}" ]; then
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

# The random tests of the given crates (scripts/lib/fuzz-targets.txt) with a few more seeds
# than the one they run with otherwise: seeds drawn from the commit, so each push tries new
# ones and a failure replays (NRESE_FUZZ_SEED=<seed>). The fuzz campaign runs many more.
run_seeds() {
  local -A wanted=()
  local -a crates=() exprs=()
  local name crate target filter seed binary code=0
  for name in "$@"; do wanted[$name]=1; done
  local base=$(( 16#$(git rev-parse --short=6 HEAD) % 100000 + 1000 ))
  while IFS=: read -r crate target filter; do
    [ -n "${wanted[$crate]:-}" ] || continue
    crates+=("$crate")
    binary=$crate
    [ "$target" = lib ] || binary="$crate::$target"
    exprs+=("(binary_id($binary)${filter:+ & test($filter)})")
  done < <(grep -v '^#' scripts/lib/fuzz-targets.txt | tr -d '\r' | grep .)
  [ ${#exprs[@]} -gt 0 ] || return 0
  local expr
  expr=$(IFS='|'; echo "${exprs[*]}")
  for seed in $base $((base + 1)) $((base + 2)); do
    if nextest; then
      # One run per seed: every random test of every changed crate in parallel.
      # shellcheck disable=SC2046
      local out
      if ! out=$(NRESE_FUZZ_SEED=$seed "$guarded" nextest run --locked --no-fail-fast \
          $(package_args "${crates[@]}") -E "$expr" 2>&1); then
        echo "random tests fail with NRESE_FUZZ_SEED=$seed:"
        printf '%s\n' "$out" | grep -E '^ +FAIL|panicked at' -A1 | grep -v '^--' | sort -u | head -20
        code=1
      fi
      continue
    fi
    while IFS=: read -r crate target filter; do
      [ -n "${wanted[$crate]:-}" ] || continue
      local kind=(--test "$target")
      [ "$target" = lib ] && kind=(--lib)
      if ! NRESE_FUZZ_SEED=$seed "$guarded" test --locked -q -p "$crate" "${kind[@]}" -- $filter > /dev/null 2>&1; then
        echo "random tests fail: NRESE_FUZZ_SEED=$seed cargo test -p $crate ${kind[*]} -- $filter"
        code=1
      fi
    done < <(grep -v '^#' scripts/lib/fuzz-targets.txt | tr -d '\r' | grep .)
  done
  return $code
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

# The steps for files outside the Rust workspace.
side_steps() {
  local files=("$@")
  if printf '%s\n' "${files[@]}" | grep -qE '(^|/)Cargo\.(toml|lock)$'; then
    step locks bash scripts/check-locks.sh
  fi
  if printf '%s\n' "${files[@]}" | grep -qE '^benches/suite/'; then
    step "suite contracts" python benches/suite/tests/isolation.py discover -s benches/suite/tests
  fi
  # Docker names only from the job namespace (scripts/check-names.sh).
  if printf '%s\n' "${files[@]}" | grep -qE '^(benches|scripts)/'; then
    step names bash scripts/check-names.sh
  fi
  if printf '%s\n' "${files[@]}" | grep -qE '^benches/nrese-bench-harness/'; then
    harness
  fi
  if printf '%s\n' "${files[@]}" | grep -qE '^apps/nrese-console/'; then
    console
  fi
}
harness() {
  local manifest=benches/nrese-bench-harness/Cargo.toml
  step "fmt (bench harness)" cargo fmt --manifest-path $manifest --all --check
  step "clippy (bench harness)" "$guarded" clippy --locked --manifest-path $manifest \
    --all-targets
  step "test (bench harness)" "$guarded" test --locked --manifest-path $manifest --no-fail-fast
}
console() {
  if [ -d apps/nrese-console/node_modules ]; then
    step "console typecheck" npm --prefix apps/nrese-console run typecheck
    step "console tests" npx --prefix apps/nrese-console vitest run --root apps/nrese-console
  else
    echo "=== the console's dependencies aren't installed (npm ci in apps/nrese-console): skipped"
  fi
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
# fails: an empty list would gate nothing.

changed_files() {
  if [ "$1" = commit ] && git rev-parse -q --verify MERGE_HEAD > /dev/null; then
    comm -12 <(git diff --name-only --cached HEAD | sort) <(git diff --name-only --cached MERGE_HEAD | sort)
  elif [ "$1" = commit ]; then
    git diff --name-only HEAD
  else
    [ -n "${push_base:-}" ] && [ -n "${push_commit:-}" ] || return 2
    git diff --name-only "$push_base" "$push_commit"
    return
  fi
  git ls-files --others --exclude-standard
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
  # A measurement commit (refs/measure/*, benches/PROTOCOL.md §5) is a commit on an old side's tree,
  # which this tier isn't for: its one check is the measurement rule against the declared sides, the
  # side on a branch of the remote the push goes to (NRESE_PUSH_REMOTE, from the hook).
  if [[ ${NRESE_PUSH_REF:-} == refs/measure/* ]]; then
    python -B benches/fast/release_gate.py measurement-ref "$NRESE_PUSH_REF" "$push_commit" \
      --remote "${NRESE_PUSH_REMOTE:-}"
    exit $?
  fi
fi
# The Windows part checks one commit, whole: COMMIT, else HEAD, by its own tier, so a commit from
# before the tier can't be checked.
if [ "$mode" = windows ] && [ -z "${NRESE_ON_BUILD_HOST:-}" ]; then
  push_commit=$(git rev-parse -q --verify "${2:-HEAD}^{commit}") || { echo "no commit ${2:-HEAD} to check" >&2; exit 2; }
  git cat-file -e "$push_commit:scripts/lib/windows-job.ps1" 2> /dev/null \
    || { echo "$(git rev-parse --short "$push_commit") has no Windows tier (scripts/lib/windows-job.ps1): it can't be checked" >&2; exit 2; }
fi

# The push tier and the Windows part run on the fleet only (scripts/lib/fleet-gate.sh), for the
# commit's own tree and the pushed range's files (Windows: none, everything runs). A pass is a
# verified fleet result, remembered by content (scripts/lib/gate-key.sh: the tier, the tree, the
# files, the toolchain), and only check.sh writes it. No verdict, no pass (no push): nothing runs
# here instead.
if { [ "$mode" = windows ] || [ "$mode" = push ]; } && [ -z "${NRESE_ON_BUILD_HOST:-}" ]; then
  # shellcheck source=lib/gate-key.sh
  . scripts/lib/gate-key.sh
  list=$(mktemp)
  where=$(mktemp)
  trap 'rm -f "$list" "$where"' EXIT
  if [ "$mode" = push ]; then
    changed_files push > "$list" || { echo "the push range $push_base..$push_commit isn't all here: fetch, then push" >&2; exit 2; }
  fi
  tree=$(git rev-parse "$push_commit^{tree}")
  list_hash=$(git hash-object "$list")
  identity=$(gate_identity) || { echo "the gate can't name its toolchain and flags (above): no $mode pass" >&2; exit 2; }
  # The Windows part's pass belongs to the commit's inputs, not its tree: a later commit with the
  # same inputs carries it (scripts/lib/gate-key.sh windows_inputs), and its record names the commit.
  keyed=$tree on=""
  if [ "$mode" = windows ]; then
    keyed=inputs-$(windows_inputs "$push_commit") || { echo "no inputs of $push_commit to key its pass" >&2; exit 2; }
    on="$push_commit "
  fi
  key=$(gate_key "$mode" "$keyed" "$list_hash" "$identity")
  if record=$(gate_pass "$key"); then
    if [ "$mode" = windows ]; then
      echo "=== windows: $(git rev-parse --short "$push_commit") carried from $record (the same inputs)"
    else
      echo "=== $mode: $(git rev-parse --short "$push_commit") passed on the fleet before (its tree, these files)"
    fi
    exit 0
  fi
  bash scripts/lib/fleet-gate.sh "$mode" "$list" "$tree" "$list_hash" "$identity" "$where" "$push_commit"
  case $? in
    0) gate_record "$key" "$on$(cat "$where")"; exit 0 ;;
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
    # the tests run on the fleet before anything lands: the push tier for the pushed commit's own
    # tree, the merge check's Windows part for the merged one (process review 20 Oct 2026, §3A).
    mapfile -t files < <(changed_files commit)
    step fmt cargo fmt --all --check
    if printf '%s\n' "${files[@]}" | grep -qE '(^|/)Cargo\.(toml|lock)$'; then
      step locks bash scripts/check-locks.sh
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
    mapfile -t changed < <(crates_of "${files[@]}")
    mapfile -t dependents < <([ ${#changed[@]} -gt 0 ] && dependents_of "${changed[@]}")
    echo "=== changed: ${changed[*]:-none}; depending on them: ${dependents[*]:-none}"
    step fmt cargo fmt --all --check
    if [ ${#changed[@]} -gt 0 ]; then
      # Lints where code changed: every target of the changed crates. A dependent's code didn't
      # change, so its lints can't either; what can break there is compiling against the new
      # API, and the dependents' tests are built and run, which checks every target of theirs.
      # shellcheck disable=SC2046
      step clippy "$guarded" clippy --locked $(package_args "${changed[@]}") --all-targets
      w3c_data
      step test run_tests "${changed[@]}" "${dependents[@]}" -- "${dependents[@]}"
      step seeds run_seeds "${changed[@]}"
      step locks bash scripts/check-locks.sh
    fi
    # nrese-exec's unsafe primitives under Miri (scripts/miri.sh), and its doc tests: the
    # compile-fail rows that keep their misuse unwritable (a settled region grows).
    if exec_changed; then
      step miri bash scripts/miri.sh
      step "doc tests (nrese-exec)" "$guarded" test --locked --doc -p nrese-exec
    fi
    side_steps "${files[@]}"
    ;;
  windows)
    # Inside its fleet job only, which readies the Windows host (scripts/lib/windows-job.sh).
    [ -n "${FLEET_OUT:-}" ] || { echo "check.sh windows runs inside its fleet job only (scripts/lib/windows-job.sh): not run" >&2; exit 2; }
    mapfile -t everything < <(workspace_packages)
    [ ${#everything[@]} -gt 0 ] || { echo "no workspace packages found" >&2; exit 2; }
    w3c_data
    step build "$guarded" build --locked --workspace --all-targets
    step test run_tests "${everything[@]}"
    step "doc tests" "$guarded" test --locked --workspace --doc --no-fail-fast
    step "suite contracts" python benches/suite/tests/isolation.py discover -s benches/suite/tests
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
    step "suite contracts" python benches/suite/tests/isolation.py discover -s benches/suite/tests
    step names bash scripts/check-names.sh
    if cargo deny --version > /dev/null 2>&1; then
      step deny cargo deny --all-features check
      step "deny (bench harness)" cargo deny --manifest-path benches/nrese-bench-harness/Cargo.toml \
        --config deny.toml --all-features check
    else
      echo "=== cargo-deny isn't installed (cargo install --locked cargo-deny): skipped"
    fi
    console
    ;;
  *)
    echo "usage: scripts/check.sh commit|push|windows [COMMIT]|all" >&2
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
