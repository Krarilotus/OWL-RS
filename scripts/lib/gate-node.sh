# The console's Node for a gate job (scripts/check.sh's console step): nodejs.org's build of the
# version scripts/lib/gate-tools.txt pins, taken only by its archive's sha256, unpacked under the
# host's cache (`node-<version>-<sha12>`), never the host's own Node and never a global install.
# Toolchains on hosts belong to the job (the owner, 2 Nov 2026). Sourced by the gate jobs,
# scripts/lib/gate-job.sh on Linux and scripts/lib/windows-job.sh on Windows.
#
#   gate_node CACHE TOOLS
#       prints the directory holding this platform's node and npm (linux-x64: the entry's bin;
#       win-x64: the entry), made first where it isn't there or its node doesn't report the pinned
#       version; else fails, the reason on stderr. The entry is made through the cache's one entry
#       maker (scripts/lib/suite-python.sh suite_python_entry: under the cache's lock, aside, what
#       killed jobs left cleared, moved into place whole).
#
# The distribution's base is NRESE_NODE_DIST (default https://nodejs.org/dist, a mirror's or a test's
# otherwise); the pinned sha256 decides what is taken from it.

# shellcheck source=suite-python.sh
declare -F suite_python_entry > /dev/null || . "$(dirname "${BASH_SOURCE[0]}")/suite-python.sh"

gate_node() {
  local cache=$1 tools=$2 platform version sha
  case "$(uname -s)" in
    Linux) platform=linux-x64 ;;
    MINGW* | MSYS* | CYGWIN*) platform=win-x64 ;;
    *) echo "no Node build pinned for $(uname -s)" >&2; return 1 ;;
  esac
  # `node VERSION PLATFORM SHA256 PLATFORM SHA256 ...`
  read -r version sha < <(tr -d '\r' < "$tools" \
    | awk -v p="$platform" '$1 == "node" { for (i = 3; i < NF; i += 2) if ($i == p) print $2, $(i + 1) }')
  [ -n "${version:-}" ] && [ -n "${sha:-}" ] || { echo "$tools pins no node for $platform" >&2; return 1; }
  suite_python_entry "$cache/node-$version-${sha:0:12}" gate_node_is gate_node_unpack "$version" "$platform" "$sha"
}

# PLATFORM VERSION SHA256: the archive part of an entry's provenance record.
gate_node_provenance() {
  local ext=tar.gz
  [ "$1" != win-x64 ] || ext=zip
  printf 'sha256 %s node-v%s-%s.%s\n' "$3" "$2" "$1" "$ext"
}

# DIR: DIR's payload, every regular file by its sha256 and every symlink by its target, in a fixed
# order; the entry's own metadata left out: its records (.provenance, .manifest) and the use marks the
# gate jobs write (.last-used, at the entry or beside its programs).
gate_node_manifest() {
  (cd "$1" && {
    find . -type l ! -name '.*' -printf 'link %p -> %l\n' | LC_ALL=C sort
    find . -type f ! -path ./.provenance ! -path ./.manifest ! -name .last-used -print0 | LC_ALL=C sort -z \
      | xargs -0 -r sha256sum -b
  })
}

# DIR PLATFORM VERSION SHA256: the record binding DIR's payload to the verified archive it was unpacked
# from (review r2 F2): its manifest, and the provenance line naming the archive and the manifest's sha256.
gate_node_seal() {
  gate_node_manifest "$1" > "$1/.manifest" || return 1
  printf '%s manifest %s\n' "$(gate_node_provenance "$2" "$3" "$4")" "$(sha256sum -b < "$1/.manifest" | cut -c1-64)" \
    > "$1/.provenance"
}

# PATH: its Git Bash (cygpath -u) form on Windows, so that `C:/x` and `/c/x` compare equal.
gate_node_canon() { cygpath -u "$1" 2> /dev/null || printf '%s\n' "$1"; }

# DIR VERSION PLATFORM SHA256: DIR's directory of node, npm and npx, if DIR is still the payload of the
# verified archive of SHA256 (its provenance names that archive and its manifest; the payload as it is
# now matches that manifest: review r1/r2 F2), and node, npm and npx, looked up as the console step
# looks them up (bare names, DIR's bin first on PATH), are DIR's own and run, node reporting VERSION:
# else the host's would run, and on Windows the host's npm and npx run the host's node (review r1/r2 F1).
gate_node_is() {
  local bin=$1/bin found tool on
  [ "$3" != win-x64 ] || bin=$1
  on=$(gate_node_canon "$bin")  # PATH's form: a `C:` in PATH splits at its colon
  [ -f "$1/.manifest" ] && [ "$(tr -d '\r' < "$1/.provenance" 2> /dev/null)" = \
    "$(gate_node_provenance "$3" "$2" "$4") manifest $(sha256sum -b < "$1/.manifest" | cut -c1-64)" ] \
    || { echo "$1 records no payload of the archive of sha256 $4" >&2; return 1; }
  [ "$(gate_node_manifest "$1")" = "$(cat "$1/.manifest")" ] \
    || { echo "$1's payload isn't the one unpacked from its archive" >&2; return 1; }
  for tool in node npm npx; do
    found=$(PATH="$on:$PATH"; hash -r; command -v "$tool") \
      && [ "$(gate_node_canon "${found%.exe}")" = "$on/$tool" ] \
      && (PATH="$on:$PATH"; hash -r; "$tool" --version > /dev/null 2>&1) \
      || { echo "$bin: $tool, as the console step finds it, isn't the entry's own or doesn't run" >&2; return 1; }
  done
  found=$(PATH="$on:$PATH"; hash -r; node --version 2> /dev/null | tr -d '\r')
  [ "$found" = "v$2" ] || { echo "$bin/node reports ${found:-nothing}, not the pinned v$2" >&2; return 1; }
  echo "$bin"
}

# PART VERSION PLATFORM SHA256: nodejs.org's archive of Node VERSION for PLATFORM, checked by SHA256,
# unpacked at PART without its top directory.
gate_node_unpack() {
  local part=$1 version=$2 platform=$3 sha=$4 name archive
  name=node-v$version-$platform
  if [ "$platform" = win-x64 ]; then archive=$name.zip; else archive=$name.tar.gz; fi
  curl -fsSL --max-time 300 -o "$part/$archive" "${NRESE_NODE_DIST:-https://nodejs.org/dist}/v$version/$archive" \
    || { echo "nodejs.org's $archive didn't download" >&2; return 1; }
  [ "$(sha256sum "$part/$archive" | cut -c1-64)" = "$sha" ] \
    || { echo "$archive isn't the pinned one (sha256 $sha)" >&2; return 1; }
  if [ "$platform" = win-x64 ]; then
    unzip -q "$part/$archive" -d "$part" || return 1
  else
    tar -xzf "$part/$archive" -C "$part" || return 1
  fi
  rm -f -- "$part/$archive"
  # The archive's one top directory, its contents moved up into PART.
  [ -d "$part/$name" ] || { echo "$archive has no $name/" >&2; return 1; }
  find "$part/$name" -mindepth 1 -maxdepth 1 -exec mv -t "$part" -- {} + && rmdir -- "$part/$name" || return 1
  # The record of the verified archive and of the payload unpacked from it, in the part: published with
  # the whole entry when it is moved into place (suite_python_entry), never on an entry made otherwise.
  gate_node_seal "$part" "$platform" "$version" "$sha"
}
