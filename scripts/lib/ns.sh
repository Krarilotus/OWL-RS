# The job namespace for shell scripts: the twin of benches/suite/suitekit/ns.py, which says why
# (one job's cleanup removed another's volumes, 19 Oct 2026). Source it; every Docker container
# and volume name, and every label, comes from here (scripts/check-names.sh fails otherwise).
#
#   ns_namespace ROOT            FLEET_JOB when set, else c + a digest of the checkout's path
#   ns_name KIND ROOT            nrese-<kind>-<namespace>-<checkout digest>
#   ns_run_args NAME ROOT OWNER  sets NS_ARGS: --name NAME and the labels, for `docker run`
#   ns_volume create NAME ROOT OWNER | rm NAME | inspect NAME   a `docker volume` command
#   ns_shared KIND               a host-wide resource jobs share on purpose: data, cargo
#   ns_clean ROOT [OWNER] [all]  removes this namespace's containers and volumes: the checkout's
#                                own (or `all`: the whole job's), of OWNER or of every owner
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
  return 0
}
