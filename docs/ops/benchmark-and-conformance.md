# NRESE Benchmark and Conformance Harness

## Purpose

This document defines a reproducible, Rust-native baseline for:

- performance benchmarking against NRESE and optionally Fuseki
- compatibility checks between NRESE and Fuseki on representative SPARQL cases

The harness is intentionally isolated from production crates and lives under:

- `benches/nrese-bench-harness/`

## Scope of This Harness

- HTTP-level query/update benchmarking via `/dataset/query` and `/dataset/update`
- compatibility checks for query semantics, update effects, bounded query/update failure semantics, and graph-store semantics
- normalized response-semantics comparison for graph-store failure paths (status, content type, body class)
- dataset seeding against both NRESE and Fuseki dataset endpoints
- optional Basic-Auth against Fuseki-style secured compare stacks
- fixture-driven workloads for repeatable runs
- optional machine-readable JSON report artifacts (`--report-json`)
- real-world ontology catalog sync from official ontology sources

It is a foundation, not the final benchmark program. Extend fixtures and case sets as coverage grows.

## Production Workload Parity Pack

The canonical replacement-evidence unit is a production workload parity pack.

A parity pack is a manifest-driven bundle that identifies:

- one seed dataset
- one query workload
- one update workload
- one or more compatibility case sets

This concept exists to prevent replacement claims from being based on ad hoc command lines and drifting fixture combinations.

Ownership:

- this document owns parity-pack format, execution rules, and evidence expectations
- the harness implementation in `benches/nrese-bench-harness/` owns execution
- status/gap tracking stays in the spec documents

Current manifest format:

- file: `pack.toml`
- fields:
  - `name`
  - `dataset`
  - `query_workload`
  - `update_workload`
  - `compat_suites`
  - optional `[nrese]`
  - optional `[fuseki]`
  - optional `[invocation_profiles.nrese.<name>]`
  - optional `[invocation_profiles.fuseki.<name>]`

All paths are resolved relative to the manifest directory unless they are absolute.
For production-style live parity, transport policy is separated from workload intent.
Selected connection profiles own live NRESE/Fuseki base URLs, auth headers, timeout defaults, and reusable invalid-auth invocation profiles.
Workload packs keep ownership of dataset, workloads, compat suites, and pack-local service defaults.
Invocation precedence is: selected connection-profile defaults, then pack-local service defaults, then named invocation profiles, then shared case-level headers/timeouts.
Profile-name collisions between a selected connection profile and a workload pack are rejected instead of silently overridden.
Shared case-level headers still win on collision, so production packs can define auth/proxy defaults without forking comparator logic.
Per-case timeout budgets still override service-level and named-profile defaults when a suite needs a stricter bound than the shared target profile.
Pack execution mode is explicit: `full` runs seed + compat + bench, while `compat-only` runs only compatibility suites against the selected live targets.

Current example:

- `benches/nrese-bench-harness/fixtures/packs/generic-baseline/pack.toml`
- the generic baseline now bundles both `protocol_cases.json` and `policy_failure_cases.json`
- the generic baseline now also includes `limit_semantics_cases.json` so query-window semantics are exercised on the same pack path
- secured live-deployment templates now exist under:
  - `benches/nrese-bench-harness/fixtures/packs/secured-live-auth-template/pack.toml`
  - `benches/nrese-bench-harness/fixtures/packs/secured-live-auth-timeout-template/pack.toml`
  - `benches/nrese-bench-harness/fixtures/live/connection-profiles.template.toml`

Example workload pack:

```toml
name = "secured-pack"
dataset = "../../datasets/comparison_seed.ttl"
query_workload = "../../workloads/query_workload.json"
update_workload = "../../workloads/update_workload.json"
compat_suites = [
  "../../compat/protocol_cases.json",
  "../../compat/secured_auth_failure_cases.json",
]

[nrese]
timeout_ms = 15000

[fuseki]
timeout_ms = 15000
```

Example connection-profile registry:

```toml
[profiles.secured-live.nrese]
base_url = "${NRESE_LIVE_BASE_URL}"
timeout_ms = 15000

[profiles.secured-live.nrese.headers]
authorization = "Bearer ${NRESE_COMPARE_READ_TOKEN}"

[profiles.secured-live.fuseki]
base_url = "${FUSEKI_LIVE_BASE_URL}"
timeout_ms = 15000

[profiles.secured-live.invocation_profiles.nrese.invalid.headers]
authorization = "Bearer invalid-token"
```

Secured live-deployment template rules:

- keep real secrets out of committed pack manifests
- keep live URLs, auth headers, and timeout defaults in a selected connection profile instead of embedding them in packs
- use environment placeholders in the connection-profile registry and inject the real values locally or in CI
- prefer CLI `--fuseki-basic-auth` only when intentionally overriding the selected connection profile
- keep timeout parity in a separate pack so operators must opt in explicitly once both stacks have comparable timeout ceilings

Environment placeholders are resolved by the harness before request execution, so selected connection profiles can stay versioned while credentials and deployment-specific tokens remain external.

The secured templates intentionally reuse the existing compat suites:

- `protocol_cases.json`
- `policy_failure_cases.json`
- `secured_auth_failure_cases.json`
- `timeout_failure_cases.json` only in the timeout template

## Files

- Benchmark harness binary:
  - `benches/nrese-bench-harness/Cargo.toml`
  - `benches/nrese-bench-harness/src/main.rs`
- Benchmark workloads:
  - `benches/nrese-bench-harness/fixtures/workloads/query_workload.json`
  - `benches/nrese-bench-harness/fixtures/workloads/update_workload.json`
  - `benches/nrese-bench-harness/fixtures/workloads/ontology_query_workload.json`
- Generic comparison fixtures:
  - `benches/nrese-bench-harness/fixtures/datasets/comparison_seed.ttl`
- Compatibility cases:
  - `benches/nrese-bench-harness/fixtures/compat/protocol_cases.json`
  - `benches/nrese-bench-harness/fixtures/compat/limit_semantics_cases.json`
  - `benches/nrese-bench-harness/fixtures/compat/ontologies/baseline_cases.json`
  - `benches/nrese-bench-harness/fixtures/compat/policy_failure_cases.json`
  - `benches/nrese-bench-harness/fixtures/compat/secured_auth_failure_cases.json`
  - `benches/nrese-bench-harness/fixtures/compat/timeout_failure_cases.json`
- Workload packs:
  - `benches/nrese-bench-harness/fixtures/packs/generic-baseline/pack.toml`
  - `benches/nrese-bench-harness/fixtures/packs/secured-live-auth-template/pack.toml`
  - `benches/nrese-bench-harness/fixtures/packs/secured-live-auth-timeout-template/pack.toml`
- Live connection profiles:
  - `benches/nrese-bench-harness/fixtures/live/connection-profiles.template.toml`
- Ontology catalog:
  - `benches/nrese-bench-harness/fixtures/catalog/ontologies.toml`
- Seed dataset:
  - `benches/nrese-bench-harness/fixtures/datasets/comparison_seed.ttl`
- Optional local Fuseki stack:
  - `ops/fuseki/docker-compose.yml`

## Prerequisites

- NRESE running (example: `http://127.0.0.1:8080`)
- Optional: Fuseki running with equivalent dataset shape (example: `http://127.0.0.1:3030/ds`)
- Optional: Fuseki admin credentials if the compare stack is protected (example: `admin:nrese-admin`)
- Ontology loaded in both systems for meaningful comparisons

Note on endpoint layouts:

- NRESE uses `/dataset/query`, `/dataset/update`, and `/dataset/data`
- Fuseki dataset endpoints are dataset-relative, so a base URL like `http://127.0.0.1:3030/ds` maps to `/ds/query`, `/ds/update`, and `/ds/data`

## 0. Generic Local Baseline

If you want a neutral baseline without the project ontology, seed both systems with the generic comparison fixture first:

```powershell
cargo run --manifest-path benches/nrese-bench-harness/Cargo.toml -- seed `
  --nrese-base-url http://127.0.0.1:8080 `
  --fuseki-base-url http://127.0.0.1:3030/ds `
  --dataset benches/nrese-bench-harness/fixtures/datasets/comparison_seed.ttl `
  --replace true
```

Then use the versioned defaults already in the harness:

- `benches/nrese-bench-harness/fixtures/workloads/query_workload.json`
- `benches/nrese-bench-harness/fixtures/workloads/update_workload.json`
- `benches/nrese-bench-harness/fixtures/compat/protocol_cases.json`

## 0.1 Real-World Ontology Catalog

The harness now has a staged ontology catalog for small-to-broad real-world sources:

- `foaf`
- `org`
- `time`
- `prov`
- `skos`
- `sosa`
- `ssn`
- `dcat`
- `vcard`
- `dcterms`
- `odrl`

Each catalog entry now also declares typed processing metadata:

- `serialization`
- `semantic_dialects`
- `reasoning_features`
- `service_coverage`

Sync them locally:

```powershell
cargo run --manifest-path benches/nrese-bench-harness/Cargo.toml -- catalog-sync
```

Sync only the smallest tier first:

```powershell
cargo run --manifest-path benches/nrese-bench-harness/Cargo.toml -- catalog-sync --tier small
```

The catalog and source URLs are documented in [ontology-fixture-catalog.md](./ontology-fixture-catalog.md).

For real ontology runs, prefer:

- `benches/nrese-bench-harness/fixtures/workloads/ontology_query_workload.json`
- `benches/nrese-bench-harness/fixtures/workloads/update_workload.json`
- `benches/nrese-bench-harness/fixtures/compat/ontologies/baseline_cases.json`
- `pack-matrix` can execute all catalog-backed baseline packs for one tier and emits a top-level `pack-matrix-report.json` alongside per-pack `pack-report.json` evidence
- `pack-matrix` filters operate on the typed ontology catalog metadata, so the same catalog now acts as both fixture inventory and execution selector for targeted evidence runs
- `pack-matrix` can also target one exact ontology name on the same selector path when a live parity run needs to stay narrowly scoped
- `pack-matrix` also validates each catalog-backed baseline pack before execution so pack identity, dataset path, and required compat suites stay aligned with the ontology catalog

Prebuilt ontology packs now exist for:

- `foaf`
- `org`
- `time`
- `prov`
- `skos`
- `sosa`
- `ssn`
- `dcat`
- `vcard`
- `dcterms`
- `odrl`

Each checked-in ontology baseline pack is expected to include:

- the shared `baseline_cases.json` suite
- the ontology-specific `<ontology>_cases.json` suite
- the shared `rdf_xml_cases.json` suite when the catalog serialization is RDF/XML

That requirement is validated by the harness for catalog-driven pack execution.

## 0. Reproducible Dataset Parity

Start Fuseki if you want a local side-by-side comparison stack:

```powershell
docker compose -f ops/fuseki/docker-compose.yml up -d
```

Then seed the same dataset into NRESE and optionally Fuseki:

```powershell
cargo run --manifest-path benches/nrese-bench-harness/Cargo.toml -- seed `
  --nrese-base-url http://127.0.0.1:8080 `
  --fuseki-base-url http://127.0.0.1:3030/ds `
  --fuseki-basic-auth admin:nrese-admin `
  --dataset benches/nrese-bench-harness/fixtures/datasets/comparison_seed.ttl `
  --replace true
```

This gives both services the same default-graph baseline before you run compatibility or latency checks.

## 1. Performance Run (NRESE only)

```powershell
cargo run --manifest-path benches/nrese-bench-harness/Cargo.toml -- bench `
  --nrese-base-url http://127.0.0.1:8080 `
  --iterations 20 `
  --query-workload benches/nrese-bench-harness/fixtures/workloads/query_workload.json `
  --update-workload benches/nrese-bench-harness/fixtures/workloads/update_workload.json `
  --report-json artifacts/bench-report.json
```

What it reports:

- sample count
- success/failure count
- min/p50/p95/p99/max latency
- total elapsed milliseconds

## 2. Comparative Performance Run (NRESE vs Fuseki)

```powershell
cargo run --manifest-path benches/nrese-bench-harness/Cargo.toml -- bench `
  --nrese-base-url http://127.0.0.1:8080 `
  --fuseki-base-url http://127.0.0.1:3030/ds `
  --fuseki-basic-auth admin:nrese-admin `
  --iterations 20 `
  --query-workload benches/nrese-bench-harness/fixtures/workloads/query_workload.json `
  --update-workload benches/nrese-bench-harness/fixtures/workloads/update_workload.json
```

Additional output:

- p95 delta for query/update (`NRESE - Fuseki`)
- optional JSON report for CI ingestion (same `--report-json` flag)

## 3. Compatibility Run (NRESE vs Fuseki)

```powershell
cargo run --manifest-path benches/nrese-bench-harness/Cargo.toml -- compat `
  --nrese-base-url http://127.0.0.1:8080 `
  --fuseki-base-url http://127.0.0.1:3030/ds `
  --fuseki-basic-auth admin:nrese-admin `
  --cases benches/nrese-bench-harness/fixtures/compat/protocol_cases.json `
  --report-json artifacts/protocol-compat-report.json
```

Current compatibility comparators:

- `ask-boolean`: compares ASK boolean result
- `solutions-count`: compares row count of SPARQL JSON bindings
- `construct-triples-set`: compares canonicalized N-Triples line sets for CONSTRUCT output
- `graph-triples-set`: compares canonicalized N-Triples line sets for Graph Store reads
- `status-content-type-body-class`: compares normalized failure/output semantics by HTTP status, normalized content type, and coarse body class

Timeout observations stay on the same shared response-semantics comparator path.
When the harness hits a client-side request timeout, it emits normalized semantics as:

- `status = 0`
- `content_type = null`
- `body_class = client-timeout`

This keeps timeout parity in the same report shape and avoids endpoint-specific timeout comparators.

Current protocol operations covered by the compat harness:

- `query`
- `update-effect`
- `graph-read`
- `graph-head`
- `graph-delete-effect`
- `graph-put-effect`
- `graph-post-effect`

Graph payload comparators canonicalize supported RDF syntaxes onto one triples-set path:

- `text/turtle`
- `application/n-triples`
- `application/rdf+xml`

Fixture authoring rules for compatibility cases:

- `request_headers` may be set per case and are applied through one shared request-normalization path for query, update, and graph-store operations
- per-case headers override the harness defaults for the same header name, so edge cases such as alternate `Accept` or proxy-forwarded headers do not require endpoint-specific comparator logic
- status-only `update-effect` cases do not require `verify_query`; verification queries remain mandatory only when the case compares post-update dataset state

If mismatches exist, the harness exits non-zero and prints case names.
When `--report-json` is set, per-case comparator summaries are emitted for machine parsing.
The current fixture set now includes graph-store write failure parity cases for unsupported media types, malformed Turtle payloads, and missing-graph lifecycle probes.
It also includes bounded query/update failure-parity cases for invalid SPARQL syntax, compared by normalized status, content type, and body class.
It also now includes a broader SPARQL Update parity slice for `DELETE DATA`, `DELETE/INSERT WHERE`, `CLEAR`, `COPY`, `MOVE`, and `ADD` over isolated fixture IRIs/graphs.
It also now supports a separate policy-failure fixture family for invalid-auth and oversize-payload parity on the same shared response-semantics comparator path.
It now also supports a dedicated timeout-failure fixture family on the same shared response-semantics comparator path, using per-case timeout budgets instead of a separate timeout-only report format.
Secured live-deployment packs can now bind invalid-auth cases through named per-side invocation profiles instead of duplicating live auth headers inside compat JSON.
RDF/XML protocol cases can now stay on the same graph/query comparator path as Turtle and N-Triples instead of needing a response-semantics-only fallback.

Policy-failure fixtures are intentionally separate from the generic protocol baseline, because they only become meaningful when both stacks are run with comparable auth and payload-limit policy.
Timeout-failure fixtures are also intentionally separate from the generic baseline, because meaningful timeout parity depends on comparable timeout ceilings, reverse-proxy behavior, and workload-specific slow paths.

Current timeout fixture starter:

- `benches/nrese-bench-harness/fixtures/compat/timeout_failure_cases.json`

Use it as an opt-in suite in a production workload parity pack once both NRESE and Fuseki are deployed with comparable timeout policy and the selected cases are known to cross the configured timeout budget.

## 3.1 Workload Pack Preflight

Use `pack-validate` before a real secured live run when you want the harness to resolve the selected connection profile, merge pack-local invocation profiles, and fail early on missing profile references without seeding or benchmarking.

```powershell
cargo run --manifest-path benches/nrese-bench-harness/Cargo.toml -- pack-validate `
  --connection-profiles benches/nrese-bench-harness/fixtures/live/connection-profiles.template.toml `
  --connection-profile secured-live `
  --workload-pack benches/nrese-bench-harness/fixtures/packs/secured-live-auth-template/pack.toml `
  --report-json artifacts/secured-live-auth-validation.json
```

When `--report-json` is set, the harness emits a machine-readable validation report with:

- selected connection-profile registry path and profile name
- resolved NRESE/Fuseki base URLs
- compat suites
- merged NRESE/Fuseki invocation-profile names

## 4. Workload Pack Run

This is the preferred production-style execution path when you want one coherent evidence run instead of manually chaining `seed`, `compat`, and `bench`.

```powershell
cargo run --manifest-path benches/nrese-bench-harness/Cargo.toml -- pack `
  --nrese-base-url http://127.0.0.1:8080 `
  --fuseki-base-url http://127.0.0.1:3030/ds `
  --fuseki-basic-auth admin:nrese-admin `
  --workload-pack benches/nrese-bench-harness/fixtures/packs/generic-baseline/pack.toml `
  --iterations 20 `
  --report-dir artifacts/generic-baseline
```

Execution order:

1. seed dataset into NRESE and optional Fuseki
2. run all configured compatibility suites if Fuseki is configured
3. run benchmark workloads

Expected evidence artifacts when `--report-dir` is set:

- `pack-report.json`
- `bench-report.json`
- one `*-report.json` artifact per configured compat suite when Fuseki is configured

`pack-report.json` is the canonical index for one workload-pack run. It ties together:

- manifest path
- selected connection-profile registry path and profile name when used
- dataset path
- configured compat suites
- per-suite match status and report path
- benchmark report path
- overall pack `status` and `error`, so failed live parity runs still emit one top-level evidence index instead of leaving only partial suite files

If a pack includes `timeout_failure_cases.json`, the resulting suite artifact is indexed the same way as any other compat suite. Timeout parity does not get a separate report type.
If a pack references named invocation profiles through its compat suites, the harness validates those references before seeding or benchmarking so a selected connection profile and a workload pack cannot drift silently.

For graph-producing cases, the harness now canonicalizes blank nodes before triples-set comparison instead of comparing raw blank-node labels. This keeps official ontology `CONSTRUCT` parity stable across NRESE and Fuseki when both outputs are graph-isomorphic but use different blank-node identifiers.

For existing secured deployments where reseeding or benchmarking is not appropriate, reuse the same pack with `--execution-mode compat-only`:

```powershell
cargo run --manifest-path benches/nrese-bench-harness/Cargo.toml -- pack `
  --connection-profiles benches/nrese-bench-harness/fixtures/live/connection-profiles.template.toml `
  --connection-profile secured-live `
  --execution-mode compat-only `
  --workload-pack benches/nrese-bench-harness/fixtures/packs/secured-live-auth-template/pack.toml `
  --report-dir artifacts/secured-live-auth-compat-only
```

Secured live-auth example:

```powershell
cargo run --manifest-path benches/nrese-bench-harness/Cargo.toml -- pack `
  --connection-profiles benches/nrese-bench-harness/fixtures/live/connection-profiles.template.toml `
  --connection-profile secured-live `
  --workload-pack benches/nrese-bench-harness/fixtures/packs/secured-live-auth-template/pack.toml `
  --iterations 20 `
  --report-dir artifacts/secured-live-auth
```

Secured live-auth plus timeout example:

```powershell
cargo run --manifest-path benches/nrese-bench-harness/Cargo.toml -- pack `
  --connection-profiles benches/nrese-bench-harness/fixtures/live/connection-profiles.template.toml `
  --connection-profile secured-live-timeout `
  --workload-pack benches/nrese-bench-harness/fixtures/packs/secured-live-auth-timeout-template/pack.toml `
  --iterations 20 `
  --report-dir artifacts/secured-live-auth-timeout
```

Before using either template:

- create a local copy of `benches/nrese-bench-harness/fixtures/live/connection-profiles.template.toml` or use it directly in CI
- export `NRESE_LIVE_BASE_URL`, `FUSEKI_LIVE_BASE_URL`, `NRESE_COMPARE_READ_TOKEN`, and `FUSEKI_COMPARE_READ_TOKEN` in the shell or CI environment
- uncomment or override the `basic_auth` block only when Fuseki really uses Basic Auth, or supply `--fuseki-basic-auth` to override the selected connection profile on the CLI
- use `--nrese-base-url` or `--fuseki-base-url` only when you intentionally want to override the selected connection profile at invocation time
- keep `policy_failure_cases.json` in the pack so oversize-payload parity stays on the same shared comparator path
- keep `secured_auth_failure_cases.json` in secured packs so invalid-auth parity uses the same per-side invocation-profile model as the live auth defaults
- only use the timeout template after aligning timeout ceilings and proxy behavior on both deployments

Catalog-driven live parity example:

```powershell
cargo run --manifest-path benches/nrese-bench-harness/Cargo.toml -- pack-matrix `
  --connection-profiles benches/nrese-bench-harness/fixtures/live/connection-profiles.template.toml `
  --connection-profile secured-live `
  --tier medium `
  --service-coverage graph-store `
  --report-dir artifacts/live-pack-matrix
```

Single-ontology live parity example:

```powershell
cargo run --manifest-path benches/nrese-bench-harness/Cargo.toml -- pack-matrix `
  --connection-profiles benches/nrese-bench-harness/fixtures/live/connection-profiles.template.toml `
  --connection-profile secured-live `
  --ontology skos `
  --report-dir artifacts/live-skos-pack-matrix
```

## Current Verified Local Baseline

The current workspace has already been exercised locally with:

- `cargo test --manifest-path benches/nrese-bench-harness/Cargo.toml -q`
- parity seeding into `http://127.0.0.1:8080` and `http://127.0.0.1:3030/cmp`
- `compat` against `benches/nrese-bench-harness/fixtures/compat/protocol_cases.json`
- `bench` against `benches/nrese-bench-harness/fixtures/workloads/query_workload.json` and `benches/nrese-bench-harness/fixtures/workloads/update_workload.json`

Example local evidence paths from a successful run:

- `artifacts/protocol-compat-report.json`
- `artifacts/bench-report.json`

These are local run artifacts, not replacement-grade proof by themselves. Full replacement evidence still requires the real ontology/workloads, richer resource measurements, and automated CI gating.

Additional verified local side-by-side evidence now exists against a locally unpacked Apache Jena Fuseki 6.0.0 installation outside the git repo:

- FOAF:
  - `artifacts/manual-live-parity-foaf/pack-matrix-report.json`
  - `artifacts/manual-live-parity-foaf/foaf/pack-report.json`
- ORG:
  - `artifacts/manual-live-parity-org-4/pack-matrix-report.json`
  - `artifacts/manual-live-parity-org-4/org/pack-report.json`
- SKOS:
  - `artifacts/manual-live-parity-skos/pack-matrix-report.json`
  - `artifacts/manual-live-parity-skos/skos/pack-report.json`

Those local runs were executed with:

- NRESE in `rules-mvp` mode with preset `bounded-owl`
- `NRESE_SPARQL_PARSE_ERROR_PROFILE=fuseki-plain-text`
- external Fuseki started from `../Apache_Fuseki/apache-jena-fuseki-6.0.0`

Observed local benchmark trend in those artifact sets:

- NRESE query p95 was lower than Fuseki on FOAF, ORG, and SKOS in these in-memory local runs
- NRESE update p95 was higher than Fuseki on the same runs

These numbers are informative, not release gates. Replacement-grade evidence still requires the secured live workload packs against the project-specific deployment.

## Reproducibility Rules

- Keep fixture files in version control.
- Run with known ontology snapshot for both NRESE and Fuseki.
- Record:
  - git commit hash
  - hardware profile
  - workload fixture version
  - command line flags
- Do not compare runs with different ontology versions.

## Next Extension Steps

- add write-heavy and mixed read/write workload phases
- extend the current compatibility suites from the existing timeout and bounded error-semantic baseline to broader timeout, limit, and deployment-specific failure equivalence
- add content-negotiation and media-type strictness comparators
- add orchestrated external process startup for controlled Fuseki benchmark runs
- extend the current timeout fixture family from client-observed timeout parity to broader timeout and limit coverage with deployment-specific budgets and proxy-aware cases
- add cold/warm split runs and resource-capture integration for CPU/RAM evidence
- add authenticated service startup orchestration so the harness can provision its own isolated compare stack end-to-end
- add project-specific parity packs for the real ontology, auth model, and workload mix
- add preflight report gating in CI before authenticated live parity runs are allowed to publish benchmark/compat evidence
- add ontology-specific workload packs on top of the staged real-world ontology catalog

## Isolated Side-by-Side Stack

If local ports `8080` or `3030` are already in use, run an isolated compare stack:

```powershell
$env:FUSEKI_PORT = "3031"
$env:FUSEKI_DATASET = "/ds"
docker compose -f ops/fuseki/docker-compose.yml up -d
```

Run NRESE on a separate bind address:

```powershell
$env:NRESE_BIND_ADDR = "127.0.0.1:18080"
$env:NRESE_STORE_MODE = "in-memory"
$env:NRESE_REASONING_MODE = "rules-mvp"
cargo run -p nrese-server
```

Then use:

- NRESE: `http://127.0.0.1:18080`
- Fuseki: `http://127.0.0.1:3031/ds`

## Local External Fuseki Helper

If you keep a local Apache Fuseki install one directory above the repo at `../Apache_Fuseki/apache-jena-fuseki-6.0.0`, you can use:

- `ops/fuseki/run-local-pack-matrix.ps1`

Example:

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -File .\ops\fuseki\run-local-pack-matrix.ps1 `
  -Ontology skos `
  -ExecutionMode full `
  -ReportDir artifacts\local-fuseki-skos
```

The helper keeps the external Fuseki process outside the git repo, starts NRESE on an isolated local port, and writes logs plus harness reports into the chosen artifact directory.
