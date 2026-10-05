# Benchmarks

Everything NRESE is measured with: what is measured, how, against which systems, and what
has been measured so far. Start here.

| Question | Answer |
|---|---|
| What is measured, what isn't, and how did it last come out? | [STATUS.md](STATUS.md), generated: `python benches/suite/suite.py status --write` |
| How is it measured, and why can the numbers be trusted? | [PROTOCOL.md](PROTOCOL.md): the rules every kit follows |
| What did a given run cover, on which commit and machine? | [runs/](runs/README.md): one record per run |
| Which workloads and systems exist, and which pair can run? | [suite/workloads.toml](suite/workloads.toml), [suite/systems.toml](suite/systems.toml); `suite.py` prints the matrix |
| How do I run something? | §"A run, step by step" below |
| Did a change make NRESE slower? | `suite.py compare BASE NEW` (suite runs), `perf-lab-compare.py` (perf lab), the kits' own `compare` |

## The hierarchy

```text
benches/
├── README.md            this map
├── PROTOCOL.md          the rules: fairness, correctness first, repetitions, statistics, records, publishing
├── STATUS.md            generated: coverage matrix, tier tables, gaps, problems, runs
├── runs/                one record per run: scope, commit, machine, protocol, outcome per pair (committed)
├── baselines/           committed NRESE reference numbers the regression gates compare against
├── suite/               the registries and the driver
│   ├── systems.toml     systems, capabilities, publishing rule, how they start
│   ├── workloads.toml   workloads, needs, standard tiers, source, licence, check, state
│   ├── suite.py         CLI: matrix, run, report, compare, ledger, status, images
│   ├── suitekit/        driver, adapters, runtimes, workload plans, schema, manifest, ledger, status, compare
│   ├── batch.sh         several runs from a worktree of one commit, then the cleanup
│   └── results/         raw results, one directory per run (git-ignored)
└── kits                 data, queries and drivers for what the suite's cycle doesn't cover
    ├── reasoning/       LUBM, OWL2Bench: data generators, queries, expected answers, inferred-set checks
    │   ├── dl/          OWL 2 DL: reference reasoners, W3C DL suite, ORE 2015, canonical taxonomies
    │   └── el-classification/  NRESE's EL classifier against ELK on random EL ontologies
    ├── competitors/     the basics datasets and query sets; licence rules; concurrency scorecard
    ├── integration/     RG × GS × GND: the HisQu integration workload
    ├── clients/         RDF4J, Jena, rdflib and DMW clients against NRESE
    ├── oracle/          Jena's ARQ as a second oracle for SPARQL answers
    ├── oxigraph-comparison/  NRESE's RDF bundle against Oxigraph's libraries
    ├── nrese-bench-harness/  the Rust client: query-mix, write-scaling, compatibility packs
    ├── perf-lab.sh, perf-lab-compare.py  NRESE's store and query engine alone, in Docker
    ├── probes/          single measurements: HTTP latency, large results, memory, autocompletion
    └── cluster/         the same suite on a SLURM cluster (Apptainer)
```

**Four layers.** Each layer only depends on the ones above it:

| Layer | What | Where | Changes when |
|---|---|---|---|
| 0. Rules | how anything is measured, checked, recorded and published | `PROTOCOL.md`, `competitors/README.md` (licences) | rarely, by decision |
| 1. Registries | what exists: systems with capabilities, workloads with needs and standard tiers | `suite/*.toml` | a system or workload is added, a capability lands |
| 2. Drivers | how a pair runs: the suite's cycle (load → size → restart → count → queries → serve) or a kit's own driver | `suite/suitekit/`, the kits | a new kind of measurement |
| 3. Evidence | what came out: raw results (local), run records (committed), STATUS (generated), baselines (committed) | `suite/results/`, `runs/`, `STATUS.md`, `baselines/` | every run |

## Kits

| Kit | Measures | Driver | Raw results | Committed |
|---|---|---|---|---|
| [suite](suite/README.md) | every workload × system pair the cycle can run: load, size, restart, count, queries, memory | `suite.py run`, `batch.sh` | `suite/results/<run>/` | record in `runs/` |
| [reasoning](reasoning/README.md) | inferred sets fact by fact against a reference closure (precision, recall); the LUBM and OWL2Bench data | `reasoning-scorecard.sh` | `reasoning/results/` | — |
| [reasoning/dl](reasoning/dl/README.md) | OWL 2 DL correctness and classification speed; the reference reasoners | `reference.py`, `w3c.py`, `ore.py`, `nrese.py` | `reasoning/dl/results/` | the TSVs (free systems only) and a record |
| [reasoning/el-classification](reasoning/el-classification/README.md) | NRESE's EL classifier against ELK on random ontologies | `run_elk.sh`, `el_check.py` | scratch | — |
| [competitors](competitors/README.md) | throughput with concurrent clients, writes under read load; the datasets of the basics mix | `scorecard.sh` | `competitors/results/` | the run records in `runs/` |
| [integration](integration/README.md) | the RG × GS × GND workload | its scripts; the suite's `integration-rg-gs-gnd` | `integration/results/` | — |
| [clients](clients/README.md) | client libraries end to end | `run-all.sh` | stdout | — |
| [oracle](oracle/README.md) | NRESE's SPARQL answers against Jena's | its script | `oracle/results/` | — |
| [oxigraph-comparison](oxigraph-comparison/README.md) | parsers, serialisers and evaluators against Oxigraph's | `cargo test`, `cargo run --bin bench` | `oxigraph-comparison/results/` | — |
| perf lab | the store and query engine without HTTP, per query p50 | `perf-lab.sh` | — | `baselines/perf-lab/*.json` |
| [probes](probes/README.md) | one property at a time | one script each | stdout | — |
| [cluster](cluster/README.md) | the suite on Draco | `suite.sbatch` | the job's directory | record in `runs/` |

## Lab tools

Single measurements and diagnoses outside the kits, run with
`scripts/cargo-guarded.sh run --release -p <crate> --example <name> -- …` (each file's
header says how). `benches/probes/` has the HTTP-level probes.

| Area | Examples |
|---|---|
| Store, load, restart | `nrese-store`: `perf_lab` (the perf lab), `restart`, `reason_query`, `support_sets`, `vector_search`; `nrese-engine`: `insert_latency`, `image` (a store image in the current format); `nrese-rdf-io`: `parse_parallel` (why a parallel load rejects a file) |
| Rule reasoning | `nrese-reasoner`: `v2_closure` (closures for the oracle), `v2_delta` (commit latency against rematerialisation), `graph_sets`, `classify` (EL, against ELK) |
| DL | `nrese-dl`: `tableau_consistency` (per-phase times, search counters), `context_classify`, `bounds_eval` (the L and U1 bounds), `tableau_fuzz`; `nrese-owl`: `fuzz` (random OWL 2 DL ontologies), `ofn_speed` (the functional-syntax reader's speed gate) |
| Studies kept for open targets | `nrese-engine`'s ignored tests `compression_study` and `vocabulary_study` (store-size options, P2 in [performance.md](../docs/design/performance.md) §6) |
| Office PC | `scripts/office/`: dataset preparation (`dbpedia.sh`, `wikidata.sh`) and the perf lab there (`perflab.sh`) |

## A run, step by step

```text
plan       suite.py status                      what is missing; pick workloads, tiers, systems
           suite.py run --dry-run ...           every step printed, nothing started
run        suite/batch.sh NAME -- ARGS [-- ARGS]  from a worktree of HEAD; writes suite/results/NAME/
                                                  results.csv   one row per measured step (suitekit/schema.py)
                                                  manifest.json commit, machine, Docker limits, protocol, images, dataset sizes
                                                  logs/         per pair and repetition
check      suite.py report suite/results/NAME   tables (restricted systems left out)
           suite.py compare BASE NEW            regressions and changed answers against a baseline run
record     suite.py ledger suite/results/NAME   writes runs/NAME.toml; then fill in purpose, baseline, findings
           suite.py status --write              regenerates STATUS.md
commit     runs/NAME.toml and STATUS.md         never the raw results
clean      batch.sh: containers, stores, scratch go; datasets stay until the results are concluded
           scripts/bench-cleanup.sh             then removes the datasets (batch.sh does it at once below 100 GB free)
```

A kit with its own driver ends the same way: its results where its README says, and a
record in `runs/` written by hand in the same format, with `kit` set to the kit's directory.

## Adding things

- **A system:** an entry in `suite/systems.toml` (capabilities from its documentation, `publish`, `runs`), and an adapter in `suite/suitekit/adapters.py`. Its first run confirms the capabilities; a capability that doesn't work is removed, with a note.
- **A workload:**
  1. An entry in `suite/workloads.toml`: `needs`, `tiers`, `source`, `licence`, `check`, `state = "planned"`.
  2. Its data and queries in a kit.
  3. A plan in `suite/suitekit/workloads.py` (then `state = "ready"`), or a kit driver that writes a run record.
- **A capability:** add it to `CAPABILITIES` in `suite.py` and to the vocabulary at the top of `systems.toml`.

`suite.py --check` validates the registries. The pre-commit hooks don't run benchmarks:
benchmarks run in bulk at the end of a batch of work (PROTOCOL.md §8).
