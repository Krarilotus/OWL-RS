# Building, running and developing NRESE

The long form of the README's quick start: building from source, the console, the CLI, and where to start in the code. Building without filling the disk, the gate and how to measure are in [CONTRIBUTING.md](../../CONTRIBUTING.md).

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
  `disabled`, `rdfs`, `rdfs-full`, `rdfs-plus`, `owl-horst`, `owl2-ql`, `owl2-rl` or `custom`
- `NRESE_REASONING_RULES`
  A Notation3 file of user rules: added to the mode's ruleset, or the whole program with `custom`
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

The canonical runtime configuration reference is [docs/ops/config-reference.md](../ops/config-reference.md). README only lists the common entry points.

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

### Where To Start

If you want to work on storage, dataset state, or SPARQL execution:

- start in [crates/nrese-store/src/lib.rs](../../crates/nrese-store/src/lib.rs)
- then look at the store service, staging, query, update, and graph-store modules

If you want to work on reasoning:

- start in [docs/design/reasoner-v2.md](../design/reasoner-v2.md) and [crates/nrese-reasoner/src/lib.rs](../../crates/nrese-reasoner/src/lib.rs)
- rules are data ([ir.rs](../../crates/nrese-reasoner/src/ir.rs), [rulesets.rs](../../crates/nrese-reasoner/src/rulesets.rs)); list axioms are compiled per axiom ([lists.rs](../../crates/nrese-reasoner/src/lists.rs))
- [naive.rs](../../crates/nrese-reasoner/src/naive.rs) is the reference evaluator every executor is tested against
- [batch.rs](../../crates/nrese-reasoner/src/batch.rs) materialises (grounding, parallel semi-naive evaluation, transitive and equality modules); [delta.rs](../../crates/nrese-reasoner/src/delta.rs) maintains the closure per commit (DRed with B/F proofs)
- reasoning over the store's engine (compile, materialise, maintain on commits, explain) is [crates/nrese-reasoner/src/engine/](../../crates/nrese-reasoner/src/engine/mod.rs); the store decides when ([crates/nrese-store/src/reasoning.rs](../../crates/nrese-store/src/reasoning.rs) keeps its reports)

If you want to work on HTTP, auth, or operator surfaces:

- start in [crates/nrese-server/src/lib.rs](../../crates/nrese-server/src/lib.rs)
- routing and handlers are under `crates/nrese-server/src/http/`
- environment-variable names and config parsing entry points are centralized under `crates/nrese-server/src/config/`
- AI provider integrations are under `crates/nrese-server/src/ai/`

If you want to work on the user frontend:

- start in `apps/nrese-console/src/App.tsx`
- the frontend/backend contract is documented in [docs/dev/frontend-backend-contract.md](../dev/frontend-backend-contract.md)
- API calls and frontend transport helpers are under `apps/nrese-console/src/lib/`
- endpoint ownership is centralized in `apps/nrese-console/src/lib/endpoints.ts`
- the shared frontend TypeScript client is in `apps/nrese-console/src/lib/client.ts`
- browser runtime config is in `apps/nrese-console/src/lib/runtimeConfig.ts`
- CLI entry points are in `apps/nrese-console/src/cli/`
- UI components are under `apps/nrese-console/src/components/`
- language strings are under `apps/nrese-console/src/i18n/`
- styling tokens and layout files are under `apps/nrese-console/src/styles/`
- extension guidance is in [docs/dev/frontend-extension-guide.md](../dev/frontend-extension-guide.md)

If you want to work on benchmarks: start at [benches/README.md](../../benches/README.md) (the map of every kit) and [benches/PROTOCOL.md](../../benches/PROTOCOL.md) (the rules).

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

