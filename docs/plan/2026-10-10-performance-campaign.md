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
7. Preserve existing CPU ownership; caller bypass and the previously rejected
   caller-budget experiment remain unapproved. Earlier single-entry coalescing was
   not accepted. Do not repeat an unchanged rejected experiment as a new proposal.
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

## Progress and resumption

- Initial checkpoint `bebada2` passed normal Office commit/push gates, including three
  additional semantic seeds. This is not a full current-head performance gate.
- Fresh read-only reviews confirmed the existing fast/native runners and highlighted
  worker-submission overhead and a potential cache single-flight progress interaction;
  neither justifies a production change without a targeted reproduction/profile.
- Office/Phuoc preflight: both reachable and idle at campaign start. Office has Docker
  and Java; Phuoc currently has neither. Assets stay under the verified preservation
  root `/home/krarilotus/nrese-assets/kraris-gptler-20261009` on each host.
- Both hosts now run all 95 registered fast cases against frozen `4e8bd38`, with
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
- Initial completed client pairs show lower QPS at every sweep width on both hosts.
  Phuoc's better high-concurrency p99 must not hide the throughput loss. Client peaks
  include one retained latency sample per completed request, so a slower run's lower
  peak does not establish an engine-memory improvement. The single-level QPS omission
  is being repaired in `fast.py`, reusing the sweep comparison and retained full reports;
  the frozen measuring driver and checkpoint binaries remain unchanged.
- No performance fix is approved/accepted merely because a benchmark has completed.
- Intermediate comparison at 108 Office / 197 Phuoc invocations: 18 / 32 complete
  paired cases. Beyond client throughput and skewed triangles, Office QL-LUBM10
  query-phase RSS and Phuoc RL-clique memory trigger investigation. Initial DL/EL
  cases show no qualified large latency change; existing failures retain no timing
  credit. These are three-pair screening results, not final performance acceptance.

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
