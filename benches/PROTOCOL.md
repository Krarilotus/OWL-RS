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
- **Rulesets before times.** The OWL 2 RL rules are complete only for ground assertions over RL ontologies without punning or annotation-property axioms (OWL 2 Profiles, Theorem PR1); elsewhere they are sound only, and stores add or omit axiomatic triples and some RDFS rules. When answer counts differ between rule reasoners, compare their rulesets first: a difference there is a semantics difference, not a wrong answer.
- **A wrong answer is a finding.** It is reported as `wrong`, never as a time. The report and the comparison list wrong answers first.
- **No majority votes.** When references disagree, a person settles the case with a minimised witness and records it (`reasoning/dl/disputed.tsv`).
- **Match the oracle to the task.** Exact bags preserve duplicates and RDF term
  identity; a total order additionally permits exact sequence comparison. Unordered
  `LIMIT`, tied ordering and other permitted nondeterminism need case-specific
  legal-output checks. Different permitted outputs alone are not a wrong answer.
  Keep strict mismatches and their adjudication visible. Do not silently coerce
  numeric terms, add an ordering to the measured query, or treat equal counts as
  complete validation. An approximate task declares its quality target before timing.
- **Timeouts aren't answers.** An exact service never turns a timeout into "not entailed" or an empty result.

## 3. Fairness

- **Same hardware opportunity, declared resource scenario.** Every system of a
  comparison runs on the same machine and runtime. Numbers from different machines
  are never compared. Choose the scenario before running, and do not pool scenarios:
  - **Native capacity:** each system gets suitable documented settings and the
    available host capacity. Do not impose an extra memory or CPU quota merely to
    make the harness fit. Record actual heap, internal memory, thread, affinity and
    ancestor-cgroup settings; an unlimited container can still have a limited parent.
    `DOCKER_MEMORY=0` is the existing suite override for no added container memory cap;
    it does not remove a JVM heap or another system's internal limit.
  - **Matched resources:** all systems get the same explicitly selected external
    CPU/memory envelope for that scenario. A JVM heap is part of its process memory,
    not an equivalent envelope for a native process. Give each system appropriate
    internal settings within that envelope and record them.
  - **Scaling and frontiers:** vary a declared resource, concurrency, data-size or
    quality dimension; keep the other conditions fixed. Existing capped fast cases
    remain useful here. A boundary outcome is coverage, not a fast successful answer.
  Isolation and client capacity are prerequisites in every scenario. If a host
  cannot meet a case's requirements, report the blocked pairing or use a suitable
  host for the entire comparison; do not silently shrink its input or add a cap.
- **Every system's strengths count.** Nothing is switched off to level the field.
  - Systems with a result cache run twice: cache off (repeated runs measure evaluation), then, after a restart, cache on (what a user sending the query again sees).
  - Both lines are reported.
- **Defaults first, tuning documented.** Each system runs with its documented defaults and the vendor's recommended settings for the workload. Any further tuning is in its adapter, with the reason, and applies to every run.
- **The measurement infrastructure must accommodate the task.** Complete answer
  recording and validation belong to the harness. Their CPU, memory, disk and time
  are reported separately from the system under test, with any shared-host
  interference disclosed. A recorder retaining every prior answer is a harness
  defect, not a reason to truncate results, reduce the workload or omit a system.
  Preserve raw answers and exact multiplicities; validate large answers without
  accumulating the whole campaign in RAM. Keep validation outside timed execution
  and label lifetime peaks that include an untimed validation replay.
- **Regimes are compared only within themselves.** A reasoning workload names the regimes it accepts, in order of preference, and each system runs the first one it has.
  - **NRESE runs every case,** in every profile it has (RDFS variants, OWL-Horst, OWL 2 QL, RL, EL, DL). Each case keeps its own NRESE baseline. Competitors run only the cases whose semantics they support.
  - **Stores without reasoning** (QLever, Oxigraph, Virtuoso, …) run the plain-SPARQL cases. They also run a reasoning workload's queries over NRESE's closure loaded as data. That compares the query side fairly and puts a number on what reasoning in the store buys.
  - **Stores without reasoning, by rewriting:** a query that needs inference is also run on them as NRESE's own OWL 2 QL rewriter prints it, as property paths (`rdf:type/rdfs:subClassOf*`, the headline form) and as enumerated `VALUES` (secondary), never rewritten by hand. NRESE runs the path form too, with reasoning off. Expressible this way: OWL 2 QL, RDFS and property axioms between named individuals (sub-properties, inverse, symmetric, transitive, regular chains); RL and EL per query only. Queries needing recursive RL/EL rules, `sameAs` or DL are reported per system as not expressible.
  - **A profile no competitor supports** is reported in a column of its own, not as a comparison.
  - **Home turf:** each competitor also runs on what it documents as its strength (the v2 merge checklist, §3).
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
- **Warm-up and first execution.** Each query's first execution on a fresh server
  is recorded apart (`repeat` 0), followed by `--query-runs` measured repeats. The
  suite's preceding count query can already warm indexes, the JVM and filesystem
  cache. This is first-query latency, not evidence of an OS-cold run; a cold-cache
  claim needs the separately recorded cache-control procedure below.
- **Order.** Queries run in rounds over the whole mix, each round shuffled. Repetition r uses seed `seed·1000 + r`, so every system gets the same orders, and the order is recorded in each row. The DL kit shuffles the axiom order for its robustness runs.
- **Limits.**
  - Per load: `--timeout-s`, default 3600.
  - Per query: `--query-timeout-s`, default 300.
  - Per DL task: 300 s on the development tiers, 1,800 s on the full tiers (ORE's limit).
  - A limit is reached as `timeout`, never as a time.
- **Memory.** Report process peak RSS, container/cgroup charged lifetime peak and
  phase observations as different measurements. Cgroup accounting includes more
  than process RSS. Record which processes and phases each number includes; a
  cumulative high-water mark cannot be subtracted to obtain a phase peak. Missing
  lifetime accounting stays unknown. Sampled observations are labelled as samples,
  regardless of run length, and cannot establish that brief peaks were absent.
- **Isolation.**
  - No other benchmark runs on the machine at the same time.
  - Containers that must stay up (the DMW containers on the main PC) are idle.
  - The manifest records the machine's Docker CPU and memory limits.

**Claim runs on Linux** (the office PC, Draco) add what a desktop can't promise:
- the memory limit of each system's cgroup (`memory.max`) and its peak (`memory.peak`) instead of sampled RSS;
- the system's cores pinned (`--cpuset-cpus`), the client on others;
- the CPU governor on `performance`, turbo as configured and recorded;
- the page cache dropped before each cold run (`echo 3 > /proc/sys/vm/drop_caches`, sudo);
- all of it recorded in the manifest.

## 5. Statistics

- **Per query:** the median of the measured repeats within a repetition, then the median over the repetitions.
- **Spread:** the range of the per-repetition medians relative to their median. With one repetition, the interquartile range of the repeats. The spread is reported next to every comparison.
- **Per pair:** the sum of the query medians (first and repeated executions apart), the load time, store size, restart time and peak memory, each as a median over the repetitions.
- **Comparing two runs or two systems:**
  - the ratio per query;
  - the geometric mean of the ratios, beside the ratio of the sums;
  - a change counts only beyond `max(10 %, the measured spread of both sides)` and beyond 2 ms (`suite.py compare`).
  - For declared microcase comparisons, record an explicitly lower `--min-ms` floor
    (zero is supported). Keep the repetition and noise checks. Exploratory reanalysis
    with a new floor is labelled as such; choose the acceptance thresholds before the
    next measurement. A below-floor change is not evidence of equivalent performance.
  - Compare throughput directly, with lower QPS as worse, at each client width.
    A latency floor in milliseconds cannot qualify throughput. Missing cases, widths,
    repetitions or answers cannot disappear into an intersection of successful work;
    expected boundary outcomes receive no timing credit.
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
  - Verified shared input snapshots, seeds, hashes, unique fixtures, reference
    answers and retained results stay. Reuse these across campaigns; neither a
    completed run nor low disk space authorizes deleting preservation copies.
  - Remove only identified owned disposable stores, containers, scratch and
    obsolete builds after final use. Keep caches/images needed by planned runs.
    Legacy `batch.sh`/`bench-cleanup.sh` broad or low-disk cleanup must not manage
    preserved inputs or another campaign's resources.
  - Installed tools and the `nrese-bench/*` images stay.

## 9. Threats to validity, and what answers them

| Threat | Answer |
|---|---|
| Docker Desktop's VM on Windows adds overhead to I/O and memory | every system runs under the same VM; the manifest records its limits; claim runs repeat on Linux (office PC, Draco) |
| The OS page cache makes a second load faster | interleave fresh-store repetitions; identify OS-cold numbers only when the recorded cache-control procedure was used |
| A load timed around `docker run` includes the container's start (a few hundred ms on Docker Desktop), which dominates the smallest tiers | the manifest records a no-op container's start (`container_start_ms`); NRESE's own load phases are in its load log; small tiers are read as start-up plus load, and claims about load speed use tiers whose loads take seconds |
| Sampled `docker stats` can miss short peaks differently across systems | retain kernel lifetime accounting through container exit, record its scope and qualify its lifecycle; otherwise label memory as sampled/unavailable and withhold lifetime-peak claims |
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
