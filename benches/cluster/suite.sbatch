#!/bin/bash
# The benchmark suite (benches/suite) on a whole node of a SLURM cluster, the systems in
# Apptainer images, NRESE and the client as processes. Written for Draco (FSU Jena); see
# README.md for what has and hasn't been run.
#
#   sbatch benches/cluster/suite.sbatch --workloads lubm --tier lubm=100
#   sbatch -p fat --mem=0 --time=3-00:00:00 benches/cluster/suite.sbatch --systems nrese,jena --workloads lubm --tier lubm=1000
#
# The arguments are those of `suite.py run` (the runtime, SIF directory, data and results
# are set here). Set before submitting (or in ~/.nrese-cluster.env):
#   NRESE_HOME   the OWL-RS checkout           (default: the directory sbatch was run in)
#   NRESE_WORK   data, images, results, scratch (default: /work/$USER/nrese)
#   RGGS_REPO    the integration workload's repository (default: $NRESE_WORK/rgonline)
#   PYTHON       Python 3.11 or newer          (default: python3; see `module avail python`)
#   GRAPHDB_LICENSE, RDFOX_LICENSE             licence files, for the licensed systems
#
# $NRESE_WORK/data holds the datasets (prepared on a workstation: README.md);
# $NRESE_WORK/sif the images (build-sif.sh).
#
#SBATCH --job-name=nrese-suite
#SBATCH --partition=standard
#SBATCH --nodes=1
#SBATCH --ntasks=1
#SBATCH --exclusive
#SBATCH --mem=0
#SBATCH --hint=nomultithread
#SBATCH --time=24:00:00
#SBATCH --output=nrese-suite.%j.out
set -euo pipefail

[ ! -f "$HOME/.nrese-cluster.env" ] || . "$HOME/.nrese-cluster.env"
NRESE_HOME=${NRESE_HOME:-${SLURM_SUBMIT_DIR:-$PWD}}
NRESE_WORK=${NRESE_WORK:-/work/$USER/nrese}
export RGGS_REPO=${RGGS_REPO:-$NRESE_WORK/rgonline}
RESULTS=${RESULTS:-$NRESE_WORK/results/$(date +%Y-%m-%d)-job${SLURM_JOB_ID:-0}}
export NRESE_BENCH_SCRATCH=${NRESE_BENCH_SCRATCH:-$NRESE_WORK/scratch/${SLURM_JOB_ID:-$$}}
export CARGO_TARGET_DIR=${CARGO_TARGET_DIR:-$NRESE_WORK/target}
export PATH=$HOME/.cargo/bin:$PATH
export CARGO_BUILD_JOBS=${SLURM_CPUS_ON_NODE:-$(nproc)}
# The binaries the suite runs as processes (its apptainer path runs NRESE on the host).
export NRESE_BIN=$CARGO_TARGET_DIR/release/nrese-server
PYTHON=${PYTHON:-python3}
mkdir -p "$RESULTS" "$NRESE_BENCH_SCRATCH"
trap 'rm -rf "$NRESE_BENCH_SCRATCH"' EXIT

{
  echo "job ${SLURM_JOB_ID:-none} on $(hostname), partition ${SLURM_JOB_PARTITION:-none}, $(date -Is)"
  echo "nrese $(git -C "$NRESE_HOME" rev-parse HEAD) $(git -C "$NRESE_HOME" status --porcelain | wc -l) uncommitted"
  [ ! -d "$RGGS_REPO/.git" ] || echo "workload $(git -C "$RGGS_REPO" rev-parse HEAD)"
  echo "arguments: $*"
  lscpu | grep -E 'Model name|^CPU\(s\)|Thread|Socket|NUMA node\(s\)'
  free -g | sed -n 2p
  uname -r
  rustc --version
  apptainer --version
  "$PYTHON" --version
  ls "$NRESE_WORK/sif"
} >"$RESULTS/environment.txt" 2>&1

cd "$NRESE_HOME"
# Built in the job, not on the login node; the suite then finds them (--skip-build).
[ -x "$NRESE_BIN" ] || scripts/cargo-guarded.sh build --release --locked -p nrese-server
[ -x "$CARGO_TARGET_DIR/release/nrese-bench-harness" ] ||
  scripts/cargo-guarded.sh build --release --locked --manifest-path benches/nrese-bench-harness/Cargo.toml
"$PYTHON" benches/suite/suite.py run --runtime apptainer --sif-dir "$NRESE_WORK/sif" \
  --data "$NRESE_WORK/data" --results "$RESULTS" --skip-build "$@"

sacct -j "${SLURM_JOB_ID:-0}" --format=JobID,Elapsed,MaxRSS,MaxVMSize,State -P >"$RESULTS/sacct.txt" 2>/dev/null || true
