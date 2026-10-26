# A fleet gate's passes, remembered by content (sourced by scripts/check.sh, scripts/lib/gate-job.sh
# and scripts/lib/windows-job.sh).
#
# A tree that passed the push tier (or the merge check's Windows part, `windows`) on the fleet
# never needs it again. The key is what the result depends on:
#
#   <tier>[ w3c-optional] <tree> <file list> rustc-<release>-<commit> flags=<rustflags> tools=<id> fleet
#
# - tree: the pushed (or checked) commit's tree, what was tested, byte for byte;
# - file list: the hash of the files the tier was selected from (the pushed range's; none for
#   `windows`, which runs everything);
# - the toolchain rustc -vV reports (the tree's rust-toolchain.toml picks it; a host with
#   another install or an override doesn't match), and the flags rustc gets
#   (scripts/cargo-guarded.sh --print-rustflags: `-C target-cpu=native` means each machine's own
#   CPU, recorded with the pass, not keyed);
# - tools: the gate's other tools and prerequisites, pinned (scripts/lib/gate-tools.txt, the
#   suite's benches/suite/requirements.txt), which the fleet job installs;
# - fleet: a pass is only ever a fleet job's, verified (scripts/lib/fleet-gate.sh): no other line
#   in the file (a hand-written one, an older gate's) has this form.
#
# A pass is a line `<key> | <the job, its host and CPU>` in <git-common-dir>/nrese-gate-passed,
# written by scripts/check.sh only.

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
  tools=$(cat "$root/scripts/lib/gate-tools.txt" "$root/benches/suite/requirements.txt" | tr -d '\r' | git hash-object --stdin)
  printf 'rustc-%s-%s flags=%s tools=%s\n' "$release" "${commit:0:12}" "$flags" "${tools:0:12}"
}

# The CPU `native` resolves to here (recorded with a pass).
gate_cpu() {
  rustc --print target-cpus 2> /dev/null | tr -d '\r' | sed -n 's/.*currently \([^)]*\)).*/\1/p' | head -1
}

# gate_key TIER TREE LIST_HASH IDENTITY
gate_key() {
  printf '%s%s %s %s %s fleet\n' "$1" "${NRESE_W3C_OPTIONAL:+ w3c-optional}" "$2" "$3" "$4"
}

gate_passed_file() { printf '%s/nrese-gate-passed\n' "$(git rev-parse --git-common-dir)"; }

# gate_passed KEY: whether a pass is recorded for KEY.
gate_passed() {
  [ -z "${NRESE_GATE_RERUN:-}" ] \
    && awk -v k="$1 |" 'index($0, k) == 1 { found = 1 } END { exit !found }' "$(gate_passed_file)" 2> /dev/null
}

# gate_record KEY WHERE: KEY passed (WHERE: the fleet job, its host and CPU).
gate_record() {
  printf '%s | %s\n' "$1" "$2" >> "$(gate_passed_file)"
}
