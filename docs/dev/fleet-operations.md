# Build and benchmark operations

Use the existing producer for the work, then inspect its result. Fleet owns host
placement and process containment; OWL's gate owns source and test selection;
the benchmark harness owns answers and statistical conclusions. A Fleet `done`
badge says the step exited successfully, not that a performance comparison passed.

This guide describes this source revision. The operator's deployment record must
identify the installed Fleet binaries and successful route checks before using a
new protocol. In particular, the container plan below needs Fleet job spec 7.

## Choose the entrypoint

| Work | Entrypoint |
| --- | --- |
| Commit checks | Normal Git hooks, under the existing local admission/commit-slot contract |
| Ordinary push | `bash scripts/check.sh push`; submits complete Linux and Windows parts |
| Narrow regression or compiler experiment | An immutable proof ref and a Fleet job using the existing host launcher |
| Targeted timing comparison | `benches/suite/abba.py`; [ABBA protocol](../../benches/abba/README.md) |
| ERA container case | [ERA prepare/consume guide](../../benches/breadth/era-shacl/README.md) and Fleet's bound container plan |

The installed Fleet repository's `skills/fleet-jobs/SKILL.md` owns job submission,
notices, priorities and cleanup. Read it before submitting. Do not copy host windows
or capacities from old notes: `fleet rules`, `fleet hosts --json` and `fleet why ID`
give the current situation. Legacy `queue.py`, direct SSH launches and private
copies of the gate are not alternative execution paths.

## Prepare once

1. Record the intended checkout, full commit and tree. A Fleet Git input pins a
   commit; it does not include uncommitted files. Use an isolated proof ref for a
   narrow pre-commit check, and keep ordinary hooks for the eventual commit.
2. Reuse verified inputs and compatible builds. Native reuse needs the same
   source, compiler, profile/features, flags, target and CPU compatibility, plus
   intact artifact hashes. An arbitrary binary with the same filename is not a
   cache hit. Preserve inputs and artifacts until their planned consumers finish.
3. Use an existing small case through the actual producer and launcher before
   expanding a new execution route. Check real child cwd, tools, inputs, outputs
   and cleanup. A successful preparation script alone does not exercise a workload.

Declare actual OS, tools and resources. For flexible untimed work, `owlrs.any`
without a host allowlist is normally sufficient. If a shareable kind supplies an
unnecessary role, explicit `requires.roles = []` clears it; omission inherits it.
Preserve timing roles and exclusivity. Pin hosts for actual input/native-artifact
locality or a same-host comparison, not because an old example did so.

The first Git/host-repository input supplies the checkout cwd. It does **not**
create `FLEET_INPUT_CHECKOUT`. Non-Git inputs receive `FLEET_INPUT_<NAME>` after
uppercasing and replacing punctuation with underscores. Inspect their staged
layout before appending paths. Use `$FLEET_OUT` for retained evidence.

## Submit and collect

Set `FLEET_OWNER` to the submitting thread's own identity and register its notice
route as the Fleet guide describes. Treat that route's URL as a credential.
From the submitting machine, with `fleet` resolving to the installed CLI:

```text
fleet submit owned-job.toml
fleet show JOB --json
fleet why JOB
fleet results JOB --json
```

Retain the acknowledged job ID. After an ambiguous submit, inspect that ID before
trying again. Prefer completion notices to repeated polling. Use the returned
results `path`; do not guess directory nesting or rely on a remote job worktree
surviving collection. A job dependency orders work; an output declaration or an
explicit retained host artifact supplies its data.

## Ordinary push and continuation

Run from the intended committed checkout:

```text
bash scripts/check.sh push
```

On native Windows, use the selected Git installation's Bash through the existing
`scripts/lib/git-bash.ps1` resolver. The producer constructs its own native argv
and EOF input; do not wrap the whole argv array as a single shell filename.

The push producer binds the committed `scripts/lib/push-base.txt`, HEAD/tree,
changed paths, compiler and flags. Its request is saved under the Git common
directory's `nrese-gate-requests/<key>/` before submission. It returns:

| Exit | Meaning |
| --- | --- |
| 0 | Both matching platform parts have complete validated evidence |
| 1 | A validated check failure |
| 75 | Pending work or infrastructure/evidence failure; no verdict |

After both jobs finish, invoke the same command. It collects and checks the saved
job identities and latest attempts. It does not blindly repeat them. Keep the
request when diagnosing failure; editing it or writing a pass stamp does not
repair evidence. There is no direct-host/local fallback for ordinary push.

The Windows part selects one installed Visual Studio C++ environment through
`vswhere` and that installation's `VsDevCmd`, before building or creating an
artifact identity. It records the compiler/helper hashes, toolset, SDK and effective
environment. Leave compiler and SDK overrides unset for the ordinary entrypoint;
conflicting inherited overrides are refused. The repository's Rust linker and
flags remain authoritative. Historical Windows records without this native
binding cannot qualify a new request; Linux artifact identities are unchanged.

After a compile-only Cargo command completes, the build adapter finalizes only
helpers bound to that selected compiler and the current owned job. The Windows
gate builds the selected test binaries first, then runs them from nextest metadata
without rebuilding. Mixed build/test commands have no helper-disposal authority.
Cargo failures remain separate from cleanup failures. The outer native launcher still refuses
unexpected surviving workers. On exit 75, inspect the build-finalization and
launcher membership receipts; do not ignore workers, change the settle window or
retry before the failure is understood.

Raw command results survive optional-reader failures:

- Remote: `unit-N-command-SUFFIX/{argv,cwd,stdout,stderr,exit}` under `FLEET_OUT`.
  `argv` is NUL-separated. `capture-status` records both stream-drain results;
  `capture-complete` exists only after both drains succeed.
  `unit-N-required`, `unit-N-steps` and
  `unit-N-selection.json` retain the reader's inputs.
- Client: `command-SUFFIX/{result.json,stdout,stderr}` inside the request store;
  `<part>-results.json` records Fleet's collected-output location.
- `unit-N.log` remains the combined transcript; `unit-N.json` is the validated
  unit record. Raw exit 0 alone cannot substitute for that record.

An exit code records command termination, not complete stream capture. Check
the shell's `capture-complete` marker or the client's `result.json`
`capture_complete: true` before consuming stdout. Interrupted captures can retain
an exit code. Client `stdout_available` / `stderr_available` distinguish returned
partial bytes from bytes unavailable at timeout; neither implies an empty stream.
Absent exits or null return codes do not establish a completed command. Preserve both
the command failure and any collection/cleanup failure. Diagnose the earliest
failed boundary before rerunning. A report-parser correction often needs only
the saved data, not another build or measurement.

## Bound container cases

Use the ERA producer, not a manually concealed `docker run`. In spec 7, a step's
ordinary argv is its driver; `step_needs[N].docker` declares that step's actual
`docker create` argv and container RAM. Total step RAM reserves both the container
and its driver. Fleet binds one allowed CPU, name, owner, local Docker daemon and
resolved argv before work starts. It checks effective limits before `start`.
The plan requires an immutable image and `--pull=never`: missing images fail
before workload execution. Image preparation is separate work, never an implicit
uncapped pull during a measured step.

The producer reads `FLEET_CONTAINER_REQUEST` and uses the staged agent's
`launch/start/read/close` operations. A close acknowledgement is required before
completion. Unknown create/start outcomes retain the reservation for recovery;
an absent name or an exited driver is not proof of cleanup. The agent owns the
container ID. Do not rewrite its state, call Docker directly to bypass a refusal,
or put two engines/converters into one single-container grant.

Before removing its container, the agent retains raw stdout/stderr and an
identity/operation receipt under `FLEET_OUT`, even if the driver died. Check the
closed record's `logs` reference; failed capture keeps the object and reservation
for recovery. A prelaunch RETREAT remains exit 130 with closure pending until
the controller confirms driver quiescence. It is not a successful case.

For a fresh NRESE build, the supported producer route may mount the verified
native `perf_lab` and canonical `nrese-era.sh` read-only into a pinned compatible
runtime image. Its receipt must name the binary's source separately from the
tooling source, and retain binary/script/image identity and library compatibility.
Do not claim the canonical image-build path ran. Daemon `docker build`, BuildKit
and arbitrary image imports are not implicitly authorized by this launch plan.

Use the producer's current help and retained case files for commands and options;
the case, actual container plan and limits must agree. Successful small Jena and
NRESE route checks establish operational readiness, not a competitor scoreboard.

## Timing and closure

Use the family producer and preserve its declared cases, controls and comparison
rule:

```text
python -B benches/suite/abba.py check benches/abba/FAMILY.toml
python -B benches/suite/abba.py jobs benches/abba/FAMILY.toml --checkout SOURCE --harness HARNESS_SHA --out OWNED_JOB_DIR
```

Inspect the generated jobs' actual resources and eligibility, then submit them
through Fleet. Read the report's own exit and coverage, not only scheduler status:
ABBA exit 0 means no loss shown, 1 loss shown/confirmation due, 2 invalid family,
3 incomplete/no verdict, and 4 confirmed loss. Keep unresolved rows explicit.
Cheap diagnostics choose a direction; focused guards validate a change; combined
acceptance checks integration. A new hypothesis does not automatically require
repeating a broad campaign.

After the last planned consumer, clean only owned disposable resources through
the normal execution owner. Preserve results, verified shared data, and reusable
caches. Never run a host-wide Docker prune. Record the exact cleanup and reclaimed
space. Remove the finished task's own notice registration; do not change another
owner's routes. Report delivered behavior, measured effects and remaining
decisions separately from completed jobs.
