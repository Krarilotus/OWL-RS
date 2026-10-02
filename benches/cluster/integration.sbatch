#!/bin/bash
# One system on one tier of the integration workload (benches/integration), on a whole
# node of a SLURM cluster. Written for Draco (FSU Jena) from its documentation; see
# README.md for what has and hasn't been run.
#
#   sbatch benches/cluster/integration.sbatch nrese example
#   sbatch -p fat --mem=0 --time=2-00:00:00 benches/cluster/integration.sbatch nrese current
#   sbatch --export=ALL,TIER_NAME=cohort,ONTOLOGY=gndo benches/cluster/integration.sbatch fuseki-owl current
#
# The arguments and the environment are those of benches/integration/run.sh. Set before
# submitting (or in ~/.nrese-cluster.env, which is read if it exists):
#   NRESE_HOME   the OWL-RS checkout           (default: the directory sbatch was run in)
#   RGGS_REPO    the workload's repository     (default: $NRESE_WORK/rgonline)
#   NRESE_WORK   data, results, scratch        (default: /work/$USER/nrese)
#
#SBATCH --job-name=nrese-integration
#SBATCH --partition=standard
#SBATCH --nodes=1
#SBATCH --ntasks=1
# The whole node, so that nothing else runs next to the measurement: all its cores, all
# its memory (--mem=0), one hardware thread per core.
#SBATCH --exclusive
#SBATCH --mem=0
#SBATCH --hint=nomultithread
#SBATCH --time=12:00:00
#SBATCH --output=nrese-integration.%j.out
set -euo pipefail

[ ! -f "$HOME/.nrese-cluster.env" ] || . "$HOME/.nrese-cluster.env"
NRESE_HOME=${NRESE_HOME:-${SLURM_SUBMIT_DIR:-$PWD}}
NRESE_WORK=${NRESE_WORK:-/work/$USER/nrese}
export RGGS_REPO=${RGGS_REPO:-$NRESE_WORK/rgonline}
export RESULTS=${RESULTS:-$NRESE_WORK/results/$(date +%Y-%m-%d)-job${SLURM_JOB_ID:-0}}
# Store and temporary files on the job's own scratch space; the run removes them.
export NRESE_BENCH_SCRATCH=${NRESE_BENCH_SCRATCH:-$NRESE_WORK/scratch/${SLURM_JOB_ID:-$$}}
# The build directory stays in the work area, not in $HOME.
export CARGO_TARGET_DIR=${CARGO_TARGET_DIR:-$NRESE_WORK/target}
export PATH=$HOME/.cargo/bin:$PATH
# Builds take every core the job has (the default is half, for shared workstations).
export CARGO_BUILD_JOBS=${SLURM_CPUS_ON_NODE:-$(nproc)}
mkdir -p "$RESULTS" "$NRESE_BENCH_SCRATCH"
trap 'rmdir "$NRESE_BENCH_SCRATCH" 2>/dev/null || true' EXIT

# What ran where: the record a published number needs.
{
  echo "job ${SLURM_JOB_ID:-none} on $(hostname), partition ${SLURM_JOB_PARTITION:-none}, $(date -Is)"
  echo "nrese $(git -C "$NRESE_HOME" rev-parse HEAD) $(git -C "$NRESE_HOME" status --porcelain | wc -l) uncommitted"
  echo "workload $(git -C "$RGGS_REPO" rev-parse HEAD)"
  echo "arguments: $*; ONTOLOGY=${ONTOLOGY:-project} TIER_NAME=${TIER_NAME:-} RUNS=${RUNS:-3}"
  lscpu | grep -E 'Model name|^CPU\(s\)|Thread|Socket|NUMA node\(s\)'
  free -g | sed -n 2p
  uname -r
  rustc --version
  java -version 2>&1 | head -1 || true
} >"$RESULTS/environment.txt" 2>&1

cd "$NRESE_HOME"
# The binaries land in CARGO_TARGET_DIR, which is where run.sh must look for them.
export NRESE_BIN=$CARGO_TARGET_DIR/release/nrese-server HARNESS=$CARGO_TARGET_DIR/release/nrese-bench-harness
[ -x "$NRESE_BIN" ] || scripts/cargo-guarded.sh build --release --locked -p nrese-server
[ -x "$HARNESS" ] || scripts/cargo-guarded.sh build --release --locked --manifest-path benches/nrese-bench-harness/Cargo.toml
benches/integration/run.sh "$@"

# SLURM's own accounting of the job (peak memory of its steps), where available.
sacct -j "${SLURM_JOB_ID:-0}" --format=JobID,Elapsed,MaxRSS,MaxVMSize,State -P >"$RESULTS/sacct.txt" 2>/dev/null || true
