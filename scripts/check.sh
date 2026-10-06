#!/usr/bin/env bash
# The gate, in three tiers. A test runs where it matters: each crate owns its tests, so a
# change runs the tests of the crates it touches, and the crates that depend on them are
# only compiled (commit) or run their fast tests (push). Slow suites (conformance, fuzz
# campaigns) are declared by their crate (`[package.metadata.nrese] slow-tests`) and run
# when that crate changed, or at a milestone. Benchmarks never run here.
#
# Usage: scripts/check.sh commit   pre-commit hook: the crates changed against HEAD
#                                  (staged or not). fmt; clippy on them and their
#                                  dependents; their fast tests.
#        scripts/check.sh push     pre-push hook: the crates changed since the upstream
#                                  branch. fmt; clippy on them and their dependents; all
#                                  their tests, the dependents' fast tests; the lock check.
#        scripts/check.sh [all]    milestones and CI: everything, both Cargo workspaces,
#                                  doc tests, cargo-deny, the console.
#
# Tests run under cargo-nextest when it is installed (every test of every binary in
# parallel; `cargo install --locked cargo-nextest`), else under cargo test, crate by
# crate. Builds go through scripts/cargo-guarded.sh. Every step runs; the summary lists
# each one's exit code, and the script fails if any failed.
set -u
cd "$(dirname "$0")/.."
mode=${1:-all}
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
        line=$(python scripts/lib/test-targets.py fast "$arg" | tr -d '\r')
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

# The steps for files outside the Rust workspace.
side_steps() {
  local files=("$@")
  if printf '%s\n' "${files[@]}" | grep -qE '(^|/)Cargo\.(toml|lock)$'; then
    step locks bash scripts/check-locks.sh
  fi
  if printf '%s\n' "${files[@]}" | grep -qE '^benches/suite/'; then
    step "suite contracts" python -m unittest discover -s benches/suite/tests
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
    --all-targets -- -D warnings
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

case "$mode" in
  commit | push)
    if [ "$mode" = commit ] && git rev-parse -q --verify MERGE_HEAD > /dev/null; then
      # A merge: what the merged tree has the same as the branch merged in was tested there.
      # New is what differs from it: this side's changes and the resolved conflicts.
      mapfile -t files < <(git diff --name-only --cached MERGE_HEAD; git ls-files --others --exclude-standard)
    elif [ "$mode" = commit ]; then
      mapfile -t files < <(git diff --name-only HEAD; git ls-files --others --exclude-standard)
    else
      base=$(git rev-parse --abbrev-ref --symbolic-full-name '@{upstream}' 2>/dev/null || echo HEAD~1)
      mapfile -t files < <(git diff --name-only "$base"...HEAD; git diff --name-only HEAD;
                           git ls-files --others --exclude-standard)
    fi
    mapfile -t changed < <(crates_of "${files[@]}")
    mapfile -t dependents < <([ ${#changed[@]} -gt 0 ] && dependents_of "${changed[@]}")
    echo "=== changed: ${changed[*]:-none}; depending on them: ${dependents[*]:-none}"
    step fmt cargo fmt --all --check
    if [ ${#changed[@]} -gt 0 ]; then
      # shellcheck disable=SC2046
      step clippy "$guarded" clippy --locked $(package_args "${changed[@]}" "${dependents[@]}") \
        --all-targets -- -D warnings
      if [ "$mode" = commit ]; then
        step test run_tests "${changed[@]}" -- "${changed[@]}"
      else
        step test run_tests "${changed[@]}" "${dependents[@]}" -- "${dependents[@]}"
        step locks bash scripts/check-locks.sh
      fi
    fi
    side_steps "${files[@]}"
    ;;
  all)
    step fmt cargo fmt --all --check
    step clippy "$guarded" clippy --locked --workspace --all-targets -- -D warnings
    mapfile -t everything < <(cargo metadata --no-deps --format-version 1 --offline \
      | python -c "import json,sys; print('\n'.join(p['name'] for p in json.load(sys.stdin)['packages']))" \
      | tr -d '\r')
    step test run_tests "${everything[@]}"
    step "doc tests" "$guarded" test --locked --workspace --doc
    harness
    step locks bash scripts/check-locks.sh
    step "suite contracts" python -m unittest discover -s benches/suite/tests
    if cargo deny --version > /dev/null 2>&1; then
      step deny cargo deny --all-features check
    else
      echo "=== cargo-deny isn't installed (cargo install --locked cargo-deny): skipped"
    fi
    console
    ;;
  *)
    echo "usage: scripts/check.sh commit|push|all" >&2
    exit 2
    ;;
esac

echo
echo "=== summary"
failed=0
for result in "${results[@]}"; do
  echo "  $result"
  [ "${result%% *}" = 0 ] || failed=1
done
exit $failed
