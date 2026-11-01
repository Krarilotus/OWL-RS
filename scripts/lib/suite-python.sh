# The suite contracts' Python packages (benches/suite/requirements.txt), in an environment of their
# own, never the host's Python (whose packages we don't install into): one per version of that file
# (and of the runtime declared for it) under the host's cache
# (Harness/notes/2026-10-20-host-cache-contract.md, `py-<hash>`). Sourced by the gate jobs,
# scripts/lib/gate-job.sh on Linux and scripts/lib/windows-job.sh on Windows; each says which Python
# to start from.
#
#   suite_python CACHE REQUIREMENTS PYTHON [RUNTIME]
#       prints the environment's directory of programs (`bin`, or `Scripts` on Windows), the
#       environment made first where it isn't there; else fails, the reason on stderr. With
#       RUNTIME (scripts/lib/gate-key.sh gate_runtime) it must run with exactly that
#       (suite_python_runtime): one that doesn't is made again, and one made that doesn't either
#       is refused.
#   suite_python_windows CACHE VERSION SHA256 BUILD
#       prints the python.exe of python.org's build of Python VERSION for Windows (its NuGet
#       package, by the package's SHA256), unpacked under CACHE first where it isn't there or
#       doesn't report BUILD (its sys.version); else fails.
#   suite_python_runtime PYTHON
#       what PYTHON runs with: `python <sys.version>`, then each distribution it finds but pip, as
#       name==version (the names normalised, sorted).
#
# Each is made under the cache's lock (CACHE/py.lock, ns_locked: flock, else suitekit/lock.py run
# by NRESE_PYTHON, never Windows' `python3`, which may be the Store's stub), aside, and moved into
# place whole (suite_python_entry): a job that finds one there as it must be uses it without the
# lock, and under the lock no other job's build is live, so what a killed job left (a
# `.partial-*`, or one not as it must be) goes there, and one as it must be is never removed (Sol,
# windows-job-python r1). Without the host's venv module's pip (Debian's python3-venv), pip comes
# from its own bootstrap, into that environment only.

# shellcheck source=ns.sh
. "$(dirname "${BASH_SOURCE[0]}")/ns.sh"

suite_python() {
  local cache=$1 requirements=$2 python=$3 runtime=${4:-} env
  env="$cache/py-$(git hash-object "$requirements" | cut -c1-12)"
  # One per declared runtime too: jobs of two declarations on one host don't make each other's again.
  [ -z "$runtime" ] || env="$env-$(printf '%s\n' "$runtime" | git hash-object --stdin | cut -c1-12)"
  NRESE_PYTHON=$python suite_python_entry "$env" suite_python_ready suite_python_make \
    "$requirements" "$python" "$runtime"
}

suite_python_windows() {
  suite_python_entry "$1/python-$2-${3:0:12}" suite_python_is suite_python_unpack "$2" "$3" "$4"
}

suite_python_runtime() {
  "$1" - << 'EOF' | tr -d '\r'
import importlib.metadata
import re
import sys

print("python", sys.version)
found = sorted(f"{re.sub(r'[-_.]+', '-', d.metadata['Name']).lower()}=={d.version}"
               for d in importlib.metadata.distributions())
print("\n".join(d for d in found if not d.startswith("pip==")))
EOF
}

# ENTRY CHECK MAKE [ARG...]: what `CHECK ENTRY ARG...` prints once it passes, ENTRY (a directory of
# the cache) made first where it doesn't: under the cache's lock, where CHECK runs again (another
# job may have made it meanwhile), ENTRY and what killed jobs left of it (ENTRY.partial-*) go,
# `MAKE PART ARG...` makes it aside, and it is moved into place whole once CHECK passes on PART.
suite_python_entry() {
  local entry=$1 check=$2
  "$check" "$entry" "${@:4}" && return
  mkdir -p "${entry%/*}" || return 1
  ns_locked "${entry%/*}/py.lock" suite_python_made "$@" || return 1
  "$check" "$entry" "${@:4}" || { echo "$entry: not as it must be once made" >&2; return 1; }
}

# ENTRY CHECK MAKE [ARG...], under the cache's lock.
suite_python_made() {
  local entry=$1 check=$2 make=$3 part
  shift 3
  "$check" "$entry" "$@" > /dev/null && return
  rm -rf -- "${entry:?}" "${entry:?}".partial-*
  part=$(mktemp -d "$entry.partial-XXXXXX") || return 1
  if "$make" "$part" "$@" && "$check" "$part" "$@" > /dev/null; then
    mv -T -- "$part" "$entry" && return
  fi
  rm -rf -- "$part"
  return 1
}

# DIR REQUIREMENTS PYTHON [RUNTIME]: DIR's directory of programs, if it holds the suite's Python (on
# Windows its python3.exe too) and, with RUNTIME, runs with exactly that.
suite_python_ready() {
  local bin found
  bin=$(suite_python_bin "$1") || return 1
  if [ -n "${4:-}" ]; then
    found=$(suite_python_runtime "$bin/python")
    if [ "$found" != "$4" ]; then
      echo "$1 doesn't run with the declared Python (<: it, >: declared):" >&2
      diff <(printf '%s\n' "$found") <(printf '%s\n' "$4") >&2
      return 1
    fi
  fi
  echo "$bin"
}

# PART REQUIREMENTS PYTHON [RUNTIME]: the environment, made at PART from PYTHON.
suite_python_make() {
  local part=$1 requirements=$2 python=$3 cache=${1%/*}
  if ! suite_python_venv "$python" "$part"; then
    rm -rf -- "$part" && mkdir "$part" || return 1
    { suite_python_venv "$python" "$part" --without-pip \
        && curl -fsSL --max-time 300 https://bootstrap.pypa.io/get-pip.py -o "$cache/get-pip.py" \
        && suite_python_quiet "$(suite_python_bin "$part")/python" "$cache/get-pip.py"; } \
      || { echo "no Python environment for the suite (venv, pip)" >&2; return 1; }
  fi
  # Only the files the requirements name by their sha256.
  suite_python_quiet "$(suite_python_bin "$part")/python" -m pip install --require-hashes -r "$requirements" \
    || { echo "the suite's Python packages didn't install" >&2; return 1; }
}

# DIR VERSION SHA256 BUILD: DIR's python.exe, if it reports BUILD.
suite_python_is() {
  local found
  [ -x "$1/tools/python.exe" ] || return 1
  found=$("$1/tools/python.exe" -I -c 'import sys; print(sys.version)' | tr -d '\r')
  [ "$found" = "$4" ] || { echo "$1/tools/python.exe reports $found, not the declared $4" >&2; return 1; }
  echo "$1/tools/python.exe"
}

# PART VERSION SHA256 BUILD: python.org's NuGet package of Python VERSION, checked by SHA256, unpacked
# at PART (its python.exe in tools/).
suite_python_unpack() {
  local part=$1 version=$2 sha=$3
  curl -fsSL --max-time 300 -o "$part/python.nupkg" "https://api.nuget.org/v3-flatcontainer/python/$version/python.$version.nupkg" \
    || { echo "python.org's package of Python $version didn't download" >&2; return 1; }
  [ "$(sha256sum "$part/python.nupkg" | cut -c1-64)" = "$sha" ] \
    || { echo "python.org's package of Python $version isn't the declared one (sha256 $sha)" >&2; return 1; }
  unzip -q "$part/python.nupkg" -d "$part" && rm -f -- "$part/python.nupkg"
}

# PYTHON DIR [ARG...]: a venv made at DIR. On Windows its programs get python3.exe beside python.exe,
# a copy of that launcher (as PyPA's virtualenv makes), so that a script's or a test's `python3` is
# the suite's there too, never the host's: on one Windows host `python3` is the Store's stub, on
# another it reached the host's own Python (windows-job-python, 31 Oct 2026).
suite_python_venv() {
  local python=$1 dir=$2
  shift 2
  suite_python_quiet "$python" -m venv "$@" "$dir" || return 1
  [ ! -e "$dir/Scripts/python.exe" ] || cp -- "$dir/Scripts/python.exe" "$dir/Scripts/python3.exe"
}

# ENV: its directory of programs, if it holds the suite's Python (on Windows its python3.exe too).
suite_python_bin() {
  if [ -x "$1/bin/python" ]; then
    echo "$1/bin"
  elif [ -x "$1/Scripts/python.exe" ] && [ -x "$1/Scripts/python3.exe" ]; then
    echo "$1/Scripts"
  else
    return 1
  fi
}

# COMMAND...: run, its output shown (the last lines, on stderr) only when it fails.
suite_python_quiet() {
  local log
  log=$(mktemp) || return 1
  "$@" > "$log" 2>&1 || { tail -20 "$log" >&2; rm -f -- "$log"; return 1; }
  rm -f -- "$log"
}
