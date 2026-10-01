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
benches/suite/suite.py images              # the images the systems need
```

| File | What |
|---|---|
| `systems.toml` | The systems, their capabilities, whether their results may be published, and how the suite can start them |
| `workloads.toml` | The workloads: what each measures, what it needs, where it comes from, its licence, how a result is checked, and whether the repository runs it today |
| `suitekit/runtime.py` | Where a system runs: Docker, Apptainer (SIF images, for Draco) or a host process; wall time, peak memory, exit code; the cleanup |
| `suitekit/adapters.py` | One adapter per system: load (reasoning included where the system reasons at load), serve, stop; the regimes it runs |
| `suitekit/workloads.py` | Per workload and tier: the inputs, queries, expected answer counts, regime preference, and how the data is made |
| `suitekit/schema.py` | The one result schema |
| `suitekit/driver.py`, `suitekit/report.py` | `suite.py run` and `suite.py report` |

## A run

For every workload tier and every system that can run it, `--runs` times (default 3), each from a fresh store:

1. **load**, and reasoning where the system reasons at load (NRESE, GraphDB, RDFox, Nemo, owlrl); wall time and peak memory
2. **size** of the store on disk; **restart**: the server on the loaded store, until it answers
3. **count** of every statement it answers with; lazy reasoners (Jena's rule reasoners) do their work here
4. **queries** through the harness's `query-mix`: one warm-up, then `--query-runs` measured runs each (default 3); answer counts checked against the expected ones where the workload has them, and cross-checked between systems within one regime at the end
5. **serve**: the server's peak memory over the run

Then its containers, store and scratch are removed. At the end of the run, what the run made is removed too: the datasets it prepared, the NRESE build volume, images it pulled or built. `--keep` (or `NRESE_BENCH_KEEP=1`) keeps them for the next run of a batch; installed tools (`install-tools.sh`) always stay. A dry run prints every command and writes nothing that stays.

**Regimes.** A reasoning workload names the regimes it accepts, in order of preference (LUBM: OWL 2 RL, else OWL-Horst); each system runs the first it has. Results are compared within one regime only.

**NRESE on Oxigraph** (`nrese-oxigraph`) is the same server built from the branch `baseline/pre-oxigraph-migration`, the last commit before the migration to the own RDF libraries. The suite builds it from a checkout beside the repository (`git worktree add ../OWL-RS-baseline baseline/pre-oxigraph-migration`, or `NRESE_OXIGRAPH_SRC`) into its own build volume. Next to `nrese` it shows what the migration changed.

**Systems without SPARQL** (Nemo, the owlrl reference closure) have their closure answered by Oxigraph: their answer counts check the others, their query times aren't theirs and are left empty.

**Licensed systems** run only with their licence files: `graphdb.license` and `RDFox.lic` in the licences directory (`~/nrese-bench/licenses`, outside the repository; `NRESE_LICENSES` elsewhere), or one by one through `GRAPHDB_LICENSE` and `RDFOX_LICENSE`. Their rows carry `publish = permission`; `suite.py report` leaves them out unless `--include-restricted`, and they may not leave the machine without the vendor's written consent ([../competitors/README.md](../competitors/README.md)). The RDFox adapter and GraphDB with a ruleset haven't run yet (no licence): the first licensed run confirms their commands.

## The result schema

One CSV row per measured step (`suitekit/schema.py`): `date, host, runtime, system, version, publish, workload, tier, regime, run, task, item, repeat, status, ms, peak_mib, rows, bytes, note`. `task` is load, reason, size, restart, count, query, update, serve or conformance; `status` ok, failed, timeout, wrong or skipped. A pair that can't run is one `skipped` row with the reason.

## Settings

| Variable | Default | |
|---|---|---|
| `JAVA_HEAP` | 16g | JVM systems |
| `DOCKER_MEMORY` | none | a hard memory cap per container on a shared machine |
| `QUERY_MEMORY_MIB` | NRESE's default | NRESE's per-query memory budget |
| `JENA_OWL_REASONER` | `OWLMicroFBRuleReasoner` | Jena's rule reasoner for OWL-Horst workloads |
| `RGGS_REPO`, `ONTOLOGY` | | the integration workload's checkout, and `gndo` for the GND ontology |
| `*_IMAGE` | see adapters.py | the image of a system (`QLEVER_IMAGE`, `GRAPHDB_IMAGE`, …) |
| `NRESE_BIN`, `HARNESS`, `CARGO_TARGET_DIR` | the repository's `target/release` | prebuilt binaries |
| `NRESE_BENCH_SCRATCH` | `tmp/` | stores and scratch |

## On a cluster

`--runtime apptainer --sif-dir DIR --data DIR`: the systems run from SIF images (`../cluster/build-sif.sh` converts them), NRESE and the client as host processes, datasets from a directory. `../cluster/suite.sbatch` is the SLURM job ([../cluster/README.md](../cluster/README.md)).

## What isn't in the driver

The workloads without a definition in `suitekit/workloads.py` are listed as not run: LDBC SPB, Sparqloscope, BSBM, WatDiv, ERA-SHACL, ORE 2015, the W3C OWL 2 RL tests, DBpedia and YAGO with their ontologies, injected contradictions, keyword search, GeoSPARQL, federation (completion plan 3.4). The kits next to this directory keep their own drivers for what they do beyond the suite:

| Kit | Runs |
|---|---|
| [../reasoning](../reasoning/README.md) | the inferred sets compared fact by fact with the reference (precision, recall) |
| [../integration](../integration/README.md) | Fuseki as the integration project configures it |
| [../competitors](../competitors/README.md) | throughput with concurrent clients and writes under read load |
| [../nrese-bench-harness](../nrese-bench-harness) | the client, write scaling, compatibility packs |

**The order of work** (owner decision, 30 September 2026): NRESE implements and tunes every capability first; the comparison runs come after. The audit behind these files is [docs/reviews/2026-09-30-benchmark-suite-and-readiness-audit.md](../../docs/reviews/2026-09-30-benchmark-suite-and-readiness-audit.md).
