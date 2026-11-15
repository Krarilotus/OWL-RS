# The workspace crates a change reaches, which the push tier lints and tests (sourced by
# scripts/check.sh; scripts/tests/test_changed_crates.py).
#
# A file under a crate's directory (crates/) reaches that crate. The root manifest (its
# [workspace.package], [workspace.dependencies], profiles and lints), the root lock file, the
# toolchain file, Cargo's build configuration and clippy's configuration reach every crate of the
# workspace: a change to one of them is every crate changed. (D16-637: a version bump changed only
# such files, and the push tier printed "changed: none" and ran no crate's clippy or tests.) The
# lock files of the other Cargo projects (fuzz/, benches/*) pin only those projects' builds.
EVERY_CRATE='^(Cargo\.toml|Cargo\.lock|rust-toolchain(\.toml)?|\.cargo/config(\.toml)?|clippy\.toml)$'

changed_crates() {
  if printf '%s\n' "$@" | grep -qE "$EVERY_CRATE"; then
    workspace_packages
  else
    crates_of "$@"
  fi
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

# The workspace's packages, by name (`python`: scripts/check.sh's, under either name).
workspace_packages() {
  cargo metadata --no-deps --format-version 1 --offline \
    | python -c "import json,sys; print('\n'.join(p['name'] for p in json.load(sys.stdin)['packages']))" \
    | tr -d '\r'
}
