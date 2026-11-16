# Benchmarks on a SLURM cluster

For the runs a paper needs: a whole node that nothing else uses, enough memory for the systems that need it, and a record of what ran where. The scripts assume only SLURM and a shared file system.

As of 30 September 2026, the scripts were run on a Linux workstation without SLURM (`bash benches/cluster/integration.sbatch nrese example`). They have not run under SLURM: there is no cluster account yet. Expect to adjust partition names and paths on first use.

## What the cluster needs

- Standard nodes of a few dozen cores and a few hundred GB, taken whole.
- Large-memory nodes (terabytes) for the systems that reason in memory, in a partition of their own.
- SLURM, environment modules and Apptainer (no Docker on the nodes).
- A parallel file system for data and results (`/work` below), apart from `/home`.

Check `sinfo` and the cluster's own documentation for its partitions, time limits and storage. The scripts ask for a partition named `standard`; `sbatch -p` names another.

Rules that shape the scripts: no computation and no heavy I/O on the login nodes (builds run in jobs), and a job is killed when it exceeds the memory it asked for.

## First use

```sh
# on a login node
git clone <this repository> ~/OWL-RS && cd ~/OWL-RS     # or copy a checkout with rsync
benches/cluster/setup.sh                                # toolchain, dependencies, the workload's repository, Fuseki
sbatch benches/cluster/integration.sbatch nrese example # the first job also builds NRESE
squeue -u $USER
cat /work/$USER/nrese/results/*/integration.csv
```

The example tier takes seconds. For the project's cohort and full data, build them in the workload's repository first (its `just` recipes), then:

```sh
sbatch --export=ALL,TIER_NAME=cohort benches/cluster/integration.sbatch nrese current
sbatch --export=ALL,TIER_NAME=cohort,FUSEKI_HEAP=200g benches/cluster/integration.sbatch fuseki-owl current
sbatch -p <large-memory partition> --export=ALL,TIER_NAME=full,FUSEKI_HEAP=2000g benches/cluster/integration.sbatch fuseki-owl current
```

## The whole suite

`suite.sbatch` runs the benchmark suite ([../suite](../suite/README.md)) in a job: the systems from SIF images, NRESE and the client as processes, the datasets from a directory.

```sh
# on a workstation with Docker: the datasets, and the images this repository builds
benches/reasoning/prepare-lubm.sh 100 1000
docker run --rm -v nrese-bench-data:/data:ro -v "$PWD/tmp/cluster-data":/out alpine sh -c 'cp /data/*.nt /out/'
mkdir -p tmp/cluster-sif
for image in nrese-bench/jena:6.2.0 nrese-bench/nemo nrese-bench/owlrl-oracle; do
  docker save "$image" -o "tmp/cluster-sif/$(echo "$image" | tr '/:' '__').tar"
done
rsync -a tmp/cluster-data/ cluster:/work/$USER/nrese/data/
rsync -a tmp/cluster-sif/ cluster:/work/$USER/nrese/sif/

# on the cluster
benches/cluster/build-sif.sh /work/$USER/nrese/sif        # the SIF files: from the archives, the rest from their registries
sbatch benches/cluster/suite.sbatch --workloads lubm --tier lubm=100 --tier lubm=1000
PYTHON=python3.11 sbatch ...                             # the suite needs Python 3.11 (module avail python)
```

The suite's Apptainer path was run in WSL (Ubuntu 24.04, Apptainer 1.5.4) with SIF images built from the local Docker images.

## How a run is set up

- Each run has a whole node (`--exclusive --mem=0`): no other job shares its cores, memory or memory bandwidth. One system at a time; systems are compared across jobs on the same node type, which `environment.txt` records.
- Runs use one hardware thread per core (`--hint=nomultithread`), as the cluster's documentation recommends for threaded programs.
- There are no containers for NRESE, Fuseki and the client: they run as processes. Peak memory is read from the processes; SLURM's accounting (`sacct.txt`) is kept next to it.
- The other systems run in containers: the cluster has Apptainer, not Docker. The suite runs them from SIF images (below). Java without a module: `JAVA="apptainer exec temurin-21.sif java"`.
- Results go in `$NRESE_WORK/results/<date>-job<id>/` (they stay), the build in `$NRESE_WORK/target`, the store and temporary files in `$NRESE_WORK/scratch/<job id>/` (removed when the run ends). Nothing large in `/home`.
- `environment.txt` records job, node, partition, CPU model and counts, memory, kernel, the commits of NRESE and of the workload, the toolchain, the arguments.

## Before a number is published

- Licensed systems: written permission from the vendor, recorded in [../competitors/README.md](../competitors/README.md). Their licence files stay outside the repository.
- The workload: the agreement of its authors, and a licence or citation for the queries.
- At least three jobs per system and tier, on the same node type; report the median and the spread.
- Ask the computing centre whether and how the cluster is to be acknowledged in a publication.
