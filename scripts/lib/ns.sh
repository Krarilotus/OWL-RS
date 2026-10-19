# The job namespace for shell scripts: the twin of benches/suite/suitekit/ns.py, which says why
# (one job's cleanup removed another's volumes, 19 Oct 2026). Source it; every Docker container
# and volume name, and every label, comes from here (scripts/check-names.sh fails otherwise).
#
#   ns_namespace ROOT            FLEET_JOB when set, else c + a digest of the checkout's path
#   ns_name KIND ROOT            nrese-<kind>-<namespace>-<checkout digest>
#   ns_run_args NAME ROOT OWNER  sets NS_ARGS: --name NAME and the labels, for `docker run`
#   ns_volume create NAME ROOT OWNER | rm NAME | inspect NAME   a `docker volume` command
#   ns_shared KIND               a host-wide resource jobs share on purpose: data, cargo
#   ns_network create NAME ROOT OWNER | rm NAME   a labelled `docker network` (swept like volumes)
#   ns_dir BASE KIND ROOT        this job's own directory under a shared one (as ns.directory)
#   ns_pin_images ROOT TAG...    at the job's start, before its first timed step: every image its
#                                measured containers will run, pulled if missing and resolved to
#                                its ID for the whole job (ns.pin_images); fails, and the job
#                                with it, if one can't be resolved
#   ns_image TAG ROOT            the pinned ID to run; fails for a tag the job didn't pin (never
#                                run by its tag). ns_pins ROOT: the pins, for the result
#   ns_locked FILE CMD...        CMD under the host lock FILE: an OS lock the kernel releases when
#                                its holder dies (flock; else suitekit/lock.py)
#   ns_serialize NAME "$0" "$@"  the calling script, run again under the host lock NAME unless it
#                                holds it (shared dataset preparation: one at a time per host)
#   ns_clean ROOT [OWNER] [all]  removes this namespace's containers, volumes and networks: the
#                                checkout's own (or `all`: the whole job's), of OWNER or of all
#   ns_docker ARGS...            Docker (NRESE_DOCKER names another executable: the tests' stub)

ns_docker() { "${NRESE_DOCKER:-docker}" "$@"; }

ns_slug() {
  local s
  s=$(printf '%s' "$1" | tr 'A-Z' 'a-z' | sed -E 's/[^a-z0-9_.-]+/-/g; s/^[.-]+//; s/[.-]+$//')
  printf '%s' "${s:-x}"
}

# The checkout's path as ns.py spells it: forward slashes, symlinks resolved, Windows' drive form.
ns_checkout() { (cd "$1" && { pwd -W 2>/dev/null || pwd -P; }); }

ns_digest() { printf '%s' "$1" | sha1sum | cut -c1-"$2"; }

ns_namespace() {
  if [ -n "${FLEET_JOB:-}" ]; then ns_slug "$FLEET_JOB"; else printf 'c%s' "$(ns_digest "$(ns_checkout "$1")" 10)"; fi
}

ns_name() { printf 'nrese-%s-%s-%s' "$(ns_slug "$1")" "$(ns_namespace "$2")" "$(ns_digest "$(ns_checkout "$2")" 6)"; }

ns_labels() {
  NS_LABELS=(--label "nrese.ns=$(ns_namespace "$1")" --label "nrese.checkout=$(ns_checkout "$1")" --label "nrese.owner=$2")
}

ns_run_args() { ns_labels "$2" "$3"; NS_ARGS=(--name "$1" "${NS_LABELS[@]}"); }

ns_volume() {
  case $1 in
    create) ns_labels "$3" "$4"; ns_docker volume create "${NS_LABELS[@]}" "$2" ;;
    rm|inspect) ns_docker volume "$1" "$2" ;;
    *) echo "ns_volume: $1?" >&2; return 2 ;;
  esac
}

ns_network() {
  case $1 in
    create) ns_labels "$3" "$4"; ns_docker network create "${NS_LABELS[@]}" "$2" ;;
    rm) ns_docker network rm "$2" ;;
    *) echo "ns_network: $1?" >&2; return 2 ;;
  esac
}

ns_dir() { printf '%s/%s/%s' "$1" "$(ns_slug "$2")" "$(ns_name dir "$3" | sed 's/^nrese-dir-//')"; }

ns_lock_dir() { printf '%s' "${NRESE_LOCK_DIR:-$HOME/.nrese-locks}"; }

NS_SUITE=$(cd "$(dirname "${BASH_SOURCE[0]}")/../../benches/suite" && pwd)
ns_python() { "${NRESE_PYTHON:-$(command -v python3 || command -v python)}" "$@"; }

# FILE CMD...: one holder at a time per host, under an OS lock the kernel releases when its
# holder dies (never a lock file or directory a crash leaves behind). flock(1) where there is one
# (Linux: the gate's and the fleet's hosts; the same lock as suitekit/lock.py's); else (Git Bash)
# suitekit/lock.py, CMD in a child shell given this shell's functions and the variables the caller
# names in NS_LOCK_VARS.
ns_locked() {
  local file=$1
  shift
  mkdir -p "$(dirname "$file")"
  if command -v flock >/dev/null 2>&1; then
    ( flock 9; "$@" ) 9>>"$file"
    return
  fi
  local sh=$BASH script vars=""
  if command -v cygpath >/dev/null 2>&1; then  # Windows' paths for Windows' Python: one lock file
    sh=$(cygpath -w "$BASH")
    file=$(cygpath -w "$file")
  fi
  # shellcheck disable=SC2086
  [ -z "${NS_LOCK_VARS:-}" ] || vars=$(declare -p ${NS_LOCK_VARS} 2>/dev/null)
  script="$(declare -f)
$vars
$(printf '%q ' "$@")"
  ns_python "$NS_SUITE/suitekit/lock.py" "$file" -- "$sh" -c "$script"
}

ns_serialize() {
  local name=$1 held
  shift
  held=NS_HELD_$(ns_slug "$name" | tr -c 'a-z0-9' _)
  [ -n "${!held:-}" ] && return 0
  export "$held=1"
  ns_locked "$(ns_lock_dir)/$name.lock" bash "$@"
  exit $?
}

ns_pins_file() { printf '%s/pins/%s.tsv' "$(ns_lock_dir)" "$(ns_namespace "$1")"; }
ns_pins() { cat "$(ns_pins_file "$1")" 2>/dev/null; return 0; }

ns_pin_images() {  # ROOT TAG...
  local root=$1
  shift
  ns_python "$NS_SUITE/suitekit/ns.py" pin "$(ns_checkout "$root")" "$@"
}

ns_image() {
  case $1 in sha256:*) printf '%s' "$1"; return ;; esac
  local id
  id=$(awk -F'\t' -v t="$1" '$1 == t { print $2 }' "$(ns_pins_file "$2")" 2>/dev/null | head -1)
  [ -n "$id" ] || { echo "$1: not pinned at the job's start (ns_pin_images); never run by its tag" >&2; return 1; }
  printf '%s' "$id"
}

ns_shared() {
  case $1 in
    data) printf '%s' "${NRESE_BENCH_VOLUME:-nrese-bench-data}" ;;
    cargo) printf '%s' "${NRESE_CARGO_VOLUME:-nrese-cargo}" ;;
    *) echo "ns_shared: $1?" >&2; return 2 ;;
  esac
}

ns_clean() {
  local root=$1 owner=${2:-} scope=${3:-} item
  local filters=(--filter "label=nrese.ns=$(ns_namespace "$root")")
  [ "$scope" = all ] || filters+=(--filter "label=nrese.checkout=$(ns_checkout "$root")")
  [ -z "$owner" ] || filters+=(--filter "label=nrese.owner=$owner")
  for item in $(ns_docker ps -a -q "${filters[@]}"); do ns_docker rm -f -v "$item" >/dev/null && echo "$item"; done
  for item in $(ns_docker volume ls -q "${filters[@]}"); do ns_docker volume rm "$item" >/dev/null && echo "$item"; done
  for item in $(ns_docker network ls -q "${filters[@]}"); do ns_docker network rm "$item" >/dev/null && echo "$item"; done
  return 0
}
