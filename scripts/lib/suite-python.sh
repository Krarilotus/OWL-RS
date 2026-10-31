# The suite contracts' Python packages (benches/suite/requirements.txt), in an environment of their
# own, never the host's Python (whose packages we don't install into): one per version of that file
# under the host's cache (Harness/notes/2026-10-20-host-cache-contract.md, `py-<hash>`). Sourced by
# the gate jobs, scripts/lib/gate-job.sh on Linux and scripts/lib/windows-job.sh on Windows; each
# says which Python of its host to start from.
#
#   suite_python CACHE REQUIREMENTS PYTHON
#       prints the environment's directory of programs (`bin`, or `Scripts` on Windows), the
#       environment made first where it isn't there; else fails, the reason on stderr.
#
# Made under the cache's lock (CACHE/py.lock, ns_locked: flock, else suitekit/lock.py run by PYTHON,
# never Windows' `python3`, which may be the Store's stub), aside, and moved into place whole: a job
# that finds one there uses it without the lock, and under the lock no other job's build is live, so
# what a killed job left (a `.partial-*`, or one without a Python) goes there, and one with a Python
# is never removed (Sol, windows-job-python r1). Without the host's venv module's pip (Debian's
# python3-venv), pip comes from its own bootstrap, into that environment only.

# shellcheck source=ns.sh
. "$(dirname "${BASH_SOURCE[0]}")/ns.sh"

suite_python() {
  local cache=$1 requirements=$2 python=$3 env
  env="$cache/py-$(git hash-object "$requirements" | cut -c1-12)"
  suite_python_bin "$env" && return
  mkdir -p "$cache" || return 1
  NRESE_PYTHON=$python ns_locked "$cache/py.lock" suite_python_make "$env" "$requirements" "$python" || return 1
  suite_python_bin "$env" || { echo "no Python in $env" >&2; return 1; }
}

# ENV REQUIREMENTS PYTHON, under the cache's lock: ENV made, unless another job made it while this
# one waited for the lock.
suite_python_make() {
  local env=$1 requirements=$2 python=$3 cache=${1%/*} part
  suite_python_bin "$env" > /dev/null && return
  rm -rf -- "${env:?}" "${env:?}".partial-*
  part=$(mktemp -d "$env.partial-XXXXXX") || return 1
  if ! suite_python_venv "$python" "$part"; then
    rm -rf -- "$part" && mkdir "$part" || return 1
    { suite_python_venv "$python" "$part" --without-pip \
        && curl -fsSL --max-time 300 https://bootstrap.pypa.io/get-pip.py -o "$cache/get-pip.py" \
        && suite_python_quiet "$(suite_python_bin "$part")/python" "$cache/get-pip.py"; } \
      || { rm -rf -- "$part"; echo "no Python environment for the suite (venv, pip)" >&2; return 1; }
  fi
  suite_python_quiet "$(suite_python_bin "$part")/python" -m pip install -r "$requirements" \
    || { rm -rf -- "$part"; echo "the suite's Python packages didn't install" >&2; return 1; }
  mv -T -- "$part" "$env" || { rm -rf -- "$part"; return 1; }
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
