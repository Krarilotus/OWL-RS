# The benchmark suite

One list of workloads, one list of systems, and one rule for which pair runs: a workload runs on a system that has every capability the workload needs, and is skipped, with the reason, on the others.

```sh
benches/suite/suite.py            # the matrix, and what NRESE can't run yet
benches/suite/suite.py graphdb    # one system, with the reason for every skip
benches/suite/suite.py --check    # validate the two files
```

| File | What |
|---|---|
| `systems.toml` | The systems, their capabilities, whether their results may be published, and how the kits start them |
| `workloads.toml` | The workloads: what each measures, what it needs, where it comes from, its licence, how a result is checked, and whether the repository runs it today |
| `suite.py` | Prints the matrix from the two files. It doesn't run anything yet |

The workloads are run by the kits next to this directory, which predate the suite and each have their own driver:

| Kit | Runs |
|---|---|
| [../reasoning](../reasoning/README.md) | LUBM, OWL2Bench: materialisation and queries under entailment, against an oracle |
| [../integration](../integration/README.md) | The RG × GS × GND integration workload |
| [../competitors](../competitors/README.md) | The basics: load, store size, restart, query mix, throughput |
| [../nrese-bench-harness](../nrese-bench-harness) | The client (`query-mix`), write scaling, compatibility packs |
| [../cluster](../cluster/README.md) | SLURM jobs |

**The order of work** (owner decision, 30 September 2026): NRESE implements and tunes every capability first; the comparison runs come after. `suite.py` lists what is missing. The audit behind these files, with the gaps to close before any published number, is [docs/reviews/2026-09-30-benchmark-suite-and-readiness-audit.md](../../docs/reviews/2026-09-30-benchmark-suite-and-readiness-audit.md).

**Still to build**, in this order: one adapter contract for all kits (`prepare`, `load`, `reason`, `serve`, `stop`, per system, for Docker, Apptainer and plain processes); one result schema (today each kit writes its own CSV); a driver that walks the matrix.
