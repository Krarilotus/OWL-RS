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

# PLATFORM VERSION SHA256: the provenance record of an entry unpacked from that archive.
gate_node_provenance() {
  local ext=tar.gz
  [ "$1" != win-x64 ] || ext=zip
  printf 'sha256 %s node-v%s-%s.%s\n' "$3" "$2" "$1" "$ext"
}

# DIR VERSION PLATFORM SHA256: DIR's directory of node, npm and npx, if DIR records the archive of
# SHA256 it was unpacked from (review r1 F2: a version string alone isn't the build), its node reports
# VERSION, and its own node, npm and npx are files that run: one that doesn't would leave PATH to the
# host's, and on Windows the host's npm and npx run the host's node (sol-loop r1 S2, review r1 F1).
gate_node_is() {
  local bin=$1/bin found tool
  [ "$3" != win-x64 ] || bin=$1
  [ "$(tr -d '\r' < "$1/.provenance" 2> /dev/null)" = "$(gate_node_provenance "$3" "$2" "$4")" ] \
    || { echo "$1 records no archive of sha256 $4" >&2; return 1; }
  [ -x "$bin/node" ] || [ -x "$bin/node.exe" ] || return 1
  found=$("$bin/node" --version 2> /dev/null | tr -d '\r')
  [ "$found" = "v$2" ] || { echo "$bin/node reports ${found:-nothing}, not the pinned v$2" >&2; return 1; }
  for tool in npm npx; do
    [ -f "$bin/$tool" ] && PATH="$bin:$PATH" "$bin/$tool" --version > /dev/null 2>&1 \
      || { echo "$bin has no $tool of its own that runs" >&2; return 1; }
  done
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
  # The record of the verified archive, in the part: published with the whole entry when it is moved
  # into place (suite_python_entry), never on an entry made otherwise.
  gate_node_provenance "$platform" "$version" "$sha" > "$part/.provenance"
}
