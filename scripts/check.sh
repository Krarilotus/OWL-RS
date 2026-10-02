#!/usr/bin/env bash
# The gate every commit passes, with the steps of CI (.github/workflows/ci.yml): rustfmt,
# clippy on all targets with warnings as errors, the tests (both Cargo workspaces), the
# lock check, cargo-deny (licences, advisories, bans, sources) where it is installed, and
# the console's typecheck and tests where its dependencies are installed. Builds go
# through scripts/cargo-guarded.sh. Every step runs; the summary lists each one's exit
# code, and the script fails if any failed.
#
# Usage: scripts/check.sh            the whole gate (before a push; what CI runs)
#        scripts/check.sh --changed  only the crates changed against HEAD (staged or not):
#                                    fmt, clippy and tests for them (the pre-commit hook)
set -u
cd "$(dirname "$0")/.."
mode=${1:-all}
guarded=scripts/cargo-guarded.sh
declare -a results=()
step() {
  local name=$1
  shift
  echo "=== $name"
  "$@"
  local code=$?
  results+=("$code  $name")
  echo "=== $name: exit $code"
}

if [ "$mode" = --changed ]; then
  # The crates whose files changed (a crate is a directory with a Cargo.toml under crates/).
  mapfile -t files < <(git diff --name-only HEAD; git ls-files --others --exclude-standard)
  declare -A seen=()
  packages=()
  for file in "${files[@]}"; do
    dir=$(dirname "$file")
    while [ "$dir" != . ] && [ "$dir" != / ]; do
      if [ -f "$dir/Cargo.toml" ] && [[ "$dir" == crates/* ]]; then
        name=$(sed -n 's/^name = "\(.*\)"/\1/p' "$dir/Cargo.toml" | head -1)
        if [ -n "$name" ] && [ -z "${seen[$name]:-}" ]; then
          seen[$name]=1
          packages+=("-p" "$name")
        fi
        break
      fi
      dir=$(dirname "$dir")
    done
  done
  step fmt cargo fmt --all --check
  if [ ${#packages[@]} -gt 0 ]; then
    step clippy "$guarded" clippy --locked "${packages[@]}" --all-targets -- -D warnings
    step test "$guarded" test --locked "${packages[@]}" --no-fail-fast
  else
    echo "=== no crate changed: fmt only"
  fi
else
  step fmt cargo fmt --all --check
  step "fmt (bench harness)" cargo fmt --manifest-path benches/nrese-bench-harness/Cargo.toml --all --check
  step clippy "$guarded" clippy --locked --workspace --all-targets -- -D warnings
  step "clippy (bench harness)" "$guarded" clippy --locked \
    --manifest-path benches/nrese-bench-harness/Cargo.toml --all-targets -- -D warnings
  step test "$guarded" test --locked --workspace --no-fail-fast
  step "test (bench harness)" "$guarded" test --locked \
    --manifest-path benches/nrese-bench-harness/Cargo.toml --no-fail-fast
  step locks bash scripts/check-locks.sh
  if cargo deny --version > /dev/null 2>&1; then
    step deny cargo deny --all-features check
  else
    echo "=== cargo-deny isn't installed (cargo install --locked cargo-deny): skipped"
  fi
  if [ -d apps/nrese-console/node_modules ]; then
    step "console typecheck" npm --prefix apps/nrese-console run typecheck
    step "console tests" npx --prefix apps/nrese-console vitest run --root apps/nrese-console
  else
    echo "=== the console's dependencies aren't installed (npm ci in apps/nrese-console): skipped"
  fi
fi

echo
echo "=== summary"
failed=0
for result in "${results[@]}"; do
  echo "  $result"
  [ "${result%% *}" = 0 ] || failed=1
done
exit $failed
