# NRESE

NRESE is a Rust RDF database under active redesign. The goal is a store that **reads like QLever and governs data like GraphDB**: its own storage engine, materialised RDFS/OWL 2 RL reasoning, SHACL validation on commit, full-text search and the RDF4J protocol. It is built to serve ResearchSpace and the datamodel workflow.

- **Architecture and ownership rules:** [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md)
- **Plan:** [docs/ROADMAP.md](docs/ROADMAP.md)
- **Status against QLever/GraphDB:** [docs/spec/06-target-capability-matrix.md](docs/spec/06-target-capability-matrix.md)
- **Decisions:** [docs/adr/](docs/adr/)
- **Document index (German):** [Spezifikation.md](Spezifikation.md)

## Current state (engine v2 storage, reasoner v2)

The server runs on **NRESE's own storage engine** (`nrese-engine`, [ADR-0002](docs/adr/0002-engine-storage-lsm-permutations.md)) with SPARQL 1.1 from `nrese-sparql`: a native executor (merge, hash and worst-case-optimal joins, cost-based join order, `explain=true`) for the queries it covers, spareval for the rest. It offers:
- SPARQL query and update, the Graph Store Protocol, and `TELL` ingest
- materialised reasoning (`rdfs`, `owl2-rl`): inferences are queryable and maintained on every commit, and consistency violations reject the commit. What each mode computes, and what it omits, is in [docs/spec/reasoning-semantics.md](docs/spec/reasoning-semantics.md)
- per-request read models: asserted only (`infer=false`), inferred only, or both
- a result cache keyed on the query and the revision
- auth modes and deployment postures
- an operator UI (`/ops`) and a user console (`/console`)
- a comparison harness

Measured over HTTP at 10 M triples (details in [benches/baselines/](benches/baselines/README.md)):

| Metric | In memory | On disk (fsync per commit) | v1, for comparison |
|---|---|---|---|
| One-triple insert, p50 | 0.2 ms | 2.0 ms | 3 s at 1 M |
| HTTP load throughput | ~430 k triples/s | ~400 k triples/s | — |

Durable storage needs no native toolchain. The revision is persistent, and recovery after a crash restores the last acknowledged commit.

Known limits, addressed by later milestones:
- **Memory:** indexes are bit-packed but held in memory (LUBM(100), 13.4 M asserted and 8.7 M inferred facts: 2.5 GB peak); memory-mapped runs for data larger than RAM are Pf2 step 2.
- **Default graph:** a query without `GRAPH` reads only the default graph unless `store.default_graph = "union"` is set ([config-reference](docs/ops/config-reference.md)); the speed of that mode hasn't been measured yet.
- **Reasoning:** the `rdfs` mode is a 6-rule subset; OWL 2 RL omits `eq-ref` and the datatype rules; named graphs aren't reasoning boundaries. See [reasoning-semantics.md](docs/spec/reasoning-semantics.md).
- **SHACL:** Core validation on request (`/dataset/shacl`, W3C Core suite 98 of 98); validation on commit is a later slice ([docs/design/shacl.md](docs/design/shacl.md)).
- **ResearchSpace and the Datamodel Workflow** run against it through the standard protocols ([docs/integration/](docs/integration/)); ResearchSpace's keyword search waits for full-text search.
- **Not yet available:** full-text and geospatial search, the RDF4J protocol, clustering. Order and status: [docs/plan/2026-09-30-graphdb-parity-plan.md](docs/plan/2026-09-30-graphdb-parity-plan.md).
- **Behaviour changes from v1:** literal lexical forms are kept exactly as written, and a named graph exists only while it holds quads. See ADR-0002.

Already fixed on the way (Milestone 0):
- A write that reports a timeout is guaranteed not to be committed.
- Unknown config values and unknown config-file keys fail at startup instead of silently changing behaviour; `nrese-server check-config` validates a configuration and prints the effective settings.
- Ontology preload happens only when `NRESE_ONTOLOGY_PATH` is set.

## Repository layout

| Path | Layer | Owns |
|---|---|---|
| `crates/nrese-core` | L0 | shared report and capability contracts |
| `crates/nrese-engine` | L1 | storage engine: dictionary, packed permutation indexes, MVCC snapshots, WAL and checkpoints, statistics |
| `crates/nrese-exec` | L1 | execution core: id tables, joins, grouping, closures, memory budgets; shared by SPARQL and reasoning |
| `crates/nrese-sparql` | L2 | SPARQL 1.1: native executor and planner, spareval fallback, updates, result writers |
| `crates/nrese-reasoner` | L2 | reasoner v2: rule IR, rulesets, batch and delta executors, modules; reasoning profiles |
| `crates/nrese-shacl` | L2 | SHACL Core: shapes compiler, validator, validation report |
| `crates/nrese-store` | L3 | operations (query, update, graph store, tell, backup) and the mutation pipeline |
| `crates/nrese-server` | L4 | HTTP transport, auth, policy, posture, UI hosting |
| `apps/nrese-console` | L5 | React/TypeScript console and CLI |
| `benches/nrese-bench-harness` | tooling | black-box comparison and benchmark harness |
| `docs/` | — | architecture, ADRs, roadmap, specs, ops runbooks |

## Quick start with Docker

```bash
docker build -t nrese .
docker run -p 8080:8080 -v nrese-data:/var/lib/nrese/data nrese
```

The server is then at `http://localhost:8080`: the console at `/console`, SPARQL at `/dataset/sparql`, the Graph Store at `/dataset/data` ([HTTP interface](docs/ops/http-api.md)). Data is kept in the `nrese-data` volume. Settings are environment variables, for example `-e NRESE_DEFAULT_GRAPH=union -e NRESE_REASONING_MODE=owl2-rl` ([configuration](docs/ops/config-reference.md)). The image has no authentication switched on; set `NRESE_AUTH_MODE` before exposing it.

Using it with other systems:
- [the Datamodel Workflow's exports](docs/integration/datamodel-workflow.md)
- [ResearchSpace](docs/integration/researchspace.md), with a compose file that starts both

## Setup

### Prerequisites

- Rust toolchain
- Cargo
- optional: Docker, if you want to run a local Fuseki comparison stack

### Build

```powershell
cargo build
```

### Build The User Frontend

The server embeds the console it finds in `apps/nrese-console/dist` when it is compiled, so build the console first if you want `/console` (the API works without it).

```powershell
Set-Location .\apps\nrese-console
npm install
npm run build
Set-Location ..\..
```

### Run The User Frontend In Dev Mode

Start the Rust server first so the frontend dev proxy has a backend to forward API calls to:

```powershell
cargo run -p nrese-server
```

Then, in a second terminal:

```powershell
Set-Location .\apps\nrese-console
npm install
npm run dev
```

Notes:

- open the Vite app at `http://127.0.0.1:5173/console/`
- frontend requests to `/dataset/*`, `/ops/*`, and `/api/*` are proxied to `http://127.0.0.1:8080`
- if your backend runs elsewhere, set `VITE_API_PROXY_TARGET`, for example:

```powershell
$env:VITE_API_PROXY_TARGET = "http://127.0.0.1:9090"
npm run dev
```

### Run The Frontend Against A Separate Backend

For a built frontend, prefer runtime configuration over hardcoding API URLs into components:

```js
window.__NRESE_CONSOLE_CONFIG__ = {
  apiBaseUrl: "https://nrese.example.com",
};
```

This lives in:

- `apps/nrese-console/public/console-config.js`

You can also bind at build time with:

- `VITE_NRESE_API_BASE_URL`
- `VITE_CONSOLE_BASE_PATH`

### Run The Server

```powershell
cargo run -p nrese-server
```

Run with an explicit config file:

```powershell
cargo run -p nrese-server -- --config .\config.toml
```

### Frontend Routes

- `/console`
  User-facing console for query, tell, update, graph-store, and AI-assisted query suggestions.
- `/ops`
  Operator-facing console and diagnostics surface.
- `/`
  Redirects to `/console`.

### Frontend CLI

The frontend package also ships a small CLI on top of the same TypeScript client boundary:

```powershell
Set-Location .\apps\nrese-console
npm install
npm run cli -- runtime
npm run cli -- capabilities
npm run cli -- query --text "SELECT * WHERE { ?s ?p ?o } LIMIT 5"
```

Useful options:

- `--base-url <url>` or `NRESE_API_BASE_URL`
- `--token <token>` or `NRESE_API_TOKEN`
- repeated `--header name:value`
- `--file <path>` for query/update/tell/graph payloads
- `--graph default|named` and `--graph-iri <iri>` for graph operations

Optional environment variables:

- `NRESE_ONTOLOGY_PATH`
  Path to an ontology file to preload.
- `NRESE_REASONING_MODE`
  `disabled`, `rdfs` or `owl2-rl`
- `NRESE_DEPLOYMENT_POSTURE`
  Example: `read-only-demo`, `internal-authenticated`, or `replacement-grade`
- `NRESE_SPARQL_PARSE_ERROR_PROFILE`
  Example: `problem-json` or `fuseki-plain-text`
- `NRESE_STORE_MODE`
  Example: `in-memory`
- `NRESE_BIND_ADDR`
  Example: `127.0.0.1:8080`
- `NRESE_AI_ENABLED`
  Example: `true`
- `NRESE_AI_PROVIDER`
  Example: `gemini`
- `GOOGLE_API_KEY`
  Used as Gemini API key fallback if `NRESE_AI_GOOGLE_API_KEY` is not set.

The canonical runtime configuration reference is [docs/ops/config-reference.md](docs/ops/config-reference.md). README only lists the common entry points.

### Run Local Side-By-Side Parity Against A Local Fuseki Install

If you have Apache Fuseki unpacked one directory above the repository at `../Apache_Fuseki/apache-jena-fuseki-6.0.0`, you can run the local compare helper:

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -File .\ops\fuseki\run-local-pack-matrix.ps1 `
  -Ontology foaf `
  -ExecutionMode full `
  -ReportDir artifacts\local-fuseki-foaf
```

The helper:

- starts `nrese-server` on an isolated local port
- starts the external Fuseki install outside the git repo
- applies `NRESE_SPARQL_PARSE_ERROR_PROFILE=fuseki-plain-text` for syntax-error parity
- runs the harness `pack-matrix` command
- writes logs and reports under the chosen artifact directory

### Run With Durable Storage

```powershell
$env:NRESE_STORE_MODE = "on-disk"
$env:NRESE_DATA_DIR = "./data"
cargo run -p nrese-server
```

On-disk mode is always available. It writes a WAL plus checkpoints under `NRESE_DATA_DIR`, locks the directory against a second process, and recovers the last committed revision on start.

## Development

### Building without filling the disk

Cargo never removes build output it no longer uses, so an unattended `target/` grows without bound (it reached 145 GB here). Two things keep it small:

- `.cargo/config.toml`: no debug information for dependencies, line tables only for our crates, no incremental cache, and one build directory for the workspace and the benchmark harness. A full build with all tests is about 6 GB.
- `scripts/cargo-guarded.sh`, used in place of `cargo`:
  - over the build directory's budget (25 GB, `NRESE_TARGET_BUDGET_GB`) it removes the build output before building;
  - below the free-space floor (20 GB, `NRESE_MIN_FREE_GB`) it doesn't build;
  - it uses half the cores and low priority, so the machine stays usable.

```bash
scripts/cargo-guarded.sh test --locked --workspace
scripts/cargo-guarded.sh --status   # sizes and limits
scripts/cargo-guarded.sh --clean    # empty the build directory
```

The benchmark scripts check free space the same way before they create datasets or containers, and clean up after themselves when they exit (`scripts/bench-cleanup.sh`; see [benches/reasoning/README.md](benches/reasoning/README.md)).

### Where To Start

If you want to work on shared contracts:

- start in [crates/nrese-core/src/lib.rs](crates/nrese-core/src/lib.rs)

If you want to work on storage, dataset state, or SPARQL execution:

- start in [crates/nrese-store/src/lib.rs](crates/nrese-store/src/lib.rs)
- then look at the store service, staging, query, update, and graph-store modules

If you want to work on reasoning:

- start in [docs/design/reasoner-v2.md](docs/design/reasoner-v2.md) and [crates/nrese-reasoner/src/v2/mod.rs](crates/nrese-reasoner/src/v2/mod.rs)
- rules are data ([ir.rs](crates/nrese-reasoner/src/v2/ir.rs), [rulesets.rs](crates/nrese-reasoner/src/v2/rulesets.rs)); list axioms are compiled per axiom ([lists.rs](crates/nrese-reasoner/src/v2/lists.rs))
- [naive.rs](crates/nrese-reasoner/src/v2/naive.rs) is the reference evaluator every executor is tested against
- [batch.rs](crates/nrese-reasoner/src/v2/batch.rs) materialises (grounding, parallel semi-naive evaluation, transitive and equality modules); [delta.rs](crates/nrese-reasoner/src/v2/delta.rs) maintains the closure per commit (DRed with B/F proofs)
- the store side (materialisation, commit path, reject explanations) is [crates/nrese-store/src/reasoning.rs](crates/nrese-store/src/reasoning.rs)

If you want to work on HTTP, auth, or operator surfaces:

- start in [crates/nrese-server/src/lib.rs](crates/nrese-server/src/lib.rs)
- routing and handlers are under `crates/nrese-server/src/http/`
- environment-variable names and config parsing entry points are centralized under `crates/nrese-server/src/config/`
- AI provider integrations are under `crates/nrese-server/src/ai/`

If you want to work on the user frontend:

- start in `apps/nrese-console/src/App.tsx`
- the frontend/backend contract is documented in [docs/dev/frontend-backend-contract.md](docs/dev/frontend-backend-contract.md)
- API calls and frontend transport helpers are under `apps/nrese-console/src/lib/`
- endpoint ownership is centralized in `apps/nrese-console/src/lib/endpoints.ts`
- the shared frontend TypeScript client is in `apps/nrese-console/src/lib/client.ts`
- browser runtime config is in `apps/nrese-console/src/lib/runtimeConfig.ts`
- CLI entry points are in `apps/nrese-console/src/cli/`
- UI components are under `apps/nrese-console/src/components/`
- language strings are under `apps/nrese-console/src/i18n/`
- styling tokens and layout files are under `apps/nrese-console/src/styles/`
- extension guidance is in [docs/dev/frontend-extension-guide.md](docs/dev/frontend-extension-guide.md)

If you want to work on benchmarks or compatibility checks:

- start in [benches/nrese-bench-harness/src/main.rs](benches/nrese-bench-harness/src/main.rs)
- keep per-case request customization in the shared compat request path instead of adding endpoint-specific compare logic
- workflow details are in [docs/ops/benchmark-and-conformance.md](docs/ops/benchmark-and-conformance.md)
- manifest-driven production-style harness runs are also defined there; do not duplicate pack format rules elsewhere
- real-world ontology catalog guidance is in [docs/ops/ontology-fixture-catalog.md](docs/ops/ontology-fixture-catalog.md)

### Project Rules

- keep concerns separated by crate and module
- avoid duplicating the same runtime rule or policy in multiple places
- keep typed runtime config in the owning crate and external parsing in `crates/nrese-server/src/config/`
- add or update tests when runtime behavior changes
- keep docs and spec files in sync with implementation changes
- prefer small modules over long mixed-responsibility files

Code structure guidance:

- the repository-wide structure and reactor rules are defined in [docs/dev/code-structure-guidelines.md](docs/dev/code-structure-guidelines.md)

Configuration guidance:

- implemented server/runtime knobs and ownership rules are documented in [docs/ops/config-reference.md](docs/ops/config-reference.md)
- env-var names used by the server are centralized in `crates/nrese-server/src/config/env_names.rs`

Backup/restore source of truth:

- operational backup/restore steps and drill evidence requirements are defined only in [docs/ops/backup-restore-drills.md](docs/ops/backup-restore-drills.md)

### Test Layout

The repository uses two styles of tests:

- integration-style crate tests under `crates/*/tests`
- module-adjacent unit tests in dedicated test files for internal behavior, for example under `crates/nrese-reasoner/src/tests`

The goal is:

- runtime code stays focused on runtime behavior
- tests stay close to the concern they validate
- private module behavior can still be tested without bloating production files
- shared minimal RDF fixtures live under `fixtures/ontologies/` and are used to exercise service-level behavior with real TTL input

The current repo convention follows Cargo’s unit-vs-integration split:

- keep small unit tests close to the owning module or topic folder
- move larger internal behavior tests into dedicated `src/tests/*` files once runtime files start mixing behavior and verification
- keep black-box crate/API checks under `crates/*/tests`

### Contributor Checklist

For a normal bounded change:

- choose the owning module before writing code
- reuse an existing topic folder if the concern already exists
- extract shared parsing/mapping/projection logic instead of copying it
- update tests in the same slice as the runtime change
- update README/spec/ops docs when a public surface, bounded scope, or config knob changes
- run at least `cargo fmt --all`, `cargo check`, and the relevant crate tests

## Useful Commands

Run the workspace checks that usually matter first:

```powershell
cargo check
cargo test -p nrese-core
cargo test -p nrese-store
cargo test -p nrese-reasoner
cargo test -p nrese-server --tests
```

Run the frontend checks:

```powershell
Set-Location .\apps\nrese-console
npm run build
npm run test -- --run
Set-Location ..\..
```

Run the benchmark and compatibility harness:

```powershell
cargo test --manifest-path benches/nrese-bench-harness/Cargo.toml
```

Seed and compare against a reference endpoint:

- see [docs/ops/benchmark-and-conformance.md](docs/ops/benchmark-and-conformance.md)

## Documentation

The HTTP interface is described in [docs/ops/http-api.md](docs/ops/http-api.md). The document index lives in one place: [Spezifikation.md](Spezifikation.md). Start with [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) and [docs/ROADMAP.md](docs/ROADMAP.md).

## Licence

Copyright (C) 2026 Krarilotus.

NRESE is free software: you can redistribute it and modify it under the terms of the **GNU Affero General Public License, version 3** ([LICENSE](LICENSE)), as published by the Free Software Foundation. It comes without any warranty.

In short, and without replacing the licence text:
- You may use, study, change and redistribute it, also commercially.
- If you distribute it, or let others use a modified version over a network, you must offer them the complete corresponding source under the same licence.
- Applications that only talk to a server over its HTTP protocols aren't covered by that obligation, and neither is the data you store.

**Commercial licences** without these obligations are available from the copyright holder: open an issue on the repository to get in touch.

Third-party material keeps its own licence:
- the Rust and JavaScript dependencies (MIT, Apache-2.0, BSD and similar);
- the vendored vocabularies under `benches/nrese-bench-harness/fixtures/catalog-cache/` (see the README there);
- the W3C test suites, which are fetched and not part of this repository.

The v1 prototype published before October 2026 (the `main` branch up to commit `8088e9a`) carried `Apache-2.0` in its package metadata; that statement stands for those commits. Contributions: see [CONTRIBUTING.md](CONTRIBUTING.md).
