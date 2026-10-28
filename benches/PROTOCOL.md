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
phuoc-yu (24 GB a case) those that fit, on khaleesi-wsl (khaleesi's 64 GB under WSL 2) the
three that need 28 GB (sparqloscope-dblp, bsbm-explore-100m, bsbm-bi-100m), all three versions
there alike. The big host builds nothing: the build job packs the verified binaries
(`release-gate.sh pack`, a tar that keeps their modes) and the big job unpacks and verifies them
again. They are Linux ELF: `jobs` refuses a timed host that isn't Linux in
[hosts.toml](hosts.toml) (khaleesi, native Windows, never), and `unpack` fails on any other
system and on a file that isn't ELF. A case left to the big host counts only when its timed part
has it in its plan (`verdict` checks); a case no host can run is *not run* and blocks. Nothing
waives it. khaleesi-wsl is a fleet host of link `wsl` (its VM given 40 GB: the case's 28 GB in a
35 GB slice; swap off, systemd on, the cases' datasets on its own disk); until it runs,
`rg-*-big` waits and the verdict can't pass. The data are prepared once, before any pass (`fast.py prepare`): taken from the host's verified fast inputs where they hold the
file (the generator's output preserved on 9 Oct 2026), else generated by the harness's own
perf lab; never by a side's: a pass on main's binaries would otherwise generate with main's perf
lab (it has no generator), and on v1.5's with an older one.
Every input is recorded with its SHA-256 and whether it is its freeze (`datasets.toml`); each
pass checks the prepared files are unchanged before and after it. A host keeps one read-only
copy of the prepared data for all its gates (`~/owlrs/fast-data-shared`: files by SHA-256,
hardlinked into each gate's `fast-data`, and an index by generator that lets the next gate link
them in instead of generating them); `clean` removes copies no gate has linked for two days. WatDiv's generated query sets
live with the data and are checked file by file against `sparql/queries.sha256`. The frozen
`bsbm-100m.nt` is not N-Triples (5,696,000 objects in Turtle's double shorthand, `2685.34e0`):
the gate **converts** it with `sparql/nt-numbers.py` (typed literals of the same lexical form,
every other line unchanged; the result's SHA-256 is pinned beside the freeze); a host's copy
that isn't the freeze (phuoc-yu's is a Jena riot conversion of a regeneration) is reported as
such. Each timed host runs one fleet job of class *exclusive* (nothing else on the host) of
*blocks* (`release-gate.sh block`, `fast.py block`): in a block each case runs base, main, head,
head, main, base back to back, the next case after it. Each side's two runs of a case are then
centred on the same instant whatever the runs' durations, so a drift linear in time falls on
every side alike; a palindrome of whole passes isn't (a case's runs sit where the other cases'
durations put them: research round 8). The mirror cancels a linear drift, not the cost of a
block's first run (tf2-gate-1, 20 Oct 2026: 15.8 % on one series). So the blocks' orders are Latin
squares: in every k blocks (k sides) each side opens one block and holds each place once.
Each square's relabelling and row order are drawn from the round's seed (`GATE_SEED`, else drawn
and kept in the results), as is the cases' order in each block; both are recorded in every
report. A case's blocks are whole squares (release_gate.py `whole_squares()`: with three timed
sides 2 blocks become 3, 8 become 9): a partial square leaves part of the cost on one side. Every block times each
side from a copy of its build made for that block and removed after it (`fast.py block
--fresh-copies`): new files, read into new page-cache pages (below, *the unit*). A block writes
one report per side and run (`pass-<n>-<side>.json`, each record with its block, layout and
nanosecond times, the report its seed, orientation and copy).
A round's first two blocks run every case, each later block only the cases whose noise needs
more ([fast/measure.toml](fast/measure.toml), "Noise: reduced, not tolerated" below).

**The schedule is checked, not assumed.** Every required baseline is there; every block has
each side's two runs, one layout for all of them; each case's six runs in a block are back to
back and a palindrome (by their recorded nanosecond times); blocks don't overlap; every case is
in at least the round's blocks (two; fewer is diagnostic, never a release verdict; a declared
outcome's case one) and, with several layouts, its blocks cover them evenly. Each report is
complete, of its block's plan, on the clean harness revision at its start and end, on its side's
verified binaries copied for its block, one host and configuration, measured by the same calibration; the CPU
frequency policy the blocks met (each records the host's state) is the same in all.

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

**The inference** (`suitekit/gatestats.py`; research round 8, 19 Oct 2026). One question per
hypothesis, all of them declared before any result, the *family*: does the head lose on this
dimension (a case's series' time, a client width's QPS, the scope's anon + kernel memory, the
peak live requested bytes) against this baseline? `release_gate.py plan` writes the family into
the plan (cases x their series and gated metrics x both baselines; about 1,200 hypotheses over
phuoc-yu and the big host); a declared hypothesis a round doesn't test counts at p = 1.

- *The unit.* A block's contrast is log(head's median in the block / the baseline's). With
  several layouts the blocks of a layout are averaged first: the layout is the unit, so its
  variance (tau^2 / K) stays in the error however many blocks repeat it; with one build per side
  each block is a unit. A block is a unit only because it times copies of its own (lks, 20 Oct
  2026): two rounds timing the same two files gave creaders' read p50 x1.032 and x0.951, each at
  p = 0.001, and a build against a byte-identical copy of itself (lks-aa-20) x1.048 in one session
  and x0.983 in the next, the same in every block of a session. The offset belongs to the file's
  cached instance (up to 0.045 in log against 0.0075 between blocks of one instance); blocks
  timing one instance share it, so the round's blocks weren't independent and its p was
  anti-conservative. A copy per block draws the offset anew: it is in the blocks' variance, and
  a confirmation round no longer inherits round 1's instances. That each copy draws it
  independently is assumed, not yet shown: lks-aa-20 shows that two fixed files differ, not
  that copying and deleting per block redraws the offset (pages can be reused). The first A-A'
  floor run (phuoc-yu) tests it, its copy-to-copy spread across blocks against lks-aa-20's
  between-file spread; until then the false-block rates of `probes/gate-falseblock.py` (0 of
  2,000 simulated A/A gates with copies, against 0.2 % to 100 % without, by blocks a round) hold
  under that assumption only.
- *The floor.* `release_gate.py floor` writes `fast/noise-floor/<host>.toml`
  from an A-A' gate on that host (every side one revision; the copies make them different
  instances): per case and dimension, the sd of one block's contrast. Each test takes its units'
  spread as at least that (floor / sqrt(k) for a unit of k blocks), so a loss must be clear
  against the noise measured between instances and sessions, not only against the few blocks'
  own spread. The report names the floor it used, or says the host has none. Open (Sol,
  20 Oct 2026; none blocks): the floor file records no host identity, source hashes or freeze
  time, and `read_floor()` checks none; `rounds_of()` doesn't check that rounds are distinct
  campaigns; with layouts timed unevenly often the floor takes their mean replication, exact only
  for equal counts; the copies' file system device isn't recorded; the instance directories are
  the gate's, outside `suitekit/ns.py`'s ownership.
- *Evidence fails closed.* A declared hypothesis of a case the round ran that has neither a test
  nor a stated reason is an error, which blocks the verdict. Stated reasons: the candidate fails
  or doesn't run the case; a baseline can't run it; the baseline doesn't report the metric
  (unsupported by it); or the candidate lost the evidence the baseline has (a series, a client
  width, a memory reading on any pass), which blocks.
- *One build, timed once.* A baseline whose build is an earlier baseline's (a confirmation job's
  main that is its base) is timed once, as that one; the report takes it as that side and leaves
  its hypotheses out of the family rather than declaring them twice. The candidate always runs.
- *A disturbed block is repeated once.* The quiet watcher (`scripts/lib/quiet-watch.py`) suspends
  only the job's own work and records what else ran; a block in which work not the job's own
  used more than a core in more than a tenth of the samples is disturbed. It is left out of the
  statistics, both sides (the disturbance is the host's, not a side's: leaving it out is
  unbiased), its reports kept aside, and run once more; a repeat disturbed again is kept, and
  the report lists it (the orchestrator's policy, 20 Oct 2026).
- *The test* is one-sided for a loss (a better head is never a loss), a t-test on the units'
  mean log contrast (Student's t computed in the kit; it matches SciPy). Its calibration on
  identical binaries is checked below; an exact sign-flip p is reported beside it where the
  units are 16 or fewer, as a check on the tails, not used to decide (with few units it can't
  reach Holm's thresholds: its smallest p is 2^-n).
- *Rounds are prespecified.* A loss counts when two consecutive rounds show it: rounds 1 and 2,
  or 2 and 3 for one first seen in round 2 (while confirming another case's: kept, never
  dropped). A hypothesis's p is min(max(p1, p2) / 0.9, max(p2, p3) / 0.1), a round not run
  counting 1 (a Bonferroni split over the two paths). Round 2 runs (`release-gate.sh confirm`:
  `rg-<h8>-confirm`, its cases on the same host, their calibrated blocks, 2 to 30) the cases
  with a hypothesis at p1 <= 0.045; round 3 those whose round-2 p Holm could still reject if
  round 3 came out at 0 (with a thousand hypotheses that takes about alpha / m, not 0.005).
- *Holm over the whole family* at alpha 0.05 decides: what it rejects is a confirmed loss
  (blocks unless a trade-off covers it); what it doesn't passes at the end of its budget. No
  interim look: no block early and no pass early, so the error needs no spending.
- *The counter's resolution is no verdict either way.* A difference within the counter's step
  (1 µs, 1 µs / K batched, a coarser reported step, a byte) that the test doesn't reject is
  *unresolved*, reported; a consistent one is tested like any other. No relative tolerance.

Other blocks (a case failing, lost series or answers, a run that doesn't qualify) aren't
retried away: they block from whichever round has them.

**What it gives** (`probes/gate-power.py`). Power is analytic: the noncentral t at alpha / m,
m about 1,200, a contrast's sd the series' run-to-run sd (each side's two runs of a block against
the other's: sigma^2 / 2 + sigma^2 / 2). A round needs, for 90 % power a round (two rounds: 81 %
together, 90 % as sqrt 0.9 a round in the plan) against a 2 % loss: 9 blocks at sd 0.5 %, 16 at
1 %, 40 at 2 %, 79 at 3 %, 206 at 5 %. The false losses on identical binaries, recomputed from
the A/A passes of 18-19 Oct (`gate-power.py aa`: the gate's own comparison and decision on
random pseudo-blocks), are in [NOISY.md](NOISY.md) with the noise per case. A noisy series
barely decides a small loss: those are a work list of their own ([NOISY.md](NOISY.md)), to be
fixed, not tolerated.

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
  `GATE_BATCH`). A series batched on both sides in every run has its sample's resolution, 1 µs /
  K: the report's microsecond steps, which a batch divides. v1.5's and main's perf labs can't
  batch, so against them a loss within a microsecond stays unresolved; the gate whose baselines
  batch (v2's successors against v2) resolves them. Likewise a series reported in coarser steps
  (an older perf lab's open time in whole milliseconds, the EL classifier's phases to a tenth)
  has its step as resolution (`release_gate.py quantum`).
- *Code layout measured, not argued* (Mytkowicz et al., ASPLOS 2009; Curtsinger and Berger,
  STABILIZER, ASPLOS 2013). With `GATE_LAYOUTS=K` (`release_gate.py jobs --layouts K`) each
  side is built K times: layout 1 the plain build, layout L its timed tools linked with their
  functions in another order (rust-lld `--shuffle-sections=.text*=L`: the same code, other
  addresses; codegen units are not varied, that would change the optimisation, not the layout).
  Block b runs layout (b - 1) mod K + 1 on every side, so any K blocks in a row run all K; the
  layout is the test's unit (above). Built once per revision and layout, reused by every case
  and gate. Each process also gets an environment pad of random size (its initial stack moves,
  on top of address randomisation). `probes/gate-power.py layout` splits each series' variance,
  per revision, into the layouts' and the rest's.
- *Builds are what they say* (`scripts/lib/gate-publish.sh`). The linker is the toolchain's own
  rust-lld (x86_64-unknown-linux-gnu's default since Rust 1.90; every side names 1.98.1),
  through `cc`, never a host's configured one (RUSTFLAGS replaces a config's, the linker
  variable is pinned); each build reads it back from its binaries (`Linker: LLD 22.1.8`) and
  fails for any other. A build lives in the directory of its identity (the commit built,
  toolchain and `rustc -vV`, the flags rustc gets, linker driver, profile, layout), so binaries
  from before are reused only when all of it matches; its `IDENTITY` adds the linker read back.
- *No thread started in a timed window.* Every revision's perf lab started a canceller thread
  per run just before its timer, and the thread's start ran inside the timed call (1-5 µs on
  microsecond queries, against 0.3-0.45 µs between the engines in a clean loop). One watchdog
  thread for the whole process, started and running before any timing, armed before the timer
  and disarmed after, replaces it: in the head's perf lab, and in v1.5's and main's through
  their *measurement commits*. A measurement commit is a reviewed commit on a side's revision
  that changes nothing but its perf lab (`crates/nrese-store/examples/`); the gate builds and
  times it for that side ([fast/measurement-sides.toml](fast/measurement-sides.toml), pushed as
  `refs/measure/*`). `release-gate.sh build` refuses one whose parent isn't the side's revision
  or that changes anything else; nothing is patched at build time. `BUILD.txt` names the
  commit built, `CAPABILITIES` its timing, and the report refuses sides timed differently.
  `probes/timing-loop.py` measures what the loop adds.
- *Nothing timed that the gate doesn't judge.* A gate's pass doesn't rerun a case past its
  limit (the frontier rerun: 226 s of a 1,018 s pass), and a case with a declared outcome (a
  known limit, whose status the gate checks but never times) runs in the first block only.
- *Blocks where the noise needs them.* Each case's noise (the largest sd of log pass medians
  among its series whose 2 % is above the resolution) sets its blocks a round for sqrt 0.9
  power against a 2 % loss at alpha / m (`gatestats.power_at`); every case gets two, and the
  blocks beyond go where they buy the most power a second, until the timed part's budget, at
  most 30. Later blocks carry the noisy ones.
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

**Memory, one policy** (research round 8). Tested, where both sides report them: the scope's
anonymous and kernel memory at its peak (Linux: `anon` + `kernel` of the case's cgroup
`memory.stat`, sampled every 50 ms by fast.py outside the measured process; the file cache the
case's reads were charged is left out, it depends on what the host had cached: rl-lubm100 2.3 GB
of 4.7) and, with `--count-allocations`, the perf lab's peak live requested bytes (allocated
and not yet freed; `requested_mib` is the cumulative total, never a peak). Reported, never
tested: the process's own peak by its kind (`peak_kind`: VmHWM on Linux; on Windows
`peak_private_commit`, PeakPagefileUsage, beside `peak_working_set_bytes`), each phase's peak
(its own window: none where the platform can't start one, never the lifetime's under the
phase's name), the cgroup's `memory.peak`. Work counters are reported (more than 2 % more work
is marked); the gate's question is time and memory.

**W3C and guards** (office-pc, `release-gate.sh w3c`, `guards`). The suites run with their
data required (`NRESE_W3C_REQUIRED`: a suite without its data fails, never skips; the OWL 2
test cases pinned by SHA-256) and `--nocapture`; each suite's counts are checked against
[fast/w3c-expected.toml](fast/w3c-expected.toml): no failure, at least its passes, at most its
known skips and disputes. The guards are the test names §0's Guard column gives; each must run
and pass (a name no test has, or an ignored one, fails: docs and code disagree).

```sh
bash scripts/release-gate.sh jobs HEAD --out jobs/   # build, timed (phuoc-yu), big (khaleesi-wsl), checks (office-pc)
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
