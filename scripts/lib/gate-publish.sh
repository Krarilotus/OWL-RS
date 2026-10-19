# The release gate's builds, published by identity (scripts/release-gate.sh). Source it; it needs
# GATE_BIN (the host's binaries, shared by every gate) and GATE (this gate's own directory).
#
# A build goes into the directory of its identity, bin-<rev8>-<digest>: the revision, its
# toolchain (rustc -vV), the flags, the linker and the layout. It is made privately and renamed
# into place only if that directory doesn't exist (rename(2) won't replace a non-empty one):
# two gates building the same identity both end with the first one's, whole, and a reader never
# sees it missing or half replaced. Nothing removes an identity directory but the age sweep, and
# not one a gate used within GATE_BIN_AGE_DAYS (default 14): each use stamps a reference.
#
#   build_identity REV [LAYOUT]   what a build of REV is, before it's built
#   identity_dir REV [LAYOUT]     its directory under GATE_BIN
#   publish PART DIR              PART becomes DIR unless DIR exists (then PART goes); 0 when DIR
#                                 holds a verified build of PART's revision either way
#   bin_of REV                    the build this gate uses for REV (its pointer: build or unpack)
#   point REV DIR                 this gate uses DIR for REV (and stamps a reference)
#   sweep_builds                  identity directories no gate used for GATE_BIN_AGE_DAYS

build_identity() {
  local channel
  channel=$(git show "$1:rust-toolchain.toml" 2>/dev/null | sed -n 's/^channel *= *"\(.*\)"/\1/p')
  echo "revision $1"
  echo "toolchain ${channel:-none}"
  rustc ${channel:+"+$channel"} -vV 2>/dev/null | sed 's/^/rustc /'
  echo "rustflags ${RUSTFLAGS:-}"
  echo "linker ${CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_LINKER:-cc} $(cc --version 2>/dev/null | head -1)"
  echo "profile release --locked"
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

point() {
  mkdir -p "$GATE/builds" "$2/.refs"
  basename "$2" > "$GATE/builds/$(git rev-parse --short=8 "$1").partial" \
    && mv -f "$GATE/builds/$(git rev-parse --short=8 "$1").partial" "$GATE/builds/$(git rev-parse --short=8 "$1")"
  touch "$2/.refs/$(basename "$GATE")"
}

bin_of() {
  local name
  name=$(cat "$GATE/builds/$(git rev-parse --short=8 "$1")" 2>/dev/null) || { echo "$GATE_BIN/none"; return; }
  mkdir -p "$GATE_BIN/$name/.refs" 2>/dev/null && touch "$GATE_BIN/$name/.refs/$(basename "$GATE")" 2>/dev/null
  echo "$GATE_BIN/$name"
}

sweep_builds() {
  local dir newest
  for dir in "$GATE_BIN"/bin-*; do
    [ -d "$dir" ] || continue
    # A build that never finished (its gate stopped): its private directory, a day on.
    case $dir in *.partial-*) [ -n "$(find "$dir" -maxdepth 0 -mmin +1440)" ] && rm -rf "$dir"; continue ;; esac
    newest=$(find "$dir" "$dir/.refs" -maxdepth 1 -newermt "${GATE_BIN_AGE_DAYS:-14} days ago" 2>/dev/null | head -1)
    [ -n "$newest" ] && continue
    # Out of the way in one rename, then removed: never a half-removed build under its name.
    mv -T "$dir" "$GATE_BIN/.gone-$$-$(basename "$dir")" && rm -rf "$GATE_BIN/.gone-$$-$(basename "$dir")"
    echo "$(basename "$dir")"
  done
}
