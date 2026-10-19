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
#   bin_of REV                    the build this gate uses for REV (its pointer: build or unpack)
#   point REV DIR                 this gate uses DIR for REV (and stamps a reference)
#   sweep_builds                  identity directories no gate used for GATE_BIN_AGE_DAYS

. "$(dirname "${BASH_SOURCE[0]}")/ns.sh"

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

# The gate's measurement patches (part of every build's identity), found from this file.
GATE_PATCHES=${GATE_PATCHES:-$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)/benches/fast/patches}

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
  # The measurement patches release-gate.sh applies to every side's perf lab source.
  local patch
  for patch in "$GATE_PATCHES"/*.py; do
    [ -f "$patch" ] && echo "patch $(basename "$patch") $(sha256sum < "$patch" | cut -c1-16)"
  done
  echo "layout ${2:-1}"
}

identity_dir() {
  echo "$GATE_BIN/bin-$(git rev-parse --short=8 "$1")-$(build_identity "$1" "${2:-1}" | sha256sum | cut -c1-12)"
}

build_verified() {  # DIR REV
  [ "$(cat "$1/REVISION" 2>/dev/null)" = "$2" ] && (cd "$1" && sha256sum -c --quiet SHA256SUMS)
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

gate_lock() { NS_LOCK_VARS="GATE GATE_BIN GATE_PATCHES" ns_locked "$GATE_BIN/.lock" "$@"; }

# DIR: a reference from this gate, under the lock; fails when the build is gone (never made anew).
stamp() { [ -d "$1/release" ] && mkdir -p "$1/.refs" && touch "$1/.refs/$(basename "$GATE")"; }

point() {
  local key
  key=$(git rev-parse --short=8 "$1")
  gate_lock stamp "$2" || return 1
  mkdir -p "$GATE/builds"
  basename "$2" > "$GATE/builds/$key.partial" && mv -f "$GATE/builds/$key.partial" "$GATE/builds/$key"
}

bin_of() {
  local name
  name=$(cat "$GATE/builds/$(git rev-parse --short=8 "$1")" 2>/dev/null) && gate_lock stamp "$GATE_BIN/$name" \
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
