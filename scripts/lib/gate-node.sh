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

# DIR VERSION PLATFORM SHA256: DIR's directory of node, npm and npx, if its node reports VERSION and
# its npm runs: an entry without them would leave PATH to the host's npm, which may run the host's
# node (sol-loop r1 S2).
gate_node_is() {
  local bin=$1/bin found
  [ "$3" != win-x64 ] || bin=$1
  [ -x "$bin/node" ] || [ -x "$bin/node.exe" ] || return 1
  found=$("$bin/node" --version 2> /dev/null | tr -d '\r')
  [ "$found" = "v$2" ] || { echo "$bin/node reports ${found:-nothing}, not the pinned v$2" >&2; return 1; }
  [ -e "$bin/npm" ] && [ -e "$bin/npx" ] && PATH="$bin:$PATH" "$bin/npm" --version > /dev/null 2>&1 \
    || { echo "$bin has no npm and npx of its own that run" >&2; return 1; }
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
  find "$part/$name" -mindepth 1 -maxdepth 1 -exec mv -t "$part" -- {} + && rmdir -- "$part/$name"
}
