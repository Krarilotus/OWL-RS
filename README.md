# NRESE

NRESE is a Rust workspace for a semantic data server.

The project is intended to replace a Fuseki-based setup over time with a Rust-native codebase that covers:

- SPARQL query and update handling
- Graph Store HTTP operations
- dataset storage and revision handling
- bounded reasoning and consistency checks
- an HTTP server and operator surface
- a user-facing web console

It is an active implementation project, not a finished replacement yet. The current state and remaining gaps are tracked in [docs/spec/06-fuseki-replacement-gap-matrix.md](docs/spec/06-fuseki-replacement-gap-matrix.md).

## What Is In This Repository

- `crates/nrese-core`
  Shared types, contracts, and traits used across the workspace.
- `crates/nrese-store`
  Dataset storage, snapshot access, Graph Store behavior, SPARQL execution, and staging.
- `crates/nrese-reasoner`
  Rule execution, consistency checks, explanation payloads, and reasoning profiles.
- `crates/nrese-server`
  HTTP API, operator endpoints, policy handling, and runtime wiring.
- `apps/nrese-console`
  React/TypeScript user frontend for query, tell, update, graph, and AI-assisted query suggestion workflows.
- `docs/spec`
  Project specification, roadmap, and replacement gap tracking.
- `docs/ops`
  Setup, maintenance, migration, benchmarking, and operational runbooks.
- `benches/nrese-bench-harness`
  A Rust-native comparison and benchmark harness for NRESE and Fuseki.

## Current Status

Implemented today:

- SPARQL query/update endpoints
- first-class `TELL` assertion ingest endpoint for RDF payloads
- Graph Store endpoint surface
- user-facing `/console` frontend for query, tell, update, graph, and dataset workflows
- guided workbench examples in `/console` so less SPARQL-native users can start from working query/update/tell/graph templates
- frontend locale selection with persisted `en`/`de` i18n instead of browser-guess-only language handling
- service description, health, readiness, metrics, and operator endpoints
- staged update validation before publish
- bounded `rules-mvp` reasoning with canonical `owl:sameAs` equality handling, bounded functional / inverse-functional equality entailment, bounded binary `owl:propertyChainAxiom` support over named-node RDF lists, bounded `owl:Nothing` effective-type rejection, and explicit unsupported-construct diagnostics
- typed `rules-mvp` presets (`rdfs-core`, `bounded-owl`) on top of the explicit feature-policy path
- snapshot-keyed memoization for repeated `rules-mvp` runs over identical dataset state
- schema-keyed memoization for `rules-mvp` preparation reuse across ABox-only changes
- cache/runtime telemetry for `rules-mvp` execution and schema reuse, exposed in reasoning diagnostics and Prometheus metrics
- prepared property-consistency indexing so `rules-mvp` reuses one grouped assertion view per run for constrained predicates instead of rescanning property closure for each consistency gate
- local comparison harness for NRESE vs Fuseki on seeded datasets
- manifest-driven workload-pack execution for production-style seed + compat + bench runs
- real-world ontology catalog sync for staged parity and hardening runs against FOAF, W3C ORG, W3C Time, PROV-O, SKOS, SOSA, SSN, and DCAT
- protocol compatibility harness coverage for query parity, update-effect parity, graph-store read/head/delete/put/post-effect parity, a bounded graph-store failure-parity slice, and bounded query/update failure-parity fixtures for covered negative cases
- bounded `bearer-jwt` auth alongside `bearer-static`
- bounded proxy-terminated `mtls` auth alongside the existing bearer modes
- bounded `oidc-introspection` auth alongside the existing bearer and proxy-terminated `mtls` modes
- file-based `config.toml` runtime configuration with environment overrides on the same typed parser path
- optional server-side AI query suggestions via Gemini or OpenRouter on one typed config path
- AI assistant now surfaces configured provider/model metadata and clearer empty-state behavior in the user console
- the user console now reads reasoning preset/policy/cache state from the real server diagnostics surface instead of maintaining local pseudo-config state

Not finished yet:

- persistence is partial: durable mode and backup/restore exist, but crash-recovery and drill-evidence gates are still open
- broader EL/RL/DL reasoning coverage
- full conformance and benchmark automation in CI
- production auth is partial: `bearer-static`, bounded `bearer-jwt`, bounded proxy-terminated `mtls`, and bounded `oidc-introspection` exist, while broader hardening work remains open
- real-world replacement evidence on the full ontology and workload set
- a project-specific production workload parity pack for replacement-grade evidence
- broader frontend production evidence and project-specific workflow validation
- timeout-oriented parity evidence and pack execution against the real secured Fuseki workload

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

Optional environment variables:

- `NRESE_ONTOLOGY_PATH`
  Path to an ontology file to preload.
- `NRESE_REASONING_MODE`
  Example: `rules-mvp`
- `NRESE_REASONER_RULES_MVP_FEATURES`
  Example: `rdfs-subclass-closure,rdfs-subproperty-closure,rdfs-type-propagation,rdfs-domain-range-typing,owl-property-assertion-closure,owl-equality-reasoning,owl-consistency-check,unsupported-diagnostics`
- `NRESE_REASONER_RULES_MVP_PRESET`
  Example: `bounded-owl`
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

- start in [crates/nrese-core/src/lib.rs](c:/Users/Johannes/Documents/OWL-RS/crates/nrese-core/src/lib.rs)

If you want to work on storage, dataset state, or SPARQL execution:

- start in [crates/nrese-store/src/lib.rs](c:/Users/Johannes/Documents/OWL-RS/crates/nrese-store/src/lib.rs)
- then look at the store service, staging, query, update, and graph-store modules

If you want to work on reasoning:

- start in [crates/nrese-reasoner/src/service.rs](c:/Users/Johannes/Documents/OWL-RS/crates/nrese-reasoner/src/service.rs)
- profile declarations are in [crates/nrese-reasoner/src/profile.rs](c:/Users/Johannes/Documents/OWL-RS/crates/nrese-reasoner/src/profile.rs)
- bounded rules are orchestrated from [crates/nrese-reasoner/src/rules.rs](c:/Users/Johannes/Documents/OWL-RS/crates/nrese-reasoner/src/rules.rs)
- typed reasoner runtime configuration is owned in [crates/nrese-reasoner/src/config.rs](c:/Users/Johannes/Documents/OWL-RS/crates/nrese-reasoner/src/config.rs), while external parsing and precedence live in the grouped `crates/nrese-server/src/config/` modules
- `rules-mvp` memoization and prepared-artifact reuse are implemented in the grouped [crates/nrese-reasoner/src/rules_mvp_cache/mod.rs](c:/Users/Johannes/Documents/OWL-RS/crates/nrese-reasoner/src/rules_mvp_cache/mod.rs) module with separate schema and prepared-run files
- dataset indexing is grouped under [crates/nrese-reasoner/src/dataset_index/mod.rs](c:/Users/Johannes/Documents/OWL-RS/crates/nrese-reasoner/src/dataset_index/mod.rs) with builder, vocabulary-id, stats, and test files kept in the same topic folder
- identity/equality handling is grouped under [crates/nrese-reasoner/src/identity/mod.rs](c:/Users/Johannes/Documents/OWL-RS/crates/nrese-reasoner/src/identity/mod.rs) with separate equality, entailment, and consistency files
- effective type derivation is grouped under [crates/nrese-reasoner/src/effective_types/mod.rs](c:/Users/Johannes/Documents/OWL-RS/crates/nrese-reasoner/src/effective_types/mod.rs) with separate builder, origin, and test files
- property closure is grouped under [crates/nrese-reasoner/src/property_closure/mod.rs](c:/Users/Johannes/Documents/OWL-RS/crates/nrese-reasoner/src/property_closure/mod.rs) with separate builder, equality-expansion, and test files
- class-side consistency checks are grouped under [crates/nrese-reasoner/src/class_consistency/mod.rs](c:/Users/Johannes/Documents/OWL-RS/crates/nrese-reasoner/src/class_consistency/mod.rs), and property-side consistency checks are grouped under [crates/nrese-reasoner/src/property_consistency/mod.rs](c:/Users/Johannes/Documents/OWL-RS/crates/nrese-reasoner/src/property_consistency/mod.rs)

If you want to work on HTTP, auth, or operator surfaces:

- start in [crates/nrese-server/src/lib.rs](c:/Users/Johannes/Documents/OWL-RS/crates/nrese-server/src/lib.rs)
- routing and handlers are under `crates/nrese-server/src/http/`
- environment-variable names and config parsing entry points are centralized under `crates/nrese-server/src/config/`
- AI provider integrations are under `crates/nrese-server/src/ai/`

If you want to work on the user frontend:

- start in `apps/nrese-console/src/App.tsx`
- API calls and frontend transport helpers are under `apps/nrese-console/src/lib/`
- UI components are under `apps/nrese-console/src/components/`
- language strings are under `apps/nrese-console/src/i18n/`
- styling tokens and layout files are under `apps/nrese-console/src/styles/`
- extension guidance is in [docs/dev/frontend-extension-guide.md](docs/dev/frontend-extension-guide.md)

If you want to work on benchmarks or compatibility checks:

- start in [benches/nrese-bench-harness/src/main.rs](c:/Users/Johannes/Documents/OWL-RS/benches/nrese-bench-harness/src/main.rs)
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

Seed and compare against Fuseki:

- see [docs/ops/benchmark-and-conformance.md](docs/ops/benchmark-and-conformance.md)

## Documentation

- [Spezifikation.md](Spezifikation.md)
- [docs/spec/01-architecture-workspace.md](docs/spec/01-architecture-workspace.md)
- [docs/spec/02-storage-and-transactions.md](docs/spec/02-storage-and-transactions.md)
- [docs/spec/03-reasoner-and-owl-profile.md](docs/spec/03-reasoner-and-owl-profile.md)
- [docs/spec/04-api-and-protocols.md](docs/spec/04-api-and-protocols.md)
- [docs/spec/05-roadmap-and-acceptance.md](docs/spec/05-roadmap-and-acceptance.md)
- [docs/spec/06-fuseki-replacement-gap-matrix.md](docs/spec/06-fuseki-replacement-gap-matrix.md)
- [docs/spec/07-replacement-implementation-plan.md](docs/spec/07-replacement-implementation-plan.md)
- [docs/ops/server-setup.md](docs/ops/server-setup.md)
- [docs/ops/config-reference.md](docs/ops/config-reference.md)
- [docs/ops/server-maintenance.md](docs/ops/server-maintenance.md)
- [docs/ops/benchmark-and-conformance.md](docs/ops/benchmark-and-conformance.md)
- [docs/ops/ontology-fixture-catalog.md](docs/ops/ontology-fixture-catalog.md)
- [docs/ops/backup-restore-drills.md](docs/ops/backup-restore-drills.md)
- [docs/dev/code-structure-guidelines.md](docs/dev/code-structure-guidelines.md)
- [docs/dev/frontend-extension-guide.md](docs/dev/frontend-extension-guide.md)
