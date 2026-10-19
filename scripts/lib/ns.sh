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
#   ns_image TAG ROOT            the image ID the job runs for TAG: resolved (pulled if missing)
#                                the first time the job uses it, that ID for the rest of the job
#                                (ns.pin_image; the pins are the result's: ns_pins ROOT)
#   ns_locked FILE CMD...        CMD under the host lock FILE (flock; else a lock directory)
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

# FILE CMD...: one holder at a time per host. flock(2) where there is one (Linux: the gate's and
# the fleet's hosts; the same lock as ns.host_lock's); else a lock directory with its holder's pid,
# taken over when that process is gone.
ns_locked() {
  local file=$1 rc
  shift
  mkdir -p "$(dirname "$file")"
  if command -v flock >/dev/null 2>&1; then
    ( flock 9; "$@" ) 9>>"$file"
    return
  fi
  until mkdir "$file.d" 2>/dev/null; do
    kill -0 "$(cat "$file.d/pid" 2>/dev/null || echo 0)" 2>/dev/null || { rm -rf "$file.d"; continue; }
    sleep 0.2
  done
  echo $$ > "$file.d/pid"
  "$@"; rc=$?
  rm -rf "$file.d"
  return $rc
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

ns_pin_locked() {  # TAG FILE
  local id
  id=$(awk -F'\t' -v t="$1" '$1 == t { print $2 }' "$2" 2>/dev/null)
  if [ -z "$id" ]; then
    ns_docker image inspect "$1" >/dev/null 2>&1 || ns_docker pull -q "$1" >/dev/null 2>&1 || { printf '%s' "$1"; return; }
    id=$(ns_docker image inspect -f '{{.Id}}' "$1") || { printf '%s' "$1"; return; }
    mkdir -p "$(dirname "$2")" && printf '%s\t%s\n' "$1" "$id" >> "$2"
  fi
  printf '%s' "$id"
}

ns_image() {
  case $1 in sha256:*) printf '%s' "$1"; return ;; esac
  local file
  file=$(ns_pins_file "$2")
  ns_locked "$file.lock" ns_pin_locked "$1" "$file"
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
