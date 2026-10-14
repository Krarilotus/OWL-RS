# Benchmark completion handoff — 14 October 2026

This package reunites the existing release checks and diagnostics on
`integrate/v2-completion-20261014`. The selection below preserves their provenance;
the acceptance steps describe the remaining measurements, not completed runs.

## Reused integration selection

| Commits | Selected scope |
|---|---|
| `8b8caa98`, `d4e2d16e`, `d629677c`, `5895ee81` | Release runner/report and report tests: symmetric passes, fixed known issues, older perf_lab routes, comparable memory, explicit not-run cases |
| `5f215ea4`, `82c351e5`, `59a4a9a7` | Seed manifest/sweep, capped command helper and tests; preserve build/cap failures apart from semantic findings |
| `596d483f` | Required pinned conformance data, preparation/count reporting, check script and remote opt-out forwarding; exclude submission policy |
| `c3be0a21`, scoped `9be91ed6` prerequisite | Fleet slice placement, task cleanup and Docker cap; host binaries under the same scope cap as image-backed contenders |
| `368c4eaf` comparison hunks | Shared `CACHE_KINDS` prerequisite and general comparison's page-cache exclusion; retain the release gate's explicit cgroup comparison policy |
| `3fd55a1e` helper prerequisite only | `host_command` environment forwarding needed by required conformance kits; exclude server restart/query-mix changes |
| `7420ff64`, `4d5dd920` | Existing revision bisect and query-swing diagnostics, unexecuted here |

The full `e0f84185..59a4a9a7` chain was inspected. Excluded queue scheduling,
packing/fairness/backfill/prerequisites, queue hooks, legacy release-gate submitter,
quiet-window/watch/host configuration and Khaleesi load-stage changes. Fleet owns
scheduling. Unrelated vendor editions, reports, void-row handling, crash recovery and
engine/perf_lab changes are outside this selection; no licensed result records are added.

## Local repairs

- `fast.py` forwards `case.timeout_s` to every measured tool in Docker and native
  runtimes. Omitted: 900 seconds per invocation; explicit `0`: unlimited wall time.
  Tableau retains its 300-second per-file search default when omitted and receives `0`
  for unlimited search. Setup, repeats, sweeps, checks and coverage are preserved.
- Shared cleanup retains the counted-query coverage, route compatibility, empty-run guard
  and tableau memory envelope.
- Reports expose harness SHA (`git.commit`) separately from engine SHA
  (`binaries.revision`). Mixed harness revisions are refused within a pool and prevent a
  gate across sides from passing. Historical reports without provenance remain readable;
  they do not certify the final head.
- The release runner writes to `FLEET_OUT` where set and propagates the report's exit
  status. No replacement scheduler/submitter is introduced.
- Release acceptance requires four complete passes per side, with exactly the intended
  cases and repetitions. Resume, post-run and final-report checks bind the requested
  engine and clean candidate harness to independently read definitions/selection and
  verified `REVISION`/`SHA256SUMS`. Each pass records source and binary provenance at
  both ends. Comparable successes require complete valid timing series; failed baselines
  and known boundaries remain visible as data. Diagnostic overrides cannot grant release
  acceptance. Interrupted or stale passes and previous final reports are archived before
  retry; reserved or duplicate baseline names are rejected.
- The runner keeps compact records under `$OUT/fast/compact` (`NRESE_FAST_BASELINES`),
  preserving a clean source checkout; fast's ordinary compact-record location is unchanged.
- Query-swing reports raw distributions only when every process supplies all requested
  raw samples. Otherwise it reports independent process p50s with the actual process
  count; reported min/max spans stay separate. It never pools summaries as timed samples.
- Git fixture setup and pinned-suite fetching isolate repository-local hook variables.
  Temporary repositories cannot redirect their writes into the caller's index or branch;
  the gate retains its own staged-index environment. Offline guards cover both new and
  cached fixture repositories and verify the caller's refs, configuration and indexes.

## Checked here

The completion worktree passed the full Python suite: 219 tests, seven platform/environment
skips, no failures. This includes capped-command fixtures, Git hook-context isolation,
query-swing freshness, exact campaign coverage, expected provenance, and complete timing
series. Independent review rejected all seven original false-pass counterexamples and
passed 24 bound positive/mutation probes. The integrated count includes three offline
pinned-data fetch guards. The HTTP fixture closes its server socket after shutdown;
its 14 focused tests pass with resource warnings treated as errors (one environment skip).
No real benchmark workloads run in these tests. Changed Python parses and the changed
shell scripts pass syntax checks.
The original `a425ee5d` perf_lab path demonstrably omits the declared 1800-second limit;
the new tests require that limit, its default and unlimited form through both runtimes.

Focused checks (PowerShell, from this worktree):

```powershell
$env:PYTHONPATH = 'benches/suite/tests'
python -B -m unittest test_fast_timeouts test_release_gate test_submission test_fleet_slices test_bisect_perf test_query_swing test_problem_finder_chunks.SeedSweep test_fast_memory test_fast_counters test_cases test_runtime
```

## Remaining acceptance

1. Review the staged selection and final integration diff. Normal local validation is
   allowed: `NRESE_BUILD_HOST=local bash scripts/check.sh push` (or the configured build-host
   path). Timings run through Fleet. No checks requiring builds were run in this staging task.
2. Use versioned `scripts/check.sh`, `scripts/fetch-w3c-tests.sh` and the conformance kits
   at the final head. Keep W3C data required; record each suite's pass/fail/skip counts and
   preparation failures. Prepare only in job-owned scratch, never point the mutable fetch
   helper at verified read-only shared inputs. Record harness SHA and tested engine SHA
   separately (equal when both come from that same clean final checkout), plus toolchain
   and existing manifest/input identities. Optional/skipped runs are not final acceptance.
3. Use the existing timed-work workflow through Fleet for `scripts/release-gate.sh`.
   One clean final candidate harness drives engines built at `0f46563`, the
   selected current main and the final candidate. Retain the existing pass order
   `base, main, cand, cand, main, base` twice, every declared case, one host/configuration,
   binary hashes and all raw failures/losses. Use a run-specific gate/result directory so
   resumable labels cannot reuse another candidate's reports. Keep every baseline failure,
   not-run case, small slowdown, memory-comparability limitation and known issue visible.
4. Frozen BSBM 100M stays immutable and remains an acceptance blocker. The pinned legacy
   generator writes bare `e0` prices in its N-Triples serializer: these are Turtle doubles,
   not valid N-Triples objects. An existing RIOT 6.2.0 sample conversion preserves their
   lexical forms as `xsd:double`, alongside sampled string and date terms. Full-input
   preparation is not yet qualified. Its separate derivative must retain the original
   hashes, generator, `.td` files and queries; complete independent parsed-stream comparison
   and fresh reference-answer qualification must precede timing. Sample success alone
   cannot qualify the full input or any benchmark result.
5. The stream-to-disk/SQLite answer-recorder design remains unapproved. Existing answer
   capture is retained. Licensed vendor numbers remain private; no messages to Opus.
