# NRESE

NRESE is a Rust RDF database under active redesign. The goal is a store that **reads like QLever and governs data like GraphDB**: its own storage engine, materialised RDFS/OWL 2 RL reasoning, SHACL validation on commit, full-text search and the RDF4J protocol. It is built to serve ResearchSpace and the datamodel workflow.

- **Architecture and ownership rules:** [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md)
- **Plan:** [docs/ROADMAP.md](docs/ROADMAP.md)
- **Status against QLever/GraphDB:** [docs/spec/06-target-capability-matrix.md](docs/spec/06-target-capability-matrix.md)
- **Decisions:** [docs/adr/](docs/adr/)
- **Document index (German):** [Spezifikation.md](Spezifikation.md)

## Current state (engine v1)

The running code is **v1**: an HTTP server over the Oxigraph store with SPARQL query/update, the Graph Store Protocol, `TELL` ingest, a bounded rule reasoner used as a write gate (`rules-mvp`), auth modes, deployment postures, an operator UI (`/ops`), a user console (`/console`) and a comparison harness.

Known v1 limits, all addressed by the roadmap's Milestone 1 (engine v2):
- Writes cost time proportional to the dataset size (the write path copies the dataset for validation).
- The reasoner only sees triples between IRIs, and its inferences can't be queried (`asserted-only`).
- The revision counter isn't persisted; durable mode needs RocksDB (and libclang on Windows).

Already fixed on the way (Milestone 0):
- A write that reports a timeout is guaranteed not to be committed.
- Unknown config values fail at startup instead of silently changing behaviour.
- Ontology preload happens only when `NRESE_ONTOLOGY_PATH` is set.

## Repository layout

| Path | Layer | Owns |
|---|---|---|
| `crates/nrese-core` | L0 | shared report and capability contracts |
| `crates/nrese-reasoner` | L2 | reasoning profiles, `rules-mvp`, consistency and explanations |
| `crates/nrese-store` | L3 | operations (query, update, graph store, tell, backup) and the mutation pipeline |
| `crates/nrese-server` | L4 | HTTP transport, auth, policy, posture, UI hosting |
| `crates/nrese-engine` | L1 | engine v2 storage (in progress, not yet part of the workspace build) |
| `apps/nrese-console` | L5 | React/TypeScript console and CLI |
| `benches/nrese-bench-harness` | tooling | black-box comparison and benchmark harness |
| `docs/` | — | architecture, ADRs, roadmap, specs, ops runbooks |

## Setup

### Prerequisites

- Rust toolchain
- Cargo
- optional: Docker, if you want to run a local Fuseki comparison stack
- optional on Windows: LLVM / `libclang` if you want to build durable storage dependencies

### Build

```powershell
cargo build
```

### Build The User Frontend

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
  Example: `rules-mvp`
- `NRESE_REASONER_READ_MODEL`
  Example: `asserted-only`
- `NRESE_REASONER_RULES_MVP_FEATURES`
  Example: `rdfs-subclass-closure,rdfs-subproperty-closure,rdfs-type-propagation,rdfs-domain-range-typing,owl-property-assertion-closure,owl-equality-reasoning,owl-consistency-check,unsupported-diagnostics`
- `NRESE_REASONER_RULES_MVP_PRESET`
  Example: `bounded-owl`
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
cargo run -p nrese-server --features durable-storage
```

Note:

- durable storage support exists behind a feature flag
- on Windows, RocksDB-related dependencies may require a working LLVM / `libclang` toolchain

## Development

### Where To Start

If you want to work on shared contracts:

- start in [crates/nrese-core/src/lib.rs](crates/nrese-core/src/lib.rs)

If you want to work on storage, dataset state, or SPARQL execution:

- start in [crates/nrese-store/src/lib.rs](crates/nrese-store/src/lib.rs)
- then look at the store service, staging, query, update, and graph-store modules

If you want to work on reasoning:

- start in [crates/nrese-reasoner/src/service.rs](crates/nrese-reasoner/src/service.rs)
- profile declarations are in [crates/nrese-reasoner/src/profile.rs](crates/nrese-reasoner/src/profile.rs)
- bounded rules are orchestrated from [crates/nrese-reasoner/src/rules.rs](crates/nrese-reasoner/src/rules.rs)
- typed reasoner runtime configuration is owned in [crates/nrese-reasoner/src/config.rs](crates/nrese-reasoner/src/config.rs), while external parsing and precedence live in the grouped `crates/nrese-server/src/config/` modules
- `rules-mvp` memoization and prepared-artifact reuse are implemented in the grouped [crates/nrese-reasoner/src/rules_mvp_cache/mod.rs](crates/nrese-reasoner/src/rules_mvp_cache/mod.rs) module with separate schema and prepared-run files
- dataset indexing is grouped under [crates/nrese-reasoner/src/dataset_index/mod.rs](crates/nrese-reasoner/src/dataset_index/mod.rs) with builder, vocabulary-id, stats, and test files kept in the same topic folder
- identity/equality handling is grouped under [crates/nrese-reasoner/src/identity/mod.rs](crates/nrese-reasoner/src/identity/mod.rs) with separate equality, entailment, and consistency files
- effective type derivation is grouped under [crates/nrese-reasoner/src/effective_types/mod.rs](crates/nrese-reasoner/src/effective_types/mod.rs) with separate builder, origin, and test files
- property closure is grouped under [crates/nrese-reasoner/src/property_closure/mod.rs](crates/nrese-reasoner/src/property_closure/mod.rs) with separate builder, equality-expansion, and test files
- class-side consistency checks are grouped under [crates/nrese-reasoner/src/class_consistency/mod.rs](crates/nrese-reasoner/src/class_consistency/mod.rs), and property-side consistency checks are grouped under [crates/nrese-reasoner/src/property_consistency/mod.rs](crates/nrese-reasoner/src/property_consistency/mod.rs)

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

The document index lives in one place: [Spezifikation.md](Spezifikation.md). Start with [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) and [docs/ROADMAP.md](docs/ROADMAP.md).
