# The benchmark suite

One list of workloads, one list of systems, one rule for which pair runs, and one driver that runs them: a workload runs on a system that has every capability the workload needs, and is skipped, with the reason, on the others.

```sh
benches/suite/suite.py                     # the matrix, and what NRESE can't run yet
benches/suite/suite.py graphdb             # one system, with the reason for every skip
benches/suite/suite.py --check             # validate the two files
benches/suite/suite.py run --dry-run       # every step of a full run, printed, nothing started
benches/suite/suite.py run                 # run it: three repetitions per pair
benches/suite/suite.py run --systems nrese,qlever --workloads basics-mix --tier basics-mix=yago-tiny
benches/suite/suite.py report benches/suite/results/<date>/results.csv
benches/suite/suite.py compare benches/suite/results/<base> benches/suite/results/<new>   # regressions beyond noise
benches/suite/suite.py ledger benches/suite/results/<date>   # the run's record in ../runs/
benches/suite/suite.py status --write      # ../STATUS.md: what is measured and what isn't
benches/suite/suite.py images              # the images the systems need
```

Where this directory sits among the kits, and the life of a run from plan to record: [../README.md](../README.md). The rules every run follows: [../PROTOCOL.md](../PROTOCOL.md).

| File | What |
|---|---|
| `systems.toml` | The systems, their capabilities, whether their results may be published, and how the suite can start them |
| `workloads.toml` | The workloads: what each measures, what it needs, where it comes from, its licence, how a result is checked, and whether the repository runs it today |
| `suitekit/runtime.py` | Where a system runs: Docker, Apptainer (SIF images, for Draco) or a host process; wall time, peak memory, exit code; the cleanup |
| `suitekit/adapters.py` | One adapter per system: load (reasoning included where the system reasons at load), serve, stop; the regimes it runs |
| `suitekit/workloads.py` | Per workload and tier: the inputs, queries, expected answer counts, regime preference, and how the data is made |
| `suitekit/schema.py` | The one result schema |
| `suitekit/manifest.py` | `manifest.json` next to the results: commit, machine, Docker's limits, protocol, image ids, dataset sizes |
| `suitekit/driver.py`, `suitekit/report.py`, `suitekit/compare.py` | `suite.py run`, `report` and `compare` |
| `suitekit/ledger.py`, `suitekit/status.py` | `suite.py ledger` (run records, ../runs/) and `status` (../STATUS.md) |

## A run

For every workload tier and every system that can run it, `--runs` times (default 3), each from a fresh store, the repetitions interleaved across the systems (every system's first run, then every system's second, in a rotated order):

1. **load**, and reasoning where the system reasons at load (NRESE, GraphDB, RDFox, Nemo, owlrl); wall time and peak memory
2. **size** of the store on disk; **restart**: the server on the loaded store, until it answers
3. **count** of every statement it answers with; lazy reasoners (Jena's rule reasoners) do their work here
4. **queries** through the harness's `query-mix`: one warm-up, then `--query-runs` measured runs each (default 3); answer counts checked against the expected ones where the workload has them, and cross-checked between systems within one regime at the end. Each query's first execution on the fresh server is reported apart (`repeat` 0)
5. **serve**: the server's peak memory over the run

Then its containers, store and scratch are removed. The run's `manifest.json` records what it was measured with (`suitekit/manifest.py`). At the end of the run, what the run made is removed too: the datasets it prepared, the NRESE build volume, images it pulled or built. `--keep` (or `NRESE_BENCH_KEEP=1`) keeps them for the next run of a batch; installed tools (`install-tools.sh`) always stay. A dry run prints every command and writes nothing that stays.

**Caches and order: every system's strengths count.** No system's advantage is switched off to level the field. Systems with a result cache (QLever, NRESE) run the queries twice: with the cache off (repeated runs measure evaluation), then, after a restart that empties it, with the system's default cache (repeated runs measure what a user sending the same query again sees). Both lines are reported (`--cache off,on`, the default; `off` or `on` alone). Queries run in rounds over the whole mix, each round shuffled (`--order shuffled`, the default; `--order fixed` runs each query's runs back to back). Repetition r uses seed `seed·1000 + r` (`--seed`, default 1), the same orders for every system, recorded in the `order` column. The first execution of each query is reported apart: on a fresh server, the cold case.

**Regimes.** A reasoning workload names the regimes it accepts, in order of preference (LUBM: OWL 2 RL, else OWL-Horst); each system runs the first it has. Results are compared within one regime only.

**NRESE on Oxigraph** (`nrese-oxigraph`) is the same server built from the branch `baseline/pre-oxigraph-migration`, the last commit before the migration to the own RDF libraries. The suite builds it from a checkout beside the repository (`git worktree add ../OWL-RS-baseline baseline/pre-oxigraph-migration`, or `NRESE_OXIGRAPH_SRC`) into its own build volume. Next to `nrese` it shows what the migration changed.

**Systems without SPARQL** (Nemo, the owlrl reference closure) have their closure answered by Oxigraph: their answer counts check the others, their query times aren't theirs and are left empty.

**Licensed systems** run only with their licence files: `graphdb.license` and `RDFox.lic` in the licences directory (`~/nrese-bench/licenses`, outside the repository; `NRESE_LICENSES` elsewhere), or one by one through `GRAPHDB_LICENSE` and `RDFOX_LICENSE`. Their rows carry `publish = permission`; `suite.py report` leaves them out unless `--include-restricted`, and they may not leave the machine without the vendor's written consent ([../competitors/README.md](../competitors/README.md)). The RDFox adapter and GraphDB with a ruleset haven't run yet (no licence): the first licensed run confirms their commands.

## The result schema

One CSV row per measured step (`suitekit/schema.py`): `date, host, runtime, system, version, publish, workload, tier, regime, run, task, item, repeat, status, ms, peak_mib, rows, bytes, note, cache, order`. `task` is load, reason, size, restart, count, query, update, serve or conformance; `status` ok, failed, timeout, wrong or skipped. A pair that can't run is one `skipped` row with the reason.

## Settings

| Variable | Default | |
|---|---|---|
| `JAVA_HEAP` | 16g | JVM systems |
| `DOCKER_MEMORY` | 90 % of Docker's memory | a hard memory cap per container, without swap (`0`: none); a run past it is killed in its container, not the host made to thrash |
| `QUERY_MEMORY_MIB` | NRESE's default | NRESE's per-query memory budget |
| `JENA_OWL_REASONER` | `OWLMicroFBRuleReasoner` | Jena's rule reasoner for OWL-Horst workloads |
| `RGGS_REPO`, `ONTOLOGY` | | the integration workload's checkout, and `gndo` for the GND ontology |
| `*_IMAGE` | see adapters.py | the image of a system (`QLEVER_IMAGE`, `GRAPHDB_IMAGE`, …) |
| `NRESE_BIN`, `HARNESS`, `CARGO_TARGET_DIR` | the repository's `target/release` | prebuilt binaries |
| `NRESE_BENCH_SCRATCH` | `tmp/` | stores and scratch |

## On a cluster

`--runtime apptainer --sif-dir DIR --data DIR`: the systems run from SIF images (`../cluster/build-sif.sh` converts them), NRESE and the client as host processes, datasets from a directory. `../cluster/suite.sbatch` is the SLURM job ([../cluster/README.md](../cluster/README.md)).

## What isn't in the driver

Workloads without a plan in `suitekit/workloads.py` are listed as not run. Which ones they
are, and which pairs have never run, is in [../STATUS.md](../STATUS.md) ("Gaps"). The kits
next to this directory keep their own drivers for what they do beyond the suite
([../README.md](../README.md), "Kits"), and they write their run records by hand
([../runs/README.md](../runs/README.md)).

**The order of work** (owner decision, 30 September 2026): NRESE implements and tunes every capability first; the comparison runs come after.
