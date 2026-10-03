# The benchmark protocol

The rules every benchmark in this directory follows. A number that wasn't measured this
way isn't used in a claim. The kits implement the rules: the suite's driver
(`suite/suitekit/`), the DL kit (`reasoning/dl/`), the perf lab. Where a kit departs from
a rule, its README says so and why.

Sources: the ORE 2015 competition report (Parsia et al., JAR 2017) on loading, timeouts and
correctness disputes; the reasoning benchmark design
([docs/design/reasoning-benchmark.md](../docs/design/reasoning-benchmark.md));
the DL performance plan ([docs/design/owl2-dl-performance.md](../docs/design/owl2-dl-performance.md)
§5); the owner's decisions on fairness (30 September and 2 October 2026).

## 1. Terms

| Term | Meaning |
|---|---|
| system | a store or reasoner as it is started (`suite/systems.toml`); a build variant (`variant-of`) is a system of its own |
| workload | data, a task (load, reason, query, update, validate, classify) and a check (`suite/workloads.toml`) |
| tier | a workload's size or variant (LUBM 100, `dbpedia-core`, ORE `dev`); the *standard tiers* are the ones a full comparison runs |
| regime | the entailment a system computes for a reasoning workload: none, rdfs, owl-horst, owl2-rl, owl2-ql, direct (DL) |
| pair | a workload tier on a system |
| run | one invocation of a driver over some pairs; it has a results directory, a manifest and a record |
| repetition | a pair measured again from a fresh store (`--runs`) |
| measured repeat | a query executed again within one repetition after the warm-up (`--query-runs`) |
| outcome | how a pair came out: ok, partial, wrong, failed, timeout, skipped, restricted |

## 2. Correctness before speed

- **Checked before timed.** A time counts only when its answer has been checked:
  - against the expected answers (LUBM's published counts, the W3C suites' results);
  - against a reference (owlrl's or Nemo's closure, the DL reference reasoners);
  - or against the other systems within the same regime (the driver's cross-check).
- **A wrong answer is a finding.** It is reported as `wrong`, never as a time. The report and the comparison list wrong answers first.
- **No majority votes.** When references disagree, a person settles the case with a minimised witness and records it (`reasoning/dl/disputed.tsv`).
- **Timeouts aren't answers.** An exact service never turns a timeout into "not entailed" or an empty result.

## 3. Fairness

- **Same machine, same runtime.** Every system of a comparison runs on one machine, in the same container runtime, with the same memory: `JAVA_HEAP` for the JVMs, `DOCKER_MEMORY` as a cap where the machine is shared. Numbers from different machines are never compared.
- **Every system's strengths count.** Nothing is switched off to level the field.
  - Systems with a result cache run twice: cache off (repeated runs measure evaluation), then, after a restart, cache on (what a user sending the query again sees).
  - Both lines are reported.
- **Defaults first, tuning documented.** Each system runs with its documented defaults and the vendor's recommended settings for the workload. Any further tuning is in its adapter, with the reason, and applies to every run.
- **Regimes are compared only within themselves.** A reasoning workload names the regimes it accepts, in order of preference, and each system runs the first one it has.
- **Licensed systems** follow [competitors/README.md](competitors/README.md):
  - their rows carry `publish = permission`, and their numbers stay on the machine;
  - their records say only that they ran (`restricted`).

## 4. Measurement

- **A fresh store per repetition.** Load, store size, restart until the server answers, then a count of every statement (lazy reasoners do their work there), then the queries, then the server's peak memory. After each repetition, its containers, store and scratch are removed.
- **Interleaved repetitions.** Per workload tier, every system's first repetition runs, then every system's second, each round in an order rotated by one. Drift on the machine (background I/O, heat, scanning of new files) then falls on every system alike. A regression check of one system runs the old build as a system of its own, interleaved in the same way (`benches/probes/ab-load.py` for loads).
- **Repetitions:**
  - 3 per pair for a check (`--runs 3`);
  - 10 for a run behind a published claim;
  - 1 only for a smoke test, which is never compared.
- **Warm-up and cold.** Each query's first execution on a fresh server is recorded apart (`repeat` 0): it is the cold case. Then come `--query-runs` measured repeats.
- **Order.** Queries run in rounds over the whole mix, each round shuffled. Repetition r uses seed `seed·1000 + r`, so every system gets the same orders, and the order is recorded in each row. The DL kit shuffles the axiom order for its robustness runs.
- **Limits.**
  - Per load: `--timeout-s`, default 3600.
  - Per query: `--query-timeout-s`, default 300.
  - Per DL task: 300 s on the development tiers, 1,800 s on the full tiers (ORE's limit).
  - A limit is reached as `timeout`, never as a time.
- **Memory.** The peak resident memory of the load and of the serving process; for the DL kit, per task.
- **Isolation.**
  - No other benchmark runs on the machine at the same time.
  - Containers that must stay up (the DMW containers on the main PC) are idle.
  - The manifest records the machine's Docker CPU and memory limits.

## 5. Statistics

- **Per query:** the median of the measured repeats within a repetition, then the median over the repetitions.
- **Spread:** the range of the per-repetition medians relative to their median. With one repetition, the interquartile range of the repeats. The spread is reported next to every comparison.
- **Per pair:** the sum of the query medians (cold and repeated apart), the load time, store size, restart time and peak memory, each as a median over the repetitions.
- **Comparing two runs or two systems:**
  - the ratio per query;
  - the geometric mean of the ratios, beside the ratio of the sums;
  - a change counts only beyond `max(10 %, the measured spread of both sides)` and beyond 2 ms (`suite.py compare`).
- **Corpora** (ORE, the W3C suites, generated ontologies):
  - solved counts;
  - median, p90, p95 and p99;
  - a PAR-2 score (a timeout counts twice the limit);
  - cactus plots.
  - Never a mean over the solved tasks alone.
- **Claims are made on a blind holdout.** For a tuned corpus, a fixed and seeded 20% is never looked at while optimising.
- **Ablations.** An optimisation is shown as an on/off chain on one build, with the same hardware, input order and limits.

## 6. Records: every number traceable

- **Raw results.** One CSV row per measured step (`suite/suitekit/schema.py`) and the logs. They stay in the run's directory, which git ignores.
- **Manifest.** `manifest.json` in the same directory, one entry per invocation:
  - the commit, and whether the tree was dirty;
  - the machine (CPU, threads, RAM, OS) and Docker's own limits;
  - the protocol settings and the relevant environment;
  - every image's content id and every dataset's size.
- **Run record.** `runs/<run>.toml`, committed:
  - the scope, commit, machine and protocol;
  - the outcome of each pair, but no times;
  - by hand: the purpose, the baseline it is compared with, and the findings.
- **Status.** `STATUS.md` is generated from the registries and the records.
- **Baselines.** `baselines/` holds committed NRESE numbers that later work is compared against. A change that makes a tier's sum more than 5% slower, beyond the noise, explains why or doesn't merge.

## 7. Publishing

- **Free systems.** Results of systems with `publish = free` may appear in committed documents.
- **Permission systems.** Results of systems with `publish = permission` (GraphDB, RDFox, Stardog, AnzoGraph) appear only with the vendor's written consent, recorded in [competitors/README.md](competitors/README.md).
- **Datasets.** Their licences are in `workloads.toml`. A dataset without a stated licence (the integration workload) needs its authors' agreement before its results are published.
- **Papers.** Every number in a paper names its run record.

## 8. When benchmarks run

- **In bulk.** Implementation and tests come first; benchmarks run in bulk at the end of a batch of work (`suite/batch.sh`), not after each change.
- **Before a milestone.** NRESE runs on its standard tiers and is compared against the last baseline run. The other systems rerun only where their pairs are missing or their versions changed.
- **Cleanup**, on every machine:
  - Containers, stores and scratch go after every run.
  - Datasets stay until the run's results are concluded, meaning its record has its findings and any reruns are done. Then `scripts/bench-cleanup.sh` removes them.
  - When the drive has less than 100 GB free, the datasets go at once (`batch.sh` checks).
  - Installed tools and the `nrese-bench/*` images stay.

## 9. Threats to validity, and what answers them

| Threat | Answer |
|---|---|
| Docker Desktop's VM on Windows adds overhead to I/O and memory | every system runs under the same VM; the manifest records its limits; claim runs repeat on Linux (office PC, Draco) |
| The OS page cache makes a second load faster | cold numbers come from the first execution on a fresh store; dataset files are read once per repetition by every system alike |
| A load timed around `docker run` includes the container's start (a few hundred ms on Docker Desktop), which dominates the smallest tiers | the manifest records a no-op container's start (`container_start_ms`); NRESE's own load phases are in its load log; small tiers are read as start-up plus load, and claims about load speed use tiers whose loads take seconds |
| Peak memory from `docker stats` comes about once a second and can miss a short peak | it misses peaks for every system alike; memory claims use steps that last several seconds; the process runtime (Apptainer, host processes) reads the kernel's peak resident memory instead |
| Freshly written datasets are scanned in the background while the first runs measure (batch 1 of 3 October: YAGO tiny's load 36% slower in the suite, 3% in an interleaved A/B) | datasets are frozen and kept (`benches/datasets.toml`), not prepared before each run; repetitions are interleaved |
| Thermal or background noise | shuffled order, repetitions, the spread reported, a noise threshold on comparisons |
| One dataset favours one design | several workloads per task; the basics mix uses five real datasets; DL corpora are stratified |
| Tuning on the test set | the blind holdout (§5) |
| An adapter misconfigures a competitor | defaults and vendor recommendations only (§3); the first run of an adapter confirms its capabilities; answer counts are cross-checked |

## 10. Checklist for a run

- [ ] `suite.py status`: the pairs to run are chosen, and the baseline run is named.
- [ ] The machine is quiet: no build, no other benchmark, nothing touching the shared containers.
- [ ] `suite.py run --dry-run`: the steps are as intended.
- [ ] Run it through `suite/batch.sh`, from a worktree of one commit.
- [ ] Check with `suite.py report`, then `suite.py compare BASE NEW`: wrong answers first, then regressions.
- [ ] Record with `suite.py ledger`, fill in purpose, baseline and findings, then `suite.py status --write`.
- [ ] Commit `runs/` and `STATUS.md`, never the raw results.
- [ ] Clean up: containers, stores and scratch at once; datasets once the results are concluded (or below 100 GB free).
