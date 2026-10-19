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
  Each system is compared with one reference, never by majority, and the strict and
  the value comparison are recorded apart: an integer typed `xsd:int` where the
  reference has `xsd:integer` is a value match with its datatype difference kept
  ([suite/suitekit/answers.py](suite/suitekit/answers.py)). Don't add an ordering to
  the measured query, or treat equal counts as complete validation. An approximate
  task declares its quality target before timing.
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
- the memory limit of each system's cgroup (`memory.max`) and its peak (`memory.peak`) instead of sampled RSS, beside the process's peak RSS: the cgroup's counts the page cache its reads were charged, which depends on what the host had cached (the fast suite's LUBM 100: 2,375 to 4,700 MiB for one build);
- the system's cores pinned (`--cpuset-cpus`), the client on others;
- the CPU governor on `performance`, turbo as configured and recorded;
- the page cache dropped before each cold run (`echo 3 > /proc/sys/vm/drop_caches`, sudo);
- all of it recorded in the manifest.

## 5. Statistics

- **Per query:** the median of the measured repeats within a repetition, then the median over the repetitions.
- **Spread:** the range of the per-repetition medians relative to their median. With one repetition, the interquartile range of the repeats. The spread is reported next to every comparison.
- **Per pair:** the sum of the query medians (first and repeated executions apart), the load time, store size, restart time and peak memory, each as a median over the repetitions.
- **Comparing two runs or two systems:** the ratio per query, and the geometric mean of
  the ratios beside the ratio of the sums. A change counts only beyond a relative
  threshold, the measured spread of both sides and an absolute floor; throughput is
  judged per client width; lost cases and answers come first and never vanish into the
  work both sides finished. Thresholds are chosen before measuring, and their values and
  exceptions have one owner: [fast/README.md](fast/README.md#comparing).
- **Corpora** (ORE, the W3C suites, generated ontologies):
  - solved counts;
  - median, p90, p95 and p99;
  - a PAR-2 score (a timeout counts twice the limit);
  - cactus plots.
  - Never a mean over the solved tasks alone.
- **Claims are made on a blind holdout.** For a tuned corpus, a fixed and seeded 20% is never looked at while optimising.
- **Ablations.** An optimisation is shown as an on/off chain on one build, with the same hardware, input order and limits.

### The release gate

One question: does the head lose, in time or memory, on any workload, against **both** the
v1.5 baseline (0f465633) and `main` (f9b515e8)? No loss is tolerated: a clear loss blocks
whatever its size, unless it is an owner-approved trade-off (the owner, 18 Oct 2026: "I would
only approve tradeoffs"). With it: every guard of
[docs/design/performance.md §0](../docs/design/performance.md) green, and every W3C suite at
its required counts. `fast/release_gate.py` decides; `scripts/release-gate.sh` runs it as fleet
steps; `release-gate.sh jobs HEAD` writes the jobs. Each part's verdict is `passed` only when
nothing blocks; `verdict` passes only when every part does.

**The passes.** One harness for every side: the head's fast suite (its cases, checks and
generator); only the engine's binaries differ, each built by its own revision and toolchain
and verified by SHA-256 in every pass. Every case runs at its declared cap, never lowered: on
phuoc-yu (24 GB a case) those that fit, on khaleesi (64 GB) the three that need 28 GB
(sparqloscope-dblp, bsbm-explore-100m, bsbm-bi-100m), all three versions there alike. Khaleesi
builds nothing: the build job packs the verified binaries (`release-gate.sh pack`, a tar that
keeps their modes) and khaleesi's job unpacks and verifies them again. A case left to khaleesi
counts only when khaleesi's timed part has it in its plan (`verdict` checks); a case no host
can run is *not run* and blocks. Nothing waives it. Khaleesi runs the job's Linux steps under
WSL 2 (a fleet host of link `wsl`, its VM given 40 GB: the case's 28 GB in a 35 GB slice;
swap off, systemd on, the cases' datasets on its own disk); until it does, `rg-*-big` waits
and the verdict can't pass. The data are prepared once, before any pass (`fast.py prepare`): taken from the host's verified fast inputs where they hold the
file (the generator's output preserved on 9 Oct 2026), else generated by the harness's own
perf lab; never by a side's: a pass on main's binaries would otherwise generate with main's perf
lab (it has no generator), and on v1.5's with an older one.
Every input is recorded with its SHA-256 and whether it is its freeze (`datasets.toml`); each
pass checks the prepared files are unchanged before and after it. WatDiv's generated query sets
live with the data and are checked file by file against `sparql/queries.sha256`. The frozen
`bsbm-100m.nt` is not N-Triples (5,696,000 objects in Turtle's double shorthand, `2685.34e0`):
the gate **converts** it with `sparql/nt-numbers.py` (typed literals of the same lexical form,
every other line unchanged; the result's SHA-256 is pinned beside the freeze); a host's copy
that isn't the freeze (phuoc-yu's is a Jena riot conversion of a regeneration) is reported as
such. Each timed host runs one fleet job of class *exclusive* (nothing else on the host),
passes of one repetition in blocks of base, main, head, head, main, base: the first two blocks
run every case (four passes a side), each later block only the cases whose noise needs more
passes ([fast/measure.toml](fast/measure.toml), "Noise: reduced, not tolerated" below).

**The schedule is checked, not assumed.** Every required baseline is there with as many
passes as the head (at least four; fewer is diagnostic, never a release verdict); the passes ran
one at a time (each ended before the next began, by their recorded nanosecond times); their
order is a palindrome, so a drift linear in time falls on every side alike; no side runs more
than two passes in a row. A case of later blocks keeps the same rules over the passes that ran
it (each side as many, at least four, a palindrome), and is judged on those. Each pass is
complete, of its block's plan, on the clean harness revision at its start and end, on its
side's verified binaries, one host and configuration, measured by the same calibration; the
CPU frequency policy the passes met (each pass records the host's state) is the same in all.

**main's perf lab** (f9b515e8, a single file) has no `--routes`, requires `--queries`, and
reports per query only `p50_ms`, `min_ms`, `max_ms`, no samples. Its side runs with `fast.py
--no-routes --empty-queries` (read from the build's `CAPABILITIES`): checks of values only
`--routes` reports are *unchecked* (null), never wrong; rows are still checked. A comparison
with such a side uses one value per process on both sides, each process's p50 (the same
`times[len / 2]` in every revision's perf lab), never main's p50 against the head's raw
samples. Each query's rows are compared between the sides; rows that differ block unless a
check pins the head's answer (then the baseline's time is not compared). A case a baseline
can't run (an argument or tool it lacks, a wrong answer of its own) is *not compared* against
it, with that reason, and doesn't block: it isn't evidence about the head. The head failing a
case blocks; so does the head stopping at its declared limit where a baseline completed.

**The noise policy** (`suitekit/compare.py` `judge`, one dimension at a time: each query's
time, the process's peak, each phase's peak, each client width's QPS and peak, each cache
mode's peak; none offsets another). The estimate is the median of all values, its interval
the two-level bootstrap (passes drawn, then values within them, 2,000 rounds, seeded), so each
case's own run-to-run noise sets its width. No relative tolerance:

- *Regression* (blocks): the whole 95 % interval on the worse side, and the difference at
  least the measurement's resolution: **5 µs** (the perf lab reports times in 1 µs steps; the
  same binaries' series under 10 µs moved between passes by at most 5 µs, p95 2.5 µs) or
  **2 MiB** (peaks are whole MiB on each side). Below the resolution a difference is no verdict.
- *Better*: the whole interval on the better side.
- *Neutral*: the interval reaches 1 and its worse end lies within **2 ×** the case's
  between-pass noise (the standard deviation of the log of its per-pass medians, the larger
  side's).
- *Suspect: needs N passes* (blocks): neither. The passes didn't pin the ratio down; N
  estimates the passes a side for the interval to settle (its half-width shrinks as
  1/√passes); "unbounded" above 64 (the noise has a cause: a bimodal plan, an unstable timer).
  The report says the largest N.
- *Incomplete* (blocks): fewer than two passes on a side, or a pass without valid values.

The precision of "neutral" (2 × the noise) is how exact the evidence must be, not a loss
accepted: a clear loss of any size blocks.

**Confirm on rerun** (the owner's decision, 18 Oct 2026). With a 95 % interval about 2.5 % of
identical series fall wholly on one side by chance, at any number of passes, so one run's
"loss" isn't proof. Every dimension a run flags (a loss, or undecided) runs again: `release-gate.sh
confirm R/... --out jobs/` writes `rg-<h8>-confirm`, a fresh, independent A-B-B-A of just those
cases on the same host (fresh processes, the same palindromic order, the passes a side each
case's noise needs: the flagged rounds' estimates, 4 to 24). The verdict combines each host's
rounds (`combine`):

- *confirmed*: two rounds show the loss at 95 % on the worse side: blocks (unless a trade-off
  covers it);
- *not confirmed*: shown once, then a round without it: reported with every round's interval,
  doesn't block; a loss only a confirmation shows is reported so too;
- *resolved*: undecided, then decided without a loss;
- *pending*: another round is due (undecided again, or a first detection in a confirmation):
  `confirm` on all the rounds' results writes `rg-<h8>-confirm2`;
- *undecided* (after three rounds) and *missing* (a confirmation without the dimension): block.

Other blocks (a case failing, lost series or answers, a pass that doesn't qualify) aren't
retried away: they block from whichever round has them. What this gives, simulated from the
14 Oct passes' measured noise (621 series; median sd 1.4 %, p75 4.3 %), four passes a side in
the first round:

| Identical binaries, per 400 series | |
|---|---|
| confirmed (false) losses | 0.48 (0.12 % a series; 400 × 0.025² would be 0.25: the first round's four passes and the undecided paths add to it) |
| still undecided after three rounds (block) | 0.97 |
| per gate run (two baselines, about 800 series) | about 1 false confirmed loss and 2 undecided blocks |

| Repeatable loss | noise sd 0.5 % | 1.4 % (median) | 4.3 % (p75) |
|---|---|---|---|
| 1 % | 81 % confirmed, 6 % undecided | 7 %, 4 % | 1 %, 0.5 % |
| 3 % | 100 % | 87 %, 3 % | 4 %, 4 % |
| 8 % | 100 % | 100 % | 72 %, 8 % |

A noisy series barely decides a small loss: those are a work list of their own
([NOISY.md](NOISY.md), 137 series above 5 % noise, grouped by likely cause), to be fixed, not
tolerated. The known blockers, against main on the 14 Oct data, all stay regressions in one
round: olympics q06-text-filter 3.74× [3.13, 4.02], yago q10-labels-en 3.50×, entities
q06-label-prefix 4.09×, dbpedia q02-born-in-berlin 3.32×.

**Noise: reduced, not tolerated** ([NOISY.md](NOISY.md); 18-19 Oct 2026). With no tolerance
the gate's power is its noise: at 14 Oct's, a repeatable 2 % loss was caught (confirmed) at
least 90 % of the time in a third of the series. The harness takes the noise down where it
arises, the same for every side:

- *More executions where they are cheap.* `release_gate.py calibrate` writes
  [fast/measure.toml](fast/measure.toml) from the gate host's own passes (an A/A run: one binary
  as every side): a perf-lab case timed by its queries runs them until about 2 s of query time a
  process (`--runs`, at most 2,000, warm-ups 3), a DL tableau case up to 3 s of files a process
  (`repeat`, at most 30 and 40 % of its timeout); `fast.py run --measure` applies it to every
  side, whatever the case's own arguments say, and records it per case. A case whose runs are
  what it measures (a first execution, a reopened store, a replayed log, a sweep, commits) is
  left as it is. Repeated processes pool their samples.
- *Fresh processes where the noise is between them.* The same binary doing the same work
  (identical work counters in every process) takes a different time in each process: its
  memory layout (address randomisation, hash seeds) differs, and in-process repeats can't
  average that. Where a case misses the target even at eight passes, a pass runs up to five
  fresh processes (15 s together at most), the perf lab's `repeat`, a DL tool's `processes`
  (its `repeat` repeats inside one process). Layouts are averaged over, never pinned: one
  fixed layout would make its luck a repeatable difference between binaries.
- *Batched timing for the microsecond cases* (`perf_lab --batch K`: a sample times K executions,
  reported per execution to the nanosecond): a 9 µs query is nine steps of the report's
  microsecond, alone. It needs every side's perf lab to have it (the build's `CAPABILITIES`,
  `GATE_BATCH`). A series batched on both sides in every pass is judged at its sample's
  resolution, 5 µs / K (`judge`'s `resolution`): the 5 µs floor is the report's microsecond
  steps, which a batch divides. v1.5's and main's perf labs can't batch, so against them a loss
  below 5 µs (a 2 % loss under 250 µs) stays undecidable; the gate whose baselines batch (v2's
  successors against v2) decides them. Likewise a series reported in coarser steps (an older
  perf lab's open time in whole milliseconds, the EL classifier's phases to a tenth) is judged
  at its step: a difference of one step is no verdict (`release_gate.py quantum`).
- *Nothing timed that the gate doesn't judge.* A gate's pass doesn't rerun a case past its
  limit (the frontier rerun: 226 s of a 1,018 s pass), and a case with a declared outcome (a
  known limit, whose status the gate checks but never times) runs in the first block only, two
  passes a side.
- *Passes where the noise needs them.* Each case's noise (the largest sd of log pass medians
  among its series whose 2 % is above the resolution) sets the passes a side for 90 % power
  against a 2 % loss, the confirm-on-rerun chain simulated as for the table above
  (`benches/probes/gate-power.py`); every case gets four, and the passes beyond go where they buy
  the most power a second, until the timed part's budget (150 min on phuoc-yu). Quiet cases stay
  at four; later blocks carry the noisy ones.
- *Host hygiene, checked and recorded per pass* (`scripts/hosts/hygiene.py`, its state at the
  pass's start and end in the pass report, problems in the gate's report): the frequency policy
  (driver, governor, energy preference, boost), THP, the CPU and L3/NUMA topology, others' busy
  processes, swap, temperature. Timings are pinned to one L3 and NUMA node where the host's
  configuration names `cpus` (office-pc: one CCD, `8-15,24-31`); each case's inputs are read
  into the page cache before it is timed (cold runs empty it themselves); each pass runs under the
  quiet watcher (the fleet's native jobs had none: this user's other busy processes stopped,
  never a desktop's compositor; others' work recorded); the gate's directories carry
  `.trackerignore` (phuoc-yu's desktop indexer reads all of `$HOME`). Settings that need root
  (the governor, boost, THP) are recorded, never changed: phuoc-yu's is performance with boost,
  office-pc's powersave with energy preference balance_performance (a shared host).

**Trade-offs.** [release-tradeoffs.toml](release-tradeoffs.toml) is the only thing that accepts a
loss, and only the owner adds to it: a case, a dimension, optionally the baseline, the largest
ratio the loss's interval may reach, the date, approver and reason, and the win that pays for
it, which must show as *better* in the same report. A covered loss is reported as an
*accepted trade-off*; anything beyond its bound, without its win, or on another dimension
blocks. The list starts empty.

**Memory, one policy.** Judged: the process's own lifetime peak (VmHWM, which every side's perf
lab reports) and, where both sides report them, each phase's peak. Reported beside them, never
judged: the scope's cgroup `memory.peak`, which also counts the page cache the case's reads were
charged and so depends on what the host had cached (rl-lubm100: 2.3 GB of 4.7). Work counters
are reported (more than 2 % more work is marked); the gate's question is time and memory.

**W3C and guards** (office-pc, `release-gate.sh w3c`, `guards`). The suites run with their
data required (`NRESE_W3C_REQUIRED`: a suite without its data fails, never skips; the OWL 2
test cases pinned by SHA-256) and `--nocapture`; each suite's counts are checked against
[fast/w3c-expected.toml](fast/w3c-expected.toml): no failure, at least its passes, at most its
known skips and disputes. The guards are the test names §0's Guard column gives; each must run
and pass (a name no test has, or an ignored one, fails: docs and code disagree).

```sh
bash scripts/release-gate.sh jobs HEAD --out jobs/   # build, timed (phuoc-yu), big (khaleesi), checks (office-pc)
fleet submit jobs/*.toml                             # with FLEET_OWNER set; then fleet results ... --to R
bash scripts/release-gate.sh confirm R/rg-H8-timed R/rg-H8-big --out jobs/   # rg-H8-confirm, rg-H8-big-confirm
fleet submit jobs/rg-H8-*confirm*.toml               # then confirm again on every round's results, if pending
bash scripts/release-gate.sh verdict R/rg-H8-timed R/rg-H8-confirm R/rg-H8-big R/rg-H8-big-confirm R/rg-H8-checks
```

## 6. Records: every number traceable

- **Raw results.** One CSV row per measured step (`suite/suitekit/schema.py`) and the logs. They stay in the run's directory, which git ignores.
- **Manifest.** `manifest.json` in the same directory, one entry per invocation:
  - the commit, and whether the tree was dirty;
  - the machine (CPU, threads, RAM, OS) and Docker's own limits;
  - the protocol settings and the relevant environment;
  - every image's content id and every dataset's size.
- **Run record.** `runs/<run>.toml`, committed ([runs/README.md](runs/README.md)): the
  builds compared and their identities, the host and scenario, the protocol, the inputs
  and results by SHA-256, and a verdict per case; the raw samples stay with the results.
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
  - Verified shared inputs ([hosts.toml](hosts.toml)), seeds, hashes, unique fixtures,
    reference answers and retained results stay; neither a finished run nor low disk
    space authorizes deleting them.
  - Remove only identified owned disposable stores, containers, scratch and obsolete
    builds after final use. Keep caches and images planned runs need.
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
