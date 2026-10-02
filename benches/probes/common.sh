# Usage: source "$(dirname "$0")/common.sh"   (from a probe)
# Shared by the probes: where the repository, the benchmark directory and its scratch
# space are. The defaults are the office PC's layout (~/nrese-bench with the repository in
# it and datasets under scratch/); override them with NRESE_BENCH and NRESE_SCRATCH.
REPO=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
BENCH=${NRESE_BENCH:-$HOME/nrese-bench}
SCRATCH=${NRESE_SCRATCH:-$BENCH/scratch}
[ -f "$HOME/.cargo/env" ] && source "$HOME/.cargo/env"
