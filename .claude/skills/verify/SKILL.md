---
name: verify
description: Prove nrese-server's HTTP-level behaviour by driving a real server through a scenario file (launch, doctor, drive, evidence, cleanup) and cite the evidence directory. Use before claiming that something visible over HTTP works or is fixed - repository lifecycle, login and auth ordering, output streaming and limits, the Graph Store Protocol, RDF4J sessions and transactions, NRESE-Completeness and other headers - instead of reasoning from the code. Never for timing.
---

# /verify

A claim about what the server answers over HTTP is evidence only when a real server answered
it. `scripts/verify/verify.py` starts one from a given build on a fresh data directory and a
free port, checks it is well (read-only), drives the HTTP steps of a scenario file with their
expected statuses, headers and bodies, keeps every request and response with the server's log,
and stops the server and removes its data, also when a step fails. Exit 0 only when every step
passed; the summary's path is the last line it prints.

The scenario schema, the predicates and the evidence layout: `scripts/verify/README.md`.

## Where it runs

- **On gamergrill, through the fleet** (the default): a build job, then a run job, both written
  by `verify.py jobs`. Read the fleet-jobs skill first: your own `FLEET_OWNER` and webhook, a
  priority with its reason, stale jobs cancelled.
- **Never on phuoc-yu**, and never to time anything: timings are recorded, never judged.
- **On the main PC** only for a quick run, when `~/owlrs-guard/BUSY` is absent and the current
  build rules let you build the server here (`--server target/debug/nrese-server.exe`).
- Never build or run on a fleet host except through a fleet job.

## Run it through the fleet

1. **The scenario.** Use one in `scripts/verify/scenarios/` or write one (below); check it with
   `python scripts/verify/verify.py check FILE`. Commit it: a fleet job sees git only. The
   commit holding the runner and the scenarios is the *harness*; it may differ from the server's.
2. **The jobs.**

       python scripts/verify/verify.py jobs --rev <server commit> --harness <harness commit> \
           --reason "<what this run decides>" [--priority 790 --verifies <fix commit>] SCENARIO...

   It writes `tmp/verify/jobs/verify-build-<sha8>-<stamp>.toml` and `verify-run-…toml` and
   prints their paths. Priority 600 by default (exploratory); 790 with `--verifies` for a v1.5
   fix verification. `--set KEY=VALUE` passes a server setting to every scenario. The run job
   asks for 0.5 GB (the example scenarios peaked at 55 MiB): declare more with `--ram-gb` when
   your scenarios hold more data; every summary records the peak.
3. **Submit both** (`fleet submit BUILD RUN`). The build is a debug `nrese-server` at the
   server commit, made on gamergrill (fleet can't stage a binary onto a WSL host) in the host's
   one cache, shared with the gates (`~/fleet/cache/owlrs-gate/`), kept there in `bin-<sha12>/`
   and returned with `build.json`. A later build of the
   same commit only checks the binary's checksum. The run job comes after it: the self-test,
   then your scenarios.
4. **On the notice**, `fleet results <run id> --to DIR`, then read
   `DIR/<run id>/out/selftest/summary.md` (it must say the self-test passed: otherwise the
   runner's checks aren't to be trusted) and `DIR/<run id>/out/evidence/summary.md`.

## Write a scenario

- One behaviour per scenario, its `description` naming where it is specified (docs/ops/http-api.md,
  a design note, an issue). Start from the state you need (data written by earlier steps), not
  from what the server happens to hold.
- Every step states the status it expects; assert the thing the claim is about (the header's
  value, the rows, the problem document's `type`), not merely that something answered.
- Use `capture` for what the server makes (a session's path, a token, a `Location`), and a
  `restart = true` step for what must survive a restart.
- Expect what the documentation promises. When the server answers otherwise, that's a finding:
  report it with the evidence; don't loosen the expectation until it passes.
- A scenario you add for a fix proves it fails without the fix (run it against the base build)
  and passes with it.

## Cite it

A claim takes the run job's id, its outcome (`fleet status ID`: done, or failed with the step),
the evidence directory, and the summary's lines for the steps it rests on, e.g. "`repository-
lifecycle` step 11 (after the restart: its settings) passed, job verify-run-…, evidence
…/out/evidence/repository-lifecycle/steps/11-….json". What wasn't run is "not run".

## Afterwards

The runner removes each server's directory itself. The host's cache is swept after every build by
the rule the gates share (Harness/notes/2026-10-20-host-cache-contract.md): binaries unused for 7
days, target directories and Python environments for 14, then the least recently used until it
fits its budget; exact paths only. Cancel your jobs a newer run supersedes (`fleet list --mine`).
