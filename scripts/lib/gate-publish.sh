# The release gate's builds, published by identity (scripts/release-gate.sh). Source it; it needs
# GATE_BIN (the host's binaries, shared by every gate) and GATE (this gate's own directory).
#
# A build goes into the directory of its identity, bin-<rev8>-<digest>: the revision, its
# toolchain (rustc -vV), the flags rustc gets (the revision's cargo-guarded.sh adds the CPU
# choice, NRESE_TARGET_CPU: a portable and an x86-64-v3 build are different builds; `native` adds
# the CPU it resolved to and its features), the linker and the layout. It is made privately and
# renamed into place only if that directory doesn't exist (rename(2) won't replace a non-empty
# one; staging is a sibling, on the same file system): two gates building the same identity both
# end with the first one's, whole, and a reader never sees it missing or half replaced.
#
# Use and removal share one host lock (GATE_BIN/.lock): a gate stamps a reference to a build it
# uses under the lock, and fails closed when the build is gone; the age sweep deletes a build only
# under the lock and only if, checked again there, no gate used it for GATE_BIN_AGE_DAYS (default
# 14). A use between the sweep's first look and its delete therefore keeps the build.
#
#   build_identity REV [LAYOUT]   what a build of REV is, before it's built
#   identity_dir REV [LAYOUT]     its directory under GATE_BIN
#   publish PART DIR              PART becomes DIR unless DIR exists (then PART goes); 0 when DIR
#                                 holds a verified build of PART's revision either way
#   bin_of REV [LAYOUT]           the build this gate uses for REV (its pointer: build or unpack)
#   point REV DIR [LAYOUT]        this gate uses DIR for REV (and stamps a reference); a layout
#                                 other than 1 has a pointer of its own
#   sweep_builds                  identity directories no gate used for GATE_BIN_AGE_DAYS
#   sweep_patch_keyed [--remove]  the builds of a patched perf lab, listed; their binaries go
#                                 with --remove, unless a gate used them within GATE_LIVE_HOURS

. "$(dirname "${BASH_SOURCE[0]}")/ns.sh"

# The linker: the toolchain's own rust-lld (x86_64-unknown-linux-gnu's default since Rust 1.90;
# every side's rust-toolchain.toml names 1.98.1), through cc, never a host's configured one
# (a ~/.cargo/config.toml linker or rustflags: release-gate.sh's RUSTFLAGS replace those, the
# linker variable is pinned). Each build reads the linker back from its binaries (lld_of) and
# fails for any other.
export CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_LINKER=cc
GATE_RUSTFLAGS=""
# LAYOUT: the flags of a layout's timed tools (layout 1 the plain link, L > 1 its functions in
# another order, seed L).
layout_flags() { echo "$GATE_RUSTFLAGS${GATE_RUSTFLAGS:+ }$( [ "$1" -gt 1 ] && echo "-C link-arg=-Wl,--shuffle-sections=.text*=$1")"; }
# DIR: the LLD every binary under DIR/release was linked by ("LLD 22.1.8"); fails, naming the
# binary, for one linked by anything else or by two versions.
lld_of() {
  local f l seen=""
  while IFS= read -r f; do
    l=$(grep -aoE 'Linker: LLD [0-9][0-9.]*' "$f" | head -1)
    [ -n "$l" ] || { echo "$f: not linked by LLD" >&2; return 1; }
    [ -z "$seen" ] || [ "$l" = "$seen" ] || { echo "$f: $l, the others $seen" >&2; return 1; }
    seen=$l
  done < <(find "$1/release" -type f | sort)
  [ -n "$seen" ] && echo "${seen#Linker: }"
}

# The flags the revision's own build hands rustc: its cargo-guarded.sh --print-rustflags where it
# has one, else its scripts/lib/target-cpu.sh applied here, as its cargo-guarded.sh does (every
# revision the gate builds has it), else RUSTFLAGS as they are.
effective_rustflags() {
  local tmp out
  tmp=$(mktemp -d)
  git archive "$1" scripts 2>/dev/null | tar -x -C "$tmp" 2>/dev/null
  if grep -q -- '--print-rustflags' "$tmp/scripts/cargo-guarded.sh" 2>/dev/null; then
    out=$(bash "$tmp/scripts/cargo-guarded.sh" --print-rustflags)
  elif [ -f "$tmp/scripts/lib/target-cpu.sh" ]; then
    # shellcheck disable=SC1091
    out=$( . "$tmp/scripts/lib/target-cpu.sh"; export_target_cpu_rustflags; printf '%s' "${RUSTFLAGS:-}")
  else
    out=${RUSTFLAGS:-}
  fi
  rm -rf "$tmp"
  printf '%s' "$out"
}

build_identity() {
  local channel flags
  channel=$(git show "$1:rust-toolchain.toml" 2>/dev/null | sed -n 's/^channel *= *"\(.*\)"/\1/p')
  flags=$(effective_rustflags "$1")
  echo "revision $1"
  echo "toolchain ${channel:-none}"
  rustc ${channel:+"+$channel"} -vV 2>/dev/null | sed 's/^/rustc /'
  echo "rustflags $flags"
  case " $flags " in
    *"target-cpu=native"*)
      echo "native cpu $(rustc ${channel:+"+$channel"} --print target-cpus 2>/dev/null | sed -n 's/.*currently \([^)]*\)).*/\1/p' | head -1)"
      echo "native features $(rustc ${channel:+"+$channel"} -C target-cpu=native --print cfg 2>/dev/null | grep '^target_feature' | sha256sum | cut -c1-16)"
      ;;
  esac
  echo "linker ${CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_LINKER:-cc} $(cc --version 2>/dev/null | head -1)"
  echo "profile release --locked"
  echo "layout ${2:-1}"
}

identity_dir() {
  echo "$GATE_BIN/bin-$(git rev-parse --short=8 "$1")-$(build_identity "$1" "${2:-1}" | sha256sum | cut -c1-12)"
}

# DIR: a build of a perf lab patched at build time, before T0 (28 Oct 2026): its timed code is in no
# commit, so it is never reused, timed or imported (its IDENTITY names the patch).
patch_keyed() { grep -q '^patch perf_lab_watchdog' "$1/IDENTITY" 2>/dev/null; }

build_verified() {  # DIR REV
  [ "$(cat "$1/REVISION" 2>/dev/null)" = "$2" ] && ! patch_keyed "$1" && (cd "$1" && sha256sum -c --quiet SHA256SUMS)
}

publish() {
  local part=$1 dir=$2 rev
  rev=$(cat "$part/REVISION")
  if mv -T "$part" "$dir" 2>/dev/null; then
    return 0
  fi
  # Another gate published this identity first: its build stays, ours goes.
  rm -rf "$part"
  build_verified "$dir" "$rev"
}

gate_lock() { NS_LOCK_VARS="GATE GATE_BIN" ns_locked "$GATE_BIN/.lock" "$@"; }

# DIR: a reference from this gate, under the lock; fails when the build is gone (never made anew).
stamp() { [ -d "$1/release" ] && mkdir -p "$1/.refs" && touch "$1/.refs/$(basename "$GATE")"; }

# REV [LAYOUT]: the name of this gate's pointer to a revision's build of that layout.
pointer() { if [ "${2:-1}" = 1 ]; then git rev-parse --short=8 "$1"; else echo "$(git rev-parse --short=8 "$1")-l$2"; fi; }

point() {
  local key
  key=$(pointer "$1" "${3:-1}")
  gate_lock stamp "$2" || return 1
  mkdir -p "$GATE/builds"
  basename "$2" > "$GATE/builds/$key.partial" && mv -f "$GATE/builds/$key.partial" "$GATE/builds/$key"
}

bin_of() {
  local name
  name=$(cat "$GATE/builds/$(pointer "$1" "${2:-1}")" 2>/dev/null) && gate_lock stamp "$GATE_BIN/$name" \
    || { echo "$GATE_BIN/none"; return; }
  echo "$GATE_BIN/$name"
}

# DIR: no gate used it for GATE_BIN_AGE_DAYS (the build's own files and its references).
old_build() { [ -z "$(find "$1" "$1/.refs" -maxdepth 1 -newermt "${GATE_BIN_AGE_DAYS:-14} days ago" 2>/dev/null | head -1)" ]; }

# DIR: removed if, checked again under the lock, it is still old: out of the way in one rename,
# then removed, never a half-removed build under its name.
remove_if_old() {
  old_build "$1" || return 1
  mv -T "$1" "$GATE_BIN/.gone-$$-$(basename "$1")" && rm -rf "$GATE_BIN/.gone-$$-$(basename "$1")"
}

sweep_builds() {
  local dir
  for dir in "$GATE_BIN"/bin-*; do
    [ -d "$dir" ] || continue
    # A build that never finished (its gate stopped): its private directory, a day on.
    case $dir in *.partial-*) [ -n "$(find "$dir" -maxdepth 0 -mmin +1440)" ] && rm -rf "$dir"; continue ;; esac
    old_build "$dir" || continue
    gate_lock remove_if_old "$dir" && basename "$dir"
  done
  return 0
}

# DIR: a gate used it within GATE_LIVE_HOURS (default 24): every block stamps the builds it times.
live_build() { [ -n "$(find "$1/.refs" -mindepth 1 -maxdepth 1 -mmin -$(( ${GATE_LIVE_HOURS:-24} * 60 )) 2>/dev/null | head -1)" ]; }

# DIR: its binaries go if, checked again under the lock, no gate is using it; its IDENTITY,
# BUILD.txt and SHA256SUMS stay as the record (a build of that identity no longer verifies).
release_unused() {
  live_build "$1" && return 1
  [ -d "$1/release" ] || return 0
  mv -T "$1/release" "$1/.release-gone-$$" && rm -rf "$1/.release-gone-$$"
}

# The builds of a perf lab patched at build time (before T0, 28 Oct 2026: their IDENTITY names
# `patch perf_lab_watchdog`), one line each: the outcome, the directory, its bytes, its release/
# bytes, and its references (gate@time). Their binaries go with --remove, else nothing does; a
# build a live gate references stays.
sweep_patch_keyed() {
  local dir refs bytes binaries outcome
  for dir in "$GATE_BIN"/bin-*; do
    case $dir in *.partial-*) continue ;; esac
    patch_keyed "$dir" || continue
    bytes=$(du -sb "$dir" | cut -f1)
    binaries=$(du -sb "$dir/release" 2>/dev/null | cut -f1)
    refs=$(find "$dir/.refs" -mindepth 1 -maxdepth 1 -printf '%f@%TY-%Tm-%Td %TH:%TM ' 2>/dev/null)
    if [ ! -d "$dir/release" ]; then outcome=released
    elif live_build "$dir"; then outcome=kept-live
    elif [ "${1:-}" != --remove ]; then outcome=would-release
    elif gate_lock release_unused "$dir"; then outcome=released-now
    else outcome=kept-live
    fi
    printf '%s\t%s\t%s\t%s\t%s\n' "$outcome" "$(basename "$dir")" "$bytes" "${binaries:-0}" "${refs:-none}"
  done
  return 0
}
