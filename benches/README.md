# Benchmarks

Everything NRESE is measured with: what is measured, how, against which systems, and what
has been measured so far. Start here.

| Question | Answer |
|---|---|
| What is measured, what isn't, and how did it last come out? | [STATUS.md](STATUS.md), generated: `python benches/suite/suite.py status --write` |
| How is it measured, and why can the numbers be trusted? | [PROTOCOL.md](PROTOCOL.md): the rules every kit follows |
| Which benchmarks exist in each field, what do they cost, and which do we run? | [CATALOG.md](CATALOG.md) |
| What did a given run cover, on which commit and machine? | [runs/](runs/README.md): one record per run |
| Which workloads and systems exist, and which pair can run? | [suite/workloads.toml](suite/workloads.toml), [suite/systems.toml](suite/systems.toml); `suite.py` prints the matrix |
| How do I run something? | §"A run, step by step" below |
| Did a change make NRESE slower? | `fast/fast.py run` then `fast/fast.py compare BASE NEW` (the fast suite, under an hour); `suite.py compare BASE NEW` (suite runs), `perf-lab-compare.py` (perf lab), the kits' own `compare` |

## The hierarchy

```text
benches/
├── README.md            this map
├── PROTOCOL.md          the rules: fairness, correctness first, repetitions, statistics, records, publishing
├── CATALOG.md           the benchmarks per field: what each measures, its cost, core / representative / later
├── STATUS.md            generated: coverage matrix, tier tables, gaps, problems, runs
├── runs/                one record per run: scope, commit, machine, protocol, outcome per pair (committed)
├── baselines/           committed NRESE reference numbers the regression gates compare against
├── fast/                the fast suite: many small cases, checked and route-checked, under an hour
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

The 9 October inventory contains **95 fast cases** and **24 registered workloads**:
10 ready, four partial and 10 planned. The `fast` registry entry bridges those same
95 cases; it is not another independent campaign. The 20 system entries include
build/rule variants and unavailable adapters. `ready` describes runnable machinery,
not a completed or successful workload × system × tier measurement.

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
| [fast](fast/README.md) | many small cases (10-300 s each, under an hour): every reasoning profile, maintenance, DL, memory caps, query shapes and operators, services; checks and route checks before times; home-turf cases per competitor | `fast/fast.py run`, `compare`, `compete` | `~/nrese-bench/reports/fast/` (full reports), `tmp/fast/`, `fast/results/` | `baselines/fast/*.json` (compact records) |
| perf lab | the store and query engine without HTTP, per query p50; the fast suite's measuring tool | `perf-lab.sh` | — | `baselines/perf-lab/*.json` |
| [probes](probes/README.md) | one property at a time | one script each | stdout | — |
| [cluster](cluster/README.md) | the suite on Draco | `suite.sbatch` | the job's directory | record in `runs/` |

The fast suite covers reasoning, maintenance, DL, memory, load, query operators,
services, kernels, layout, rules, parsing, planning, concurrency and caches. Its
10–300 seconds per case / under-an-hour description is a design budget, not a
verified duration for the current full campaign. Some cases have no independent
answer check; consult the case's checks before treating a timing as validated.

Correctness has additional owners: crate conformance tests cover RDF, SPARQL, OWL,
SHACL and GeoSPARQL; the Jena oracle compares SPARQL result content; EL and DL kits
compare canonical reasoning answers. General cross-system row counts alone do not
prove equal bindings. QL/OBDA's [NPD probes](reasoning/queries/npd-stress/README.md)
compare Ontop certain-answer counts and stress rewriting; these are not a separate
completed scale campaign. The HTTP harness and perf lab are shared measuring tools;
probes diagnose individual properties, and the cluster directory supplies an
execution wrapper rather than evidence of distributed performance.

The registry still marks integration, ORE, GeoSPARQL and the fast competitor bridge
partial. LDBC SPB, real ontologies, consistency, OWL2Bench classification,
SPARQLoscope, BSBM, WatDiv, ERA SHACL, full-text search and federation remain planned
entries. Existing probes or test coverage in these areas do not complete those
campaigns. [Run records](runs/README.md) identify actual coverage and known disputes.

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

## Remote hosts and shared data

Transfers from **KRARIS-GPTLER are underway** (9 October 2026). The Windows
`nrese-data` and `nrese-bench` snapshots and Docker `nrese-fast-data` snapshot have
passed full source/destination SHA-256 verification on both hosts; their local
content has been retired. Other groups still require their own completed proof.
Office-PC alone builds/tests/formats Rust and executes build-triggering hooks.
Phuoc-Yu runs verified Office-built native binaries; it currently has no Docker,
Java or Apptainer. The kit table above and [CATALOG.md](CATALOG.md) remain the suite
overview; host availability does not imply every kit can run there.

Both hosts preserve source snapshots under
`/home/krarilotus/nrese-assets/kraris-gptler-20261009/snapshots`: `windows/nrese-data`,
`windows/nrese-bench`, `docker-volumes/nrese-bench-data`, `docker-volumes/nrese-fast-data`,
`docker-volumes/nrese-classify-data`, and the transferred build volumes. Check the
transfer manifest for the actual layout. These are immutable preservation copies,
not writable data, build or cleanup targets. Keep existing remote campaign paths;
do not merge same-named files/volumes or assume their contents match.

Before selecting an input, verify the completed transfer on **each host** against
the source inventory (relative paths, sizes and checksums), recording missing files
and mismatches. Map each workload to its verified input directory explicitly. Share
that directory across worktrees, read-only; keep generated data, stores, scratch and
results outside preservation roots. Local-copy cleanup waits for verified remote
preservation and rerun needs to be settled; transfer progress alone is insufficient.

Both hosts now have `~/.config/nrese-bench.env` (mode 0600), explicitly sourced rather
than autoloaded. It names the existing checkout/campaign paths, Python interpreter,
shared Windows input snapshot (`BENCH_DATA`), fast snapshot (`BENCH_FAST_DATA`),
results parent (`BENCH_RESULTS_ROOT`), mutable scratch (`NRESE_BENCH_SCRATCH`),
native binaries (`NRESE_BIN`/`HARNESS`) and preserved licences (`NRESE_LICENSES`).
The `BENCH_*` names are shell conveniences, not new suite settings. The current
profiles identify measured binaries from `77478d1` through `BENCH_BINARY_REVISION`;
the checkout revision can differ and must be recorded separately. They do not select
latest-source binaries or start runs. For the verified LUBM 1 input directory:

```sh
ssh Phuoc-Yu 'bash -se' <<'REMOTE'
source "$HOME/.config/nrese-bench.env"
: "${BENCH_PYTHON:?}" "${BENCH_CHECKOUT:?}" "${BENCH_DATA:?}" "${BENCH_RESULTS_ROOT:?}"
: "${NRESE_BENCH_SCRATCH:?}" "${NRESE_BIN:?}" "${HARNESS:?}"
BENCH_RESULTS="$BENCH_RESULTS_ROOT/$(date -u +%Y%m%dT%H%M%SZ)-$$"
test ! -e "$BENCH_RESULTS"
cd "$BENCH_CHECKOUT"
test -f "$NRESE_BIN"
test -x "$NRESE_BIN"
test -f "$HARNESS"
test -x "$HARNESS"
test -s "$BENCH_DATA/univ-bench.nt"
test -s "$BENCH_DATA/lubm-1.nt"
"$BENCH_PYTHON" benches/suite/suite.py run --runtime process --systems nrese \
  --workloads lubm --tier lubm=1 --data "$BENCH_DATA" \
  --skip-build --keep --results "$BENCH_RESULTS" --dry-run
REMOTE
```

Review the printed commands before repeating without `--dry-run`; the same template
can target Office-PC. `--data DIR` skips missing inputs instead of preparing them.
Docker mounts these inputs read-only; process mode needs host-enforced read-only
permissions. This branch's `--skip-build` refuses a missing harness; Phuoc's frozen
`77478d1` driver predates that guard, so retain the explicit file/executable checks.
`--skip-build` does not suppress Cargo
in kit workloads, so select explicit native workloads on Phuoc-Yu. Verify binary
checksums, source revision, CPU/ABI compatibility and dependencies; record these
alongside results. `--keep` retains reusable assets, not per-run stores/scratch.

The fast driver still hardcodes `DATA_VOLUME`/`BENCH_VOLUME`, writes generated
inputs/references to `/fast`, and `clean` removes its data volume. Do not bind a
preservation snapshot writable there. Its `--out`/`NRESE_BENCH_REPORTS` and
`NRESE_ORE_DIR` knobs do not relocate all data/scratch. Use direct suite invocations
for shared preserved inputs: `batch.sh` and `bench-cleanup.sh` have legacy cleanup
policies, including name-based and low-disk cleanup, and must not manage preservation
copies or existing campaigns. The generic lifecycle below applies to disposable runs.

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
