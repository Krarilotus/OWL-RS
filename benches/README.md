# Benchmarks

Everything NRESE is measured with: what is measured, how, against which systems, and what
has been measured so far. Start here.

| Question | Answer |
|---|---|
| What is measured, what isn't, and how did it last come out? | [STATUS.md](STATUS.md), generated: `python benches/suite/suite.py status --write` |
| Where does NRESE stand against each contender, on its home turf and elsewhere? | [STANDINGS.md](STANDINGS.md), generated from the run records: `python benches/suite/suite.py standings --write` |
| How is a case written, for any group? | [cases/README.md](cases/README.md): one schema, a file per group |
| How is it measured, and why can the numbers be trusted? | [PROTOCOL.md](PROTOCOL.md): the rules every kit follows |
| Which benchmarks exist in each field, what do they cost, and which do we run? | [CATALOG.md](CATALOG.md) |
| What did a given run cover, on which commit and host? | [runs/](runs/README.md): one record per run |
| Which workloads and systems exist, and which pair can run? | [suite/workloads.toml](suite/workloads.toml), [suite/systems.toml](suite/systems.toml); `suite.py` prints the matrix |
| Which host does what, and where are the verified inputs? | [hosts.toml](hosts.toml) |
| Did a change make NRESE slower? | level 2 below: `fast/fast.py run`, then `fast/fast.py compare BASE NEW` |

## Four levels

Each level answers its own question, at its own cost. A change goes down the levels as far
as its risk needs; a level's evidence never stands in for a lower one's.

| Level | Question | Size | Driver | Runs on |
|---|---|---|---|---|
| 1. Unit | Does each function keep its contract and its work bound? | seconds per crate | `cargo test`; the guards of [performance.md §0](../docs/design/performance.md#0-wins-to-keep-check-these-before-a-rewrite-and-guard-them) (exact counts, plan shapes) | every commit (the gate, [CONTRIBUTING.md](../CONTRIBUTING.md)) |
| 2. Focused | Did this change make its area faster, slower or wrong? | minutes to an hour: the cases of one area, chainable | `fast.py run --cases …` / `--areas …`, then `compare` against the same host's baseline | a `measure` host |
| 3. Full | Where does a build stand, against its base and against the competitors? | everything, up to a day, in checkpointed chunks | every fast case, every suite workload × system, the DL corpora | a `measure` host, one comparison per host |
| 4. Cluster | How does it scale past one machine? | later: the Draco cluster with the Zebratlas graph | the suite under Apptainer (`cluster/`) | `draco` |

A level-2 case is cut to one bottleneck (an operator, a ruleset, a kernel) and checks its
answers and route before its time counts; level 3 runs the same cases and the realistic
workloads in full, with competitors. Native-capacity, matched-resource and frontier
scenarios answer different questions ([PROTOCOL.md §3](PROTOCOL.md#3-fairness)): a run
declares one and reports it apart.

## One model

Every level shares four things, so a case written for level 2 runs unchanged at level 3,
and every run ends in the same record.

- **A case.** One registry entry: its data (an id in `datasets.toml`, with its SHA-256, or a
  generator), its task (queries, load, reason, classify, commits, clients), its regime, the
  systems that may run it, the scenario, what a right answer is (`expect`: an oracle, a
  count, bag, sequence or legal `LIMIT` subset) and its exact work checks (`guards`: routes
  and counters), the metric, a timeout and an estimate of its minutes:

  ```toml
  [[case]]
  id = "op-group"; areas = ["query", "aggregates"]; levels = ["focused", "full"]
  data = "dataset:social-people300000"
  task = "queries:fast/queries/op-group"
  regime = "none"; scenario = "native"
  systems = ["nrese", "qlever", "oxigraph", "jena", "virtuoso"]
  expect = { answers = "oracle:jena", complete = true, order = "bag" }
  guards = { route = { groups = "has group count" }, counters = {} }
  metric = "sum_p50_ms"; timeout_s = 120; est_min = 3
  needs = ["docker"]
  ```

- **A result row.** [suite/suitekit/schema.py](suite/suitekit/schema.py): identity (run,
  case, system, version, host, scenario, item, repeat), then the answer (status, complete,
  rows, answer SHA-256, verdict), then time (ms, phase), memory (peak MiB and its kind:
  lifetime, cgroup, sampled) and disk (bytes), and the case's counters.
- **A run record.** [runs/README.md](runs/README.md): the builds compared, the host, the
  scenario, the inputs and results by SHA-256, a verdict per case.
- **One `compare`.** Answers and completeness first (status and verdict changes), then
  time, then memory by its kind, then disk; lost and missing cases are reported before
  anything else. Its thresholds are in [fast/README.md](fast/README.md#comparing).

Answers are compared by [suite/suitekit/answers.py](suite/suitekit/answers.py)
(`suite.py run --answers full --answers-reference jena,oxigraph,…`): each system against
one reference, never by majority. The reference is the first listed system with a complete,
stable answer, confirmed by the next one or by a known answer (an adjudicated count, a fast
case's own check); two references that disagree leave the query to adjudication.
`strict_match` and `value_match` are kept apart (a `COUNT` typed `xsd:int` is the same
value as `xsd:integer`, and the datatype difference is recorded); an unordered final
`LIMIT` is checked as a legal subset of the reference's unlimited answer. Each distinct
query on the same data is one group, classified `ok`, `wrong` (whose), `incomplete` or
`unsupported`; `suite.py answers DIR...` classifies captured runs again, without running.

### Where this stands

| Step | State |
|---|---|
| Hosts as declared roles: [hosts.toml](hosts.toml), read by `suitekit/hosts.py`: inputs, volumes, preserved image aliases | done |
| Run records in one schema | done |
| The answer recorder in the suite: an adjudicated reference per query, a verdict per group (`suite.py answers`) | done |
| A native runtime for the fast suite: host processes in capped scopes, on a measure host without Docker (`NRESE_FAST_RUNTIME=native`, [fast/README.md](fast/README.md#how-it-measures)) | done |
| One case registry: [cases/](cases/README.md), a file per group, one schema validated by `suite.py --check` (`levels`, `needs`, `expect`, `scenario`, `workload`) | done |
| the fast runner moves into `suitekit` as the native plan kind; `fast.py` becomes `suite.py run --level focused` | next |
| One `compare` over result rows: `suitekit/compare.py` reads fast reports, perf-lab reports and suite results, in the order missing, answers, time, memory by kind, disk; `fast.py compare` and `suite.py compare` are it. Suite pairs still use `summary.py`'s estimator: the bootstrap for them is the next step | started |
| `perf-lab.sh` and `perf-lab-compare.py` retired: the `lab-*` fast cases (rows pinned; the same rows and times within the old runs' spread on Phuoc-Yu, 12 Oct 2026) and the one `compare` | done |
| `fast/compete.py` folds into the suite, the suite running comparators on any case | next |
| Checkpointed chunks for level 3: resume after a pause, keep the order, never rerun a finished case | next |
| The result row's answer and memory-kind fields | next |
| Level 4: `runtime = "apptainer"` on `draco`, Zebratlas as a workload | later |

## Hosts

[hosts.toml](hosts.toml) owns the host facts: each host's roles (`build`, `test`,
`measure`, `cluster`), what it has (Docker, Java, Apptainer), and the verified inputs under
the preservation root on the hosts that hold it. Only a `build` host compiles Rust and runs
the gate (`scripts/lib/remote.sh`, `git config nrese.buildHost`); a `measure`-only host runs
binaries a build host made, with their source revision and SHA-256 recorded. What changes
timings on a host (toolchain, CPU governor, profiling, page-cache control) is its `config`,
dated: a change starts a new configuration, and `compare` refuses to set runs of two
configurations side by side; earlier records stay valid. Inputs are
used read-only and shared across worktrees; generated data, stores, scratch and results
live elsewhere. `~/.config/nrese-bench.env` on a host names only what changes per campaign
(the binaries under test, the results directory), and is sourced explicitly.

## The directory

```text
benches/
├── README.md            this map and the design
├── PROTOCOL.md          the rules: fairness, correctness first, repetitions, statistics, records, publishing
├── CATALOG.md           the benchmarks per field: what each measures, its cost, core / representative / later
├── STATUS.md            generated: coverage matrix, tier tables, gaps, problems, runs
├── hosts.toml           the hosts, their roles, the verified inputs
├── runs/                one record per run (committed)
├── baselines/           committed NRESE reference numbers the regression gates compare against
├── cases/               every case, a file per group, one schema (cases/README.md)
├── fast/                the fast runner (levels 2 and 3): each case checked and route-checked, in a container or natively
├── suite/               the registries and the driver
│   ├── systems.toml     systems, capabilities, publishing rule, how they start
│   ├── workloads.toml   workloads, needs, standard tiers, source, licence, check, state
│   ├── suite.py         CLI: matrix, run, report, compare, ledger, status, images
│   ├── suitekit/        driver, adapters, runtimes, workload plans, schema, answers, hosts, manifest, ledger, status
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
    ├── nrese-bench-harness/  the Rust HTTP client: query-mix, write-scaling, compatibility packs
    ├── probes/          single measurements: HTTP latency, large results, memory, autocompletion
    └── cluster/         the same suite on a SLURM cluster (Apptainer)
```

| Layer | What | Where | Changes when |
|---|---|---|---|
| Rules | how anything is measured, checked, recorded and published | `PROTOCOL.md`, `competitors/README.md` (licences) | rarely, by decision |
| Registries | what exists: systems, workloads, cases, hosts | `suite/*.toml`, `cases/*.toml`, `hosts.toml` | a system, workload, case or host is added |
| Drivers | how a pair or case runs | `suite/suitekit/`, `fast/fast.py`, the kits | a new kind of measurement |
| Evidence | what came out: raw results (local), run records, STATUS, baselines (committed) | `suite/results/`, `runs/`, `STATUS.md`, `baselines/` | every run |

## Kits

| Kit | Measures | Driver | Raw results | Committed |
|---|---|---|---|---|
| [fast](fast/README.md) | many small cases: every reasoning profile, maintenance, DL, memory caps, query shapes and operators, services, kernels, layout, plans; checks and route checks before times; home-turf cases per competitor | `fast/fast.py run`, `compare`, `compete` | `~/nrese-bench/reports/fast/` (full reports) | `baselines/fast/*.json` (compact records) |
| [suite](suite/README.md) | every workload × system pair the cycle can run: load, size, restart, count, queries, answers, memory | `suite.py run`, `batch.sh` | `suite/results/<run>/` | record in `runs/` |
| [reasoning](reasoning/README.md) | inferred sets fact by fact against a reference closure (precision, recall); the LUBM and OWL2Bench data | `reasoning-scorecard.sh` | `reasoning/results/` | — |
| [reasoning/dl](reasoning/dl/README.md) | OWL 2 DL correctness and classification speed; the reference reasoners | `reference.py`, `w3c.py`, `ore.py`, `nrese.py` | `reasoning/dl/results/` | the TSVs (free systems only) and a record |
| [reasoning/el-classification](reasoning/el-classification/README.md) | NRESE's EL classifier against ELK on random ontologies | `run_elk.sh`, `el_check.py` | scratch | — |
| [competitors](competitors/README.md) | throughput with concurrent clients, writes under read load; the datasets of the basics mix | `scorecard.sh` | `competitors/results/` | the run records in `runs/` |
| [integration](integration/README.md) | the RG × GS × GND workload | its scripts; the suite's `integration-rg-gs-gnd` | `integration/results/` | — |
| [clients](clients/README.md) | client libraries end to end | `run-all.sh` | stdout | — |
| [oracle](oracle/README.md) | NRESE's SPARQL answers against Jena's | its script | `oracle/results/` | — |
| [oxigraph-comparison](oxigraph-comparison/README.md) | parsers, serialisers and evaluators against Oxigraph's | `cargo test`, `cargo run --bin bench` | `oxigraph-comparison/results/` | — |
| perf lab | the store and query engine without HTTP, per query p50; the fast suite's measuring tool, on the perf lab's datasets the `lab-*` cases | `fast.py run --cases lab-…` | — | `baselines/perf-lab/*.json` (until 27 Sep 2026; `compare` reads them) |
| [probes](probes/README.md) | one property at a time | one script each | stdout | — |
| [cluster](cluster/README.md) | the suite on Draco | `suite.sbatch` | the job's directory | record in `runs/` |

A case without an answer check times only that it ran: consult its checks before reading
its time as validated. Correctness also has owners outside these kits: crate conformance
tests (RDF, SPARQL, OWL, SHACL, GeoSPARQL), the Jena oracle, and the EL and DL kits'
canonical taxonomies. Equal row counts across systems don't prove equal bindings; the
answer recorder compares the bindings.

## Lab tools

Single measurements and diagnoses outside the kits, run with
`scripts/cargo-guarded.sh run --release -p <crate> --example <name> -- …` on a build host
(each file's header says how). `benches/probes/` has the HTTP-level probes.

| Area | Examples |
|---|---|
| Store, load, restart | `nrese-store`: `perf_lab` (the perf lab), `restart`, `reason_query`, `support_sets`, `vector_search`; `nrese-engine`: `insert_latency`, `image` (a store image in the current format); `nrese-rdf-io`: `parse_parallel` (why a parallel load rejects a file) |
| Rule reasoning | `nrese-reasoner`: `v2_closure` (closures for the oracle), `v2_delta` (commit latency against rematerialisation), `graph_sets`, `classify` (EL, against ELK) |
| DL | `nrese-dl`: `tableau_consistency` (per-phase times, search counters), `context_classify`, `bounds_eval` (the L and U1 bounds), `tableau_fuzz`; `nrese-owl`: `fuzz` (random OWL 2 DL ontologies), `ofn_speed` (the functional-syntax reader's speed gate) |
| Studies kept for open targets | `nrese-engine`'s ignored tests `compression_study` and `vocabulary_study` (store-size options, P2 in [performance.md](../docs/design/performance.md) §6) |
| Office PC | `scripts/office/`: dataset preparation (`dbpedia.sh`, `wikidata.sh`) and the perf lab there (`perflab.sh`) |

## The measurement queue

Timed runs don't start by hand: they are jobs in a measure host's queue
([suite/queue.py](suite/queue.py)), run one at a time, so that no two timings share a
machine and every system of a comparison runs on the same host under the same limits. A
host's `queue` in [hosts.toml](hosts.toml) says when a timed job may start: the office PC
at night or while its owner leaves `~/owlrs/FREE`, never while its owners are present
(`~/owlrs/RETREAT`; a job they stop runs again); Phuoc-Yu whenever no compiler runs;
Khaleesi (64 GiB, Windows) at night, for runs that need RAM and whole comparisons whose
every contender runs there, its results fetched the day they end (`queue.py fetch`);
GamerGrill (WSL) any time, but never a timing: downloads, data preparation, answer checks
(`window = "any"`). The main PC builds and prepares, never times.

```text
python benches/suite/queue.py submit JOB.toml        the job's checkout pushed to its host, the job queued
python benches/suite/queue.py status                 every host's pending, running, done and failed jobs
python benches/suite/queue.py cancel ID --host HOST  a pending job back out
python benches/suite/queue.py fetch ID --host HOST --to DIR   its results here (removed there on a same-day host)
python benches/suite/queue.py work                   on the host: its runner (one per host, detached)
```

A job names its group, host, checkout and commands (a build first where it needs one), the
environment (binaries, a matched scenario's limits), what the host must have, its window
(`quiet` for timings, `any` for counts), priority and expected minutes. Its results land in
`~/owlrs/results/<id>` on the host; its record is written from them (`suite.py ledger`).
Answers come before time: compete captures every answer, a timed `suite.py run` must ask for
them (`--answers full`), or the job is refused.

## A run, step by step

```text
plan       suite.py status                      what is missing; pick workloads, tiers, systems
           suite.py run --dry-run ...           every step printed, nothing started
run        suite/batch.sh NAME -- ARGS [-- ARGS]  from a worktree of HEAD; writes suite/results/NAME/
                                                  results.csv   one row per measured step (suitekit/schema.py)
                                                  manifest.json commit, machine, Docker limits, protocol, images, dataset sizes
                                                  answers.json  with --answers full: every answer and its verdict
                                                  logs/         per pair and repetition
check      suite.py report suite/results/NAME   tables (restricted systems left out)
           suite.py compare BASE NEW            regressions and changed answers against a baseline run
record     suite.py ledger suite/results/NAME   writes runs/NAME.toml; then fill in purpose, cases, findings
           suite.py status --write              regenerates STATUS.md
commit     runs/NAME.toml and STATUS.md         never the raw results
clean      batch.sh: containers, stores, scratch go; datasets stay until the results are concluded
```

On a host with preserved inputs, pass them with `--data` (the path from
[hosts.toml](hosts.toml)) and `--skip-build` with the binaries named in the host's
`nrese-bench.env`; `--data DIR` skips a missing input instead of preparing it. Neither
`batch.sh` nor `scripts/bench-cleanup.sh` may manage preserved inputs or another
campaign's resources: both clean by name and on low disk.

## Adding things

- **A system:** an entry in `suite/systems.toml` (capabilities from its documentation, `publish`, `runs`), and an adapter in `suite/suitekit/adapters.py`. Its first run confirms the capabilities; a capability that doesn't work is removed, with a note.
- **A workload:**
  1. An entry in `suite/workloads.toml`: `needs`, `tiers`, `source`, `licence`, `check`, `state = "planned"`.
  2. Its data and queries in a kit.
  3. A plan in `suite/suitekit/workloads.py` (then `state = "ready"`), or a kit driver that writes a run record.
- **A fast case:** [fast/README.md](fast/README.md#adding-a-case).
- **A host:** an entry in `hosts.toml`; its inputs verified against the preservation receipts before first use.
- **A capability:** add it to `CAPABILITIES` in `suite.py` and to the vocabulary at the top of `systems.toml`.

`suite.py --check` validates the registries. The hooks don't run benchmarks: they run in
bulk at the end of a batch of work (PROTOCOL.md §8).
