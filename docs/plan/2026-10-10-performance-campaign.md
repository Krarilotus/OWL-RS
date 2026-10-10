# V2 checkpoint and performance campaign, 10 October 2026

This is the durable task reference and continuation entry point for the campaign
requested below. After each compaction, read this file, the current checkpoint/run
records, and the owning architecture before resuming. Do not restart completed runs
or treat an earlier revision's results as current-head qualification.

- Task reference: [user request](#user-request), preserved verbatim below.
- Audit destination: [draft PR #23](https://github.com/Krarilotus/OWL-RS/pull/23),
  branch `plan/engine-v2-performance-architecture`, base `refactor/engine-v2`.
- Frozen initial checkpoint: `bebada2e6631b58ed0554ee302572570e80b085e`.
- Owners: [architecture](../ARCHITECTURE.md), [benchmark protocol](../../benches/PROTOCOL.md),
  [host/data guide](../../benches/README.md#remote-hosts-and-shared-data).
- Prior evidence: [status](../STATUS.md), [performance log](../design/performance.md).

## Execution contract

1. Freeze source, binaries, inputs, seeds, settings and machine identities before
   measuring. Keep the initial checkpoint immutable while subsequent fixes get
   separate candidates. Compare revisions and competitors on the same host/runtime.
2. Office alone builds/tests/formats Rust and runs build-triggering hooks. Phuoc-Yu
   executes verified compatible Office-built binaries. Reuse the existing checkout
   and target on Office; no local Rust or per-helper worktrees/build caches.
3. Initial estimate: 8–10 hours for the runnable checkpoint and prioritized competitor
   pairs across both hosts; later repair/retest cycles are additional, evidence-led work.
   The 95 fast cases are one suite, not additional to the registry's `fast` bridge.
   Partial/planned adapters and unsupported semantics stay visible as exclusions.
4. Evaluate each completed batch: correctness and completeness first, then speed,
   throughput by width, tails, lifetime peak memory, phase/retained memory and disk.
   Keep failures/timeouts and previous red cases. No cherry-picked intersection of
   successful work, weakened checks or treating reset VmHWM as lifetime peak memory.
5. Use three interleaved outer repetitions for initial regression checks, and ten
   for final published performance claims. Predeclare microcase latency floor zero;
   retain spread/bootstrap checks and the existing independent QPS/memory criteria.
   Below-floor changes and overlapping ranges do not establish general neutrality.
6. Investigate causes against original architecture and the read-only research corpus.
   Fix the owning layer, reuse existing code, remove what becomes obsolete, and keep
   modules lean. Before appreciable production wiring, describe the problem, necessity,
   first-principles rationale, scope and tradeoffs for the owner's checkpoint.
7. The owner's 10 October correction supersedes the mandatory physical-pool assumption:
   resource policy must be configurable and execution suited to the actual work. Rewind
   unconditional default handoffs while preserving explicitly selected limits and
   independent correctness fixes. The rejected FIFO caller-budget and whole-result
   coalescing experiments remain rejected; do not repackage them as the correction.
8. Retest exactly affected cases plus neighboring correctness/performance controls.
   Reject/revert regressions; record rejected ideas, not just retained improvements.
   Stop adding speculative complexity where evidence does not justify a change.
9. Competitors get applicable semantics and documented defaults/home-turf tuning.
   The main view may filter unsuitable/noncompetitive pairs, but raw completed results
   and an explicit exclusion ledger remain auditable. No guaranteed universal victory
   or literal proof of optimality is implied by an exhausted measured campaign.
10. The owner selected **private licensed numbers; public coverage/status** on
    10 October. GraphDB/RDFox/Stardog/AnzoGraph numbers remain in a private HTML report;
    public PR/docs include only permitted results and restricted-system coverage.
11. Remove only verified owned finished containers/stores/scratch. Preserve MongoDB,
    immutable shared snapshots, results, hashes, seeds, checkpoint binaries and caches
    needed for planned reruns. Do not use legacy broad cleanup/prune scripts.
12. The owner's later benchmark correction separates native-capacity, explicit
    matched-resource and scaling/frontier scenarios. No new cap, smaller input,
    omitted case or weakened answer check may compensate for harness overhead.
    Preserve the original kits and extend their owners. Target fast focused feedback,
    1–4-hour area campaigns and a roughly one-day runnable full profile; these are
    scheduling targets, not measured guarantees or replacements for repetitions.
    The [suite map](../../benches/README.md#feedback-cadence-and-scenario-selection)
    and [protocol](../../benches/PROTOCOL.md#3-fairness) own these distinctions.

## Progress and resumption

- Owner correction after the report: the default physical-worker requirement contradicted
  configurable, use-case-driven execution. Root is selectively rewinding that policy in
  `Workers`/`Runtime`, retaining QL/DL correctness and optional explicit worker limits.
  Office focused tests and the normal commit gate pass. Phuoc's separate three-pair
  recovery screen of lookups, mixed commits, skewed/uniform joins and aggregate memory
  passes all 24 invocations, with no current-comparator flags. The ten-pair lookup
  follow-up confirms recovery near baseline. Office completed all 95 cases in three
  pairs: 570 reports, 85 valid cases, six expected boundaries and four known both-failed
  cases; no new comparator flags. Its driver removed 23,062,991,016 logical bytes of
  finished store scratch, preserving inputs and evidence. Phuoc completed ten pairs
  of all 14 affected/control cases: 280 reports, 340 native
  invocations, all registered checks pass and no comparator regression flags. The
  [recovery record](../../benches/runs/2026-10-10-default-execution-recovery.toml)
  preserves the dirty source patch and separate cohorts. Compare against `4e8bd38`
  and the failed `bebada2` checkpoint separately. The pending Office documentation-commit
  service was stopped before applying its now-superseded patch; no hooks were bypassed.
- The restricted competitor queue omitted existing home-turf configurations, runners and
  data. Its results remain screening evidence. Reuse the historical Office home-turf batch
  and vendor-specific qualification paths; do not present materialised-query timings as
  the requested complete loading/reasoning/concurrency comparison. The fresh inventory
  is retained under `tmp/regression-cause-20261010`. Root interrupted the restricted
  Office queue to validate recovery; its completed reports and interrupted-row receipt
  remain under the original campaign root. No failed or interrupted row becomes a win.
- The owner approved bounded consolidation of the existing campaign wrapper and full-answer
  recorder on 10 October: select preserved DATA/FAST inputs and current verified binaries,
  dispatch the existing DL reference runner, and require the complete selected answer matrix.
  The two campaign files pass 24 focused Python tests and independent review; verified
  copies are staged on Office while native recovery measurements continue unchanged.
  Captures cover every selected query, regime, cache mode and fresh-store
  repetition, outside timed query execution; they do not inspect every timed response.
  Replay equality and timed-row coverage/status have separate verdicts; both must pass.
  Missing, failed and disputed captures cannot qualify a comparison. No new engine,
  timing harness, dataset copy or local Rust build is part of this work.

- The owner approved the revised filtered-vector repair after its ownership review:
  SPARQL's collector merges sorted rounds, retains each winning candidate's probe rows,
  disposes displaced rows and their existing capacity charges, and combines final winners.
  It replaces append-only selection, terminal sorting and eager probe unions. Search
  settings, index algorithms and public APIs remain unchanged. Tests and matched speed/
  memory measurements must pass before acceptance; ranking fetched candidates alone
  does not establish approximate recall. All 22 focused Office tests and the normal
  commit gate passed; both release artifacts and their source manifests were verified.
  A separately approved benchmark-only option reuses synchronous graph preparation and
  records coverage before ordinary query timing. Both comparison builds receive the same
  instrumentation; preparation memory/time stays separate. The two revisions reuse one
  Office checkout/target, preserving immutable baseline binaries and exact source patches.
  Phuoc's three-pair diagnostic completed all 30 exact-control processes with matching
  complete answers and no changes beyond the predeclared thresholds. The separate
  ten-pair qualification also completed: 100 processes, 500 saved answers, all 25 query
  comparisons within the thresholds. This establishes neither a speedup nor universal
  equivalence. All three Auto families fail the unchanged recall
  gate on both binaries and receive no timing credit. Parallel HNSW construction varies
  between processes, so differing recall in one fresh-build pair does not identify a
  collector regression. A counts-only single-graph diagnostic confirms the collector
  retains every available exact winner; missing winners were never exposed by the
  original search. A second graph gives 24/50 expected hits through Auto, 34/50 with
  explicit search effort 256, and 50/50 through the existing candidate-first route.
  That route also changes effective effort, so this is not an isolated routing win or
  a performance measurement. No search-policy change follows without matched testing.
  See the [vector record](../../benches/runs/2026-10-10-vector-collector-repair.toml).

- A small HNSW allocation experiment reuses one adjacency snapshot buffer per traversal
  routine, replacing repeated clones in the existing index owner. It adds no algorithm,
  policy or public API. All 432 frozen-graph comparisons preserve ordered results,
  distance bits and acceptance calls. Office's normal gate and release build pass;
  the separately frozen binary completed Phuoc allocation and ten-pair qualification.
  All 100 exact-family processes pass, with no detected regression across the
  predeclared query, preparation and memory comparisons. Graph-preparation allocation
  calls fall 81.18–81.44%, and cumulative traffic falls 44.62–44.68% in the separate
  instrumented cohort. These independently built graphs do not establish identical
  topology, and no latency or peak-memory reduction qualifies. Small upward memory
  observations are preserved, including the probe query-phase median +7 MiB.
  Oversized imported adjacency may retain capacity until the routine returns. This
  is an allocation-efficiency result, separate from the collector repair and from
  approximate search quality. All original host controls are restored; raw answers
  and the completed audit remain on Phuoc. No new build target or dataset copy exists.

- Home-turf answer qualification now uses the current frozen vector candidate, rather
  than the earlier recovery binaries. The public aggregate smoke completed all selected
  slots and full replays, but strict cross-system answers disagree: tied ordering,
  numeric datatypes and AVG precision need separate dispositions. Follow-up diagnostics
  locate Virtuoso's extra statements outside the workload graph, confirm aggregate
  inputs, and preserve numeric/type disagreements. Equal row counts do not qualify it.
  Existing DL references and native counterparts completed independently on Office:
  seven matching reference pairs, four reference timeouts, one reference error and a
  Horn taxonomy dispute. Complete EL signatures/taxonomies match ELK and Konclude.
  Horn's saved reference omits explicitly asserted subclass relations. A separately
  retained raw Konclude hierarchy and its exact adapter-API replay both match native;
  the original invocation deleted its intermediates. Three fresh normal invocations
  retain identical converted input. One raw hierarchy loses 107 asserted pairs and
  305 entailments, and its final Java output preserves that same closure; the other
  two runs match native. The reproduced loss precedes final extraction, but the
  precise Konclude classification/output cause remains unknown. Preserve every
  result and the original dispute; no OWL-RS or adapter change is justified by this.
  These one-repetition checks establish neither comparative speed nor final memory peaks.
  The owner approved the benchmark runtime repair for container-lifetime peaks and CPU
  placement. It reuses the existing runtime and must first prove retained accounting
  survives container exit. The first proof stopped before creating a container or
  slice: Office denied unprivileged system-manager access and noninteractive sudo
  requires a password. Cleanup and the frozen source are verified. Implementation
  remains gated on a successful lifecycle proof; no host-security change is authorized.
  earlier sampled-memory cohorts retain their original limitations and identities.

- Four existing public home-turf cases completed once: star, chain, snowflake and
  sideways. All 56 selected capture attempts and all 168 intended measured slots
  remain in the ledger: 55 complete answers and 165 successful measured slots.
  Chain queries and values-star match complete bags across all selected systems.
  Numeric datatype differences remain under adjudication; unordered LIMIT needs a
  legal-output check rather than assuming one exact cross-system subset. Oxigraph's
  EXISTS timeout and failed replay remain visible. The 14-minute-27-second elapsed
  cohort informs scheduling only, not a speed ranking. Office is released.
- ANN stage 03 uses one prepared graph and 30 declared query outputs: original Auto
  exposes 27/50 oracle winners, literal effort 640 exposes 45/50, and the existing
  candidate-first route exposes 50/50 at effective effort 640. Exact controls match
  all 50; raw-prefix checks pass. Equal beam settings do not mean equal traversal
  work or memory. There is no timing, robust-recall or default-policy claim; the
  proposed compact eligibility/routing work still needs its scope decision.
- The owner rejected the recorder's proposed capped diagnostic as the wrong framing.
  The full large-result case remains selected. Remove campaign-wide retained answer
  state at the validation owner, preserve full raw answers, and keep comparison work
  outside measured engine execution. Review the revised design against the existing
  suite before implementation; no capped run or reduced workload was authorized.

- The QL q15 route guard was corrected in the registry: the fixed fixture requires no
  additional witness union, as supported by existing realised-witness elimination and
  its missing-witness regression test. The 21-answer check remains, with q15 soundness
  and completeness now checked explicitly. All 24 retained Office raw invocations pass
  the revised checks. This is labelled revalidation, not a rerun or retroactive change
  to the frozen 95-case cohort's failed status; no production change was needed.

- Initial checkpoint `bebada2` passed normal Office commit/push gates, including three
  additional semantic seeds. This is not a full current-head performance gate.
- Fresh read-only reviews confirmed the existing fast/native runners and highlighted
  worker-submission overhead and a potential cache single-flight progress interaction;
  neither justifies a production change without a targeted reproduction/profile.
- Office/Phuoc preflight: both reachable and idle at campaign start. Office has Docker
  and Java; Phuoc currently has neither. Assets stay under the verified preservation
  root `/home/krarilotus/nrese-assets/kraris-gptler-20261009` on each host.
- Both hosts completed all 95 registered fast cases against frozen `4e8bd38`, with
  three interleaved outer pairs (570 invocations per host). Office uses its pinned
  Rust container; Phuoc uses native user services. Cross-host wall times are not
  combined. This first checkpoint precedes competitor and repair/retest phases.
- Remote root on both hosts:
  `/home/krarilotus/nrese-prep-20261009-4e8bd38/campaign-20261010-bebada2`.
  Owned user units: `owl-checkpoint-office-20261010` and
  `owl-checkpoint-phuoc-20261010`. Read `checkpoint/active.json`, `checkpoint.log`
  and `progress.json` before interacting; `PAUSE` stops dispatch at a case boundary.
- Frozen source archive, all nine binary hashes, toolchain/build receipt, machine
  inventories and per-input SHA256 receipts accompany the runs. Existing verified
  inputs are shared read-only; only two missing companions (176 bytes total) were
  transferred to Phuoc. Binary views use hardlinks, not duplicate builds.
- The disposable transport reuses unchanged `fast.py` workloads/checks. Native
  cgroup capture corrects its host-root probe for registered cgroup checks; exact
  original output is retained. Maximum workload-service lifetime peak across
  sweeps is reported separately from original/phase/per-width RSS. Missing memory
  remains unknown. Setup/reopen, DL, classification and expected OOM smoke checks
  pass on both hosts. Failed transport smokes are retained outside qualification.
- Initial owned transfer/smoke cleanup reclaimed 74,681,627 bytes on Office and
  44,473,103 bytes on Phuoc. Exact paths are in `smoke-cleanup.json`. Measured store
  scratch is removed after final use; answers, logs, hashes and checkpoint binaries
  remain. No shared snapshots or MongoDB resources were touched.
- Bootstrap comparison runs on the coordinating PC to avoid adding analysis CPU
  load to timed hosts. Only cases with all three complete pairs receive a verdict;
  failed statuses, missing series and boundaries retain the protocol's checks.
- Office reproduced the nested Rayon/cache-flight dependency cycle with a disposable
  two-worker test: forced ordering and cleanup assertions pass, then the progress
  assertion fails after the five-second watchdog cancels only the waiter. Source
  is restored clean. Evidence: `flight-diagnostic.patch`, `flight-diagnostic-test.log`
  and `flight-diagnostic-exit.txt` under the remote root. This establishes the
  composition defect, not its frequency or contribution to throughput regressions.
  Whole-query retry is unsafe for SERVICE/volatile evaluation and partial reservations.
  The owner requested a proper replacement design instead of approving intermediate
  recomputation. [Query progress design](2026-10-10-query-progress.md) proposes owned
  continuations on the existing worker pool, reusing kernels and cache semantics.
  Synchronous SERVICE/worker reentry is an explicit ownership decision still to settle;
  the complete migration touches low thousands of lines, not the earlier bounded bypass.
  The owner's subsequent question challenges whether this design is better; it does
  not approve implementation or the callback ownership change. Validate and compare
  the candidate before recommending a migration. Both remain unapproved.
- Initial completed client pairs show lower QPS at every sweep width on both hosts.
  Phuoc's better high-concurrency p99 must not hide the throughput loss. Client peaks
  include one retained latency sample per completed request, so a slower run's lower
  peak does not establish an engine-memory improvement. The single-level QPS omission
  is repaired at `62ae2e0` in `fast.py`, reusing the sweep comparison and retained full reports;
  the frozen measuring driver and checkpoint binaries remain unchanged.
- No performance fix is approved/accepted merely because a benchmark has completed.
- Full checkpoint audit: **1,140/1,140 invocations complete; comparison fails on both
  hosts**. No status/check pass-to-fail transition was found. Each host retains 85
  all-OK cases, six expected-boundary cases and four failed cases present on both
  revisions: DL transitive-400, 20M load under 2 GiB, QL OWL2Bench route and filtered
  vector recall. They receive no timing credit. Throughput falls at every measured
  client width; mixed readers/writes, skewed counting and phase memory remain leads.
  Office adds aggregate-query latency and reader-loaded write-tail flags. Two variable
  Office star-count speedup flags are screening observations, not acceptance.
  All 114 input hashes and all nine executable hashes per revision agree across hosts.
  Exact findings and limitations are indexed in the [run record](../../benches/runs/2026-10-10-checkpoint-bebada2.toml).
- Follow-on owned services started after completion of each host's checkpoint,
  without overlapping it. `owl-competitors-office-20261010` reuses the
  frozen suite for public store/reasoning small and scale tracks plus an RDF4J
  readiness probe. Its disposable `campaign-competitor.py` validates/pins existing
  image IDs, refuses pulls/builds and pre-existing resource replacement, hashes
  selected shared inputs and retains provenance. It uses the existing quiet slot,
  cleanup and measurements. Docker-stats memory remains labelled as sampled, not
  lifetime peak; three repetitions remain screening. Licensed comparisons are not
  part of this public queue. At the 11:24 UTC audit it is running the public LUBM100
  reasoning group. Kernel OOM records identify the six completed Nemo closure-to-answer-store
  failures as auxiliary Oxigraph imports exceeding the 12 GiB container cap. Reasoning
  completed, but answer correctness remains unvalidated. The generic invalid-IRI hint
  is not a parse-error diagnosis. A strict importer follow-up and retention of its exit/OOM
  evidence remain open; these are not established Nemo reasoning/performance losses.
- `owl-qualification-phuoc-20261010` completed ten new interleaved pairs for seven
  already-flagged cases using unchanged `campaign-fast.py --label qualification`:
  client lookups/sweep, skewed counting control, mixed commits/readers, fresh open,
  RL clique and aggregates. This confirms existing findings; it is not a repaired
  runtime or a performance-improvement claim. All 140 reports pass their recorded
  checks; the 520 workload calls complete without timeout/OOM kill. Every client-width
  QPS loss repeats in all ten pairs (approximately 67–80%); mixed commit p50 rises
  39%, skewed triangles 55%, and aggregate query-phase RSS maximum rises 108 MiB.
  RL-clique's initial maximum-based memory flag is not reconfirmed; retain both
  observations. Rounded open timing and unverified affinity/governor/quietness controls
  prevent treating ten repetitions alone as publication qualification. Results remain
  separate from the initial three-pair checkpoint; Phuoc has no continuing benchmark
  assigned by this service. Read `qualification.log` and `qualification/active.json`.
- Each host's checkpoint cleanup removed 23,062,991,016 bytes of owned scratch-file
  contents. This is summed file length, not measured physical
  free-space recovery. Shared inputs, binaries, results and MongoDB remain retained.
- Research synthesis and independent source/audit reviews completed after the user
  restarted the agents window. The verified connector now reaches real running T3
  model sessions; the stale queued synthesis was cancelled. Read and answer INPUT
  requests explicitly. No fallback helper permission or restart is currently needed.
- The owner's subsequent direction is a staged system-design path: establish
  request-wide solver cooperation and contracts, replace faulty ownership one slice
  at a time, then optimise work, representations and kernels. The existing
  [implementation plan §7](2026-10-09-performance-architecture.md#7-ordered-design-and-implementation-gates-10-october)
  owns that sequence and its rejection gates. The continuation proposal remains
  unselected; callback/embedding and shared resource accounting remain explicit gaps.

## User request

Source: the owner's message in this task on 10 October 2026. The application does
not expose a verified per-message URL; this repository anchor is the durable link.

> can you run afull benchmark suite (like 8-10 hours is fine you can estimate) split across the office and the Phuoc-Yu PC, make sure the clean up after but evaluate results and keep them as checkpoint for current state, add an overview to the PR draft for these, then after it completes, and during intermediary compeltions of parts of the benchmark suite, check for any regressions on memory or speed, and fix them one by one, make sure to fix them by analyzing and udnerstanding the originals architecture intent, consulting the research cataloge, and finding the best optimal abstraction layer to apply the fixes on, and also find any performance improvements still left on the table at the same time and implement those as well, making sure to keep code DRY modular and lean, unifying code thats similar and cleaning up code thats no longer needed
>
> -> When you go through step by step refer to this task here, make sure to safe that as a reference link somewhere so you can reference fter each compaction step
> -> after all performance improvements have landed, retest the benchmarks for those exact parts, and iterate auntil performance is cranked to the max. Make sure to also revert and reject changs that further regress performance instead, and get to a state, where we have the best version for v2 thats possible. When its all done update those performance numbers agaisnt all the other software stacks we can bench against, try to beat every single competotor on their home turf, and exclude benches for the competitors that obviously won#t owrk or that they obviously are much slower than us from the result comparison.
>
> -> then publish also those results to the PR and give me an overview table here in the chat over our numbers vs competitors for each benchmark in an easy digestible way, maybe just build a small html page to review them in a good format!
>
> Go hard and stay consistant. Make sure to follow best code practices, industry leading bleeding edge performance is the goal, while maintaining zero tech debt and lean overall code, thats mainly functional and modular

### Subsequent benchmark direction

The owner clarified that each application must receive the inputs and configuration
appropriate to the task. Native-capacity runs are uncapped by the benchmark unless
an explicit resource-comparison scenario calls for limits. Harness problems must
not be solved by reducing or removing benchmarks; preserve the original suite's
intent, renew obsolete parts at their owning layer, and keep the structure modular.

> We want to maximize performance under all circumstances thats what we need to therefore bench for while maintining a benchable profile for now, until we scale up
> -> so if the whole benchmark suite runs over the course of 1 day on all applications once thats fine, if we have modular smaller test benches for specific tests they shoudl run very fast and accumulative tests in an area of expertise shoudl come back in 1-4 hours tops so that we get benchable data and proper reads on what we need to improve and how we are measuring comapred to before or our competitors
