# ERA through Fleet's bound container plan

`run.py prepare` renders a spec7 diagnostic using the existing `Spec`/Docker argv
renderer. It does not submit, pull images, build, or launch. Use a Fleet version
that implements the typed `step_needs[i].docker` contract; older installed readers
must refuse it. Keep the ordinary `run`, `images`, `check`, and `reproduce` modes
for their existing routes.

For one cached Jena ES/tds case, after committing the tooling and activating its
compatible Fleet reader/agent:

```powershell
python -B benches/breadth/era-shacl/run.py prepare `
  --job C:/path/to/era-jena.json --job-id era-tooling-jena-UNIQUE `
  --owner t3:YOUR_THREAD_ID --source-git C:/path/to/committed/OWL-RS `
  --source-rev FULL_TOOLING_COMMIT `
  --data /home/krarilotus/nrese-breadth/data/era-shacl `
  --results /home/krarilotus/nrese-breadth/results/era-tooling-jena-UNIQUE
```

Use the normal Fleet CLI, owner registration, notices, and result retrieval
described in Fleet's `skills/fleet-jobs/SKILL.md`. The defaults are one engine,
one case, one repetition, 2700s wait, Office's declared CPU pool `8-15,24-31`.
Pass `--host`/`--cpu-pool` for another verified cache location. `--engines`,
`--shapes`, and `--runs` retain the kit's interleaved order; they do not qualify a
larger campaign. These shareable diagnostic jobs are not comparative timing.

Each step has driver-only argv:
`python3 -B benches/breadth/era-shacl/run.py consume --case <JSON>`.
Its actual container command lives in
`step_needs[i].docker = {"ram_gb": 12, "argv": ["docker", "create", ...]}`,
beside `cores: 1, ram_gb: 16`. The agent confines the driver to the 4GiB remainder,
selects one allowed CPU, and freezes the Docker executable and local Unix daemon.
The container uses 12GiB, no swap, quota1, and the same singleton CPU.
Its plan includes `--pull=never`: an absent cached image fails with its raw error,
without a daemon-side download outside the declared container budget.

The consumer copies the resolved `argv` from `FLEET_CONTAINER_REQUEST` unchanged
into the agent's `container_phase/launch` request. The request's other fields are
`fleet_dir, job, step, attempt, owner`. It never rewrites that file or agent state.
The only plan placeholders are `@FLEET_CPU@`, `@FLEET_CONTAINER_NAME@`, and
`@FLEET_CONTAINER_OWNER@`; Fleet resolves them, including the `fleet.owner` label.
Every create/start/wait/stats/logs/inspect/close operation goes through the same
agent. An inspected create precedes start. A successful result requires an
explicit `ack: true, phase_closed: true` close response. Fleet also requires the
driver slice to be empty before another step can begin.

The monotonic container wall timer starts before gated create. It includes the
agent/inspect/start overhead; engine load/validation timers remain the engine's
own. Stats are actual no-stream agent observations, with a new observation after
the preceding reply plus the sampling interval. This differs from legacy
streaming stats. Wait timeout is exit124 only after acknowledged cleanup.
RETREAT yields exit130 and never replays the old binding; Fleet must establish a
new attempt after full finality. Unknown operations or failed close remain
failures, with both the primary error and cleanup errors retained.

Each attempt retains `container.log`, raw per-operation transport stdout/stderr,
`container.log.bound.json`, `invocation.json`, result rows, and the raw report.
They are copied to `$FLEET_OUT/era-attempts/<agent-owned-name>` for normal Fleet
fetch. A previous report is preserved as `previous-report.ttl` and cannot make a
new attempt succeed. Engine exits propagate; a failed close publishes no success
rows. Missing converter pins produce empty counts with an explicit unavailable
note. Supplying `--converter-image sha256:...` adds a separate bound riot step
and retains its N-Triples and count in that step's invocation. It never implies
oracle equivalence. Do not run the legacy `check` mode as a substitute: it can
start its own legacy converter.

## Fresh-source NRESE without daemon image builds

Use `--engines nrese --native-manifest <local JSON receipt>`. The receipt names
an already verified, same-host native cache and an immutable ABI-compatible
Debian runtime image:

```json
{
  "host": "office-pc",
  "source": "FULL_BINARY_SOURCE_COMMIT",
  "image": "sha256:IMMUTABLE_RUNTIME_IMAGE_ID",
  "binary": {"path": "/verified/cache/release/examples/perf_lab", "sha256": "BINARY_SHA256"},
  "identity": {"path": "/verified/cache/IDENTITY", "sha256": "IDENTITY_SHA256"}
}
```

Obtain that binary through the existing guarded native build/cache owner in an
official Fleet job. Reuse a matching cache; this producer does not compile or
copy binaries between hosts. Keep the harness source and binary source distinct.
The consumer verifies both hashes and the source/compiler/toolchain/rustflags/
profile/linker fields of `IDENTITY`, retains the identity, and rechecks the binary
hash after execution. The canonical `nrese-era.sh` is pinned by its digest and
staged under the unique results directory. Both binary and adapter are bind
mounted read-only at their usual `/usr/local/bin` paths. `RAYON_NUM_THREADS=1` is
explicit. The image's `ldd` must succeed without missing libraries before the
canonical adapter runs; its raw output is retained as `native-ldd.txt`. This is
the **native prebuilt + read-only canonical adapter** packaging route, with the
same actual perf_lab flags and input/report contract as `nrese.Dockerfile`.

`images --prebuilt` and `nrese.Dockerfile` remain another packaging path. Docker
daemon builds are unsupported by the capped Fleet route; do not hide `docker
build`, remove the Docker tool requirement, or use a dummy launch to admit one.
The native route completes fresh-source execution without a BuildKit/import
service. Its first actual smoke must retain a successful load/validation/report,
raw exit, loader output, inspected limits and closure; historical `--help`
unknown-option exit1 is not that smoke.
