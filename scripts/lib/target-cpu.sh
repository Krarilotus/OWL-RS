# The CPU builds generate code for (sourced by scripts that compile).
#
# NRESE_TARGET_CPU selects it:
# - native (the default): the CPU of the machine that builds, which is where the binary
#   runs for local use, the perf lab and the benchmark containers. Every instruction set
#   it has (AVX2, AVX-512, BMI2, …) is available to the compiler.
# - portable: the architecture's baseline (x86-64 or aarch64), for a binary that ships to
#   unknown machines.
# - any CPU name rustc knows: x86-64-v2, x86-64-v3 (AVX2, the common server level),
#   x86-64-v4 (AVX-512), znver4, neoverse-v1, …, for a known fleet.
#
# A binary built for a CPU that has more than the one it runs on stops at start with a
# message naming the missing features (nrese-server checks them), never with an illegal
# instruction halfway through a query.
#
# Building for a CPU above the building machine's (x86-64-v4 on a machine without AVX-512)
# needs `--target <triple>` (x86_64-unknown-linux-gnu): without it, cargo applies RUSTFLAGS
# to the build scripts too, and they stop with an illegal instruction on the machine that
# builds. With it, the binary lands in target/<triple>/release.

# Prints the rustc flag for NRESE_TARGET_CPU, or nothing for `portable`.
target_cpu_flag() {
  local cpu="${NRESE_TARGET_CPU:-native}"
  case "$cpu" in
    portable | "") ;;
    *) printf -- '-C target-cpu=%s' "$cpu" ;;
  esac
}

# Adds the flag to RUSTFLAGS (keeping what the caller set) and exports it.
export_target_cpu_rustflags() {
  local flag
  flag="$(target_cpu_flag)"
  if [ -n "$flag" ]; then
    case " ${RUSTFLAGS:-} " in
      *"target-cpu="*) ;; # the caller chose one already
      *) export RUSTFLAGS="${RUSTFLAGS:+$RUSTFLAGS }$flag" ;;
    esac
  fi
}
