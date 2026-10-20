# The reasoning scorecard's systems and the image each one's measured containers run: the one
# table its pin set, its runs (measured LOG SYSTEM ...) and its check read, so a system can't be
# timed without its image pinned at the scorecard's start (scripts/lib/ns.sh). Source it after
# RUST_IMAGE is set.
#
#   REASONING_SYSTEMS        every system the scorecard can run
#   system_image SYSTEM      its image (fails for a system not in the table)
#   pin_set SYSTEM...        the images of those systems, the Rust image's first (the build's)

REASONING_SYSTEMS="oracle nrese-v2 nemo jena-rdfs jena-owl-micro jena-owl-mini jena-owl"

system_image() {
  case $1 in
    oracle) echo nrese-bench/owlrl-oracle ;;
    nrese-v*) echo "$RUST_IMAGE" ;;
    nemo) echo nrese-bench/nemo ;;
    jena-rdfs|jena-owl-micro|jena-owl-mini|jena-owl) echo nrese-bench/jena-reasoner ;;
    *) echo "no image for the system $1 (benches/reasoning/systems.sh)" >&2; return 1 ;;
  esac
}

pin_set() {
  local system
  echo "$RUST_IMAGE"
  for system in "$@"; do system_image "$system" || return 1; done
}
