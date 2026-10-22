# A gate's passes, remembered by content (sourced by scripts/check.sh and scripts/lib/fleet-gate.sh).
#
# A tier passed for a key never runs again for it. The key is what the result depends on:
#
#   <tier>[ w3c-optional] <tree> <file list> rustc-<release>-<commit> flags=<rustflags> tools=<id> part=<part>
#
# - tree: the working tree's id (scripts/lib/remote.sh tree), what was tested, byte for byte;
# - file list: the hash of the changed files the tier was selected from;
# - the toolchain rustc -vV reports (the tree's rust-toolchain.toml picks it; a host with
#   another install or an override doesn't match), and the flags rustc gets
#   (scripts/cargo-guarded.sh --print-rustflags: `-C target-cpu=native` means each machine's own
#   CPU, recorded with the pass, not keyed);
# - tools: the gate's other tools and prerequisites, pinned (scripts/lib/gate-tools.txt, the
#   suite's benches/suite/requirements.txt; GATE_TOOLING's, when another tree supplies them) and
#   checked by every run (gate_tools_check): a run whose tools differ fails, so a pass never
#   stands for a weaker gate;
# - part: `windows`, the steps that stay on Windows (scripts/lib/windows-tests.txt and the
#   console), or `rest`, all the others, which a build host may run. A tier passed when both
#   parts did; a full run on Windows records both.
#
# A pass is a line `<key> | <where and on what CPU>` in <git-common-dir>/nrese-gate-passed.

# rustc-<release>-<commit> flags=<rustflags> tools=<id>, for the toolchain, flags and tools a
# build here uses.
gate_identity() {
  local root info release commit flags tools
  # Cargo takes CARGO_ENCODED_RUSTFLAGS over RUSTFLAGS, and the identity (and what a fleet gate is
  # sent) names RUSTFLAGS only: a gate under it would stand for flags it never ran with (Sol,
  # review-remote-gates-r1-20261020). Refused, before any pass is looked up or run.
  if [ -n "${CARGO_ENCODED_RUSTFLAGS+set}" ]; then
    echo "the gate doesn't run under CARGO_ENCODED_RUSTFLAGS (it overrides RUSTFLAGS): unset it, set RUSTFLAGS" >&2
    return 1
  fi
  root=$(git rev-parse --show-toplevel)
  info=$(cd "$root" && rustc -vV 2> /dev/null | tr -d '\r') || return 1
  release=$(printf '%s\n' "$info" | sed -n 's/^release: //p')
  commit=$(printf '%s\n' "$info" | sed -n 's/^commit-hash: //p')
  [ -n "$release" ] && [ -n "$commit" ] || return 1
  flags=$(bash "$root/scripts/cargo-guarded.sh" --print-rustflags | tr -d '\r' | tr ' ' _)
  tools=$(cat "${GATE_TOOLING:-$root}/scripts/lib/gate-tools.txt" "${GATE_TOOLING:-$root}/benches/suite/requirements.txt" | tr -d '\r' | git hash-object --stdin)
  printf 'rustc-%s-%s flags=%s tools=%s\n' "$release" "${commit:0:12}" "$flags" "${tools:0:12}"
}

# Of the changed files on stdin, the ones every crate is built with: the workspace's manifest and
# lock, the toolchain, Cargo's and the tools' configuration (audit COT-17). A change to one counts
# as a change to every crate.
gate_global_inputs() {
  tr -d '\r' | grep -E '^(Cargo\.(toml|lock)|rust-toolchain(\.toml)?|\.cargo/.*|\.?(clippy|rustfmt)\.toml|\.config/nextest\.toml|deny\.toml)$'
}

# A pinned tool's version (scripts/lib/gate-tools.txt).
gate_tool_pin() {
  sed -n "s/^$1 //p" "${GATE_TOOLING:-$(git rev-parse --show-toplevel)}/scripts/lib/gate-tools.txt" | tr -d '\r'
}

# gate_tools_check TOOL...: whether the tools here are the pinned ones (cargo-nextest, cargo-deny,
# miri: the pinned toolchain has Miri, python: benches/suite/requirements.txt is installed);
# says what differs and how to fix it.
gate_tools_check() {
  local tool want have bad=0
  for tool in "$@"; do
    case $tool in
      cargo-nextest | cargo-deny)
        want=$(gate_tool_pin "$tool")
        have=$(cargo "${tool#cargo-}" --version 2> /dev/null | tr -d '\r' | awk 'NR == 1 { print $2 }')
        [ "$have" = "$want" ] \
          || { echo "$tool ${have:-missing}, the gate's is $want: cargo install --locked $tool@$want"; bad=1; } ;;
      miri)
        want=$(gate_tool_pin miri)
        cargo "+$want" miri --version > /dev/null 2>&1 \
          || { echo "no Miri on $want: rustup toolchain install $want --component miri"; bad=1; } ;;
      python)
        "$(type -P python || type -P python3)" - "${GATE_TOOLING:-$(git rev-parse --show-toplevel)}/benches/suite/requirements.txt" << 'EOF' || bad=1
import importlib.metadata as m, sys
for line in open(sys.argv[1], encoding="utf-8"):
    line = line.split("#")[0].strip()
    if not line:
        continue
    name, want = line.split("==")
    try:
        have = m.version(name)
    except m.PackageNotFoundError:
        have = "missing"
    if have != want:
        print(f"Python's {name} {have}, the suite's is {want}: pip install {name}=={want}")
        sys.exit(1)
EOF
        ;;
    esac
  done
  return $bad
}

# The CPU `native` resolves to here (recorded with a pass).
gate_cpu() {
  rustc --print target-cpus 2> /dev/null | tr -d '\r' | sed -n 's/.*currently \([^)]*\)).*/\1/p' | head -1
}

# gate_key TIER TREE LIST_HASH IDENTITY PART
gate_key() {
  printf '%s%s %s %s %s part=%s\n' "$1" "${NRESE_W3C_OPTIONAL:+ w3c-optional}" "$2" "$3" "$4" "$5"
}

gate_passed_file() { printf '%s/nrese-gate-passed\n' "$(git rev-parse --git-common-dir)"; }

# gate_passed KEY: whether a pass is recorded for KEY.
gate_passed() {
  [ -z "${NRESE_GATE_RERUN:-}" ] \
    && awk -v k="$1 |" 'index($0, k) == 1 { found = 1 } END { exit !found }' "$(gate_passed_file)" 2> /dev/null
}

# gate_record KEY WHERE: KEY passed (WHERE: the machine and CPU).
gate_record() {
  printf '%s | %s\n' "$1" "$2" >> "$(gate_passed_file)"
}
