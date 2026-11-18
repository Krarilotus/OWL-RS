# Building, running and developing NRESE

This page is the long form of the README's [quick start](../../README.md#quick-start), for building from source, the console, the CLI and where to start in the code. Building without filling the disk, the gate and how to measure are in [CONTRIBUTING.md](../../CONTRIBUTING.md).

## Setup

### Prerequisites

- Rust 1.98 or later (`rust-version` in [Cargo.toml](../../Cargo.toml), and the builds here use the
  version [rust-toolchain.toml](../../rust-toolchain.toml) names), with Cargo
- Node.js, for the console
- Docker, if you want to build and run the image

### Build

```bash
cargo build
```

### Build the user frontend

The server embeds the console it finds in `apps/nrese-console/dist` when it is compiled, so build the console first if you want `/console` (the API works without it).

```bash
cd apps/nrese-console
npm install
npm run build
cd ../..
```

### Run the user frontend in dev mode

Start the Rust server first, so that the frontend's dev proxy has a backend to forward API calls to:

```bash
cargo run -p nrese-server
```

Then, in a second terminal:

```bash
cd apps/nrese-console
npm install
npm run dev
```

- Open the Vite app at `http://127.0.0.1:5173/console/`.
- The frontend's requests to `/dataset/*`, `/ops/*` and `/api/*` are proxied to `http://127.0.0.1:8080`.
- If your backend runs elsewhere, set `VITE_API_PROXY_TARGET`, for example like this:

```bash
VITE_API_PROXY_TARGET=http://127.0.0.1:9090 npm run dev
```

### Run the frontend against a separate backend

For a built frontend, configure the API's address at run time instead of hardcoding it into components:

```js
window.__NRESE_CONSOLE_CONFIG__ = {
  apiBaseUrl: "https://nrese.example.com",
};
```

This lives in `apps/nrese-console/public/console-config.js`. You can also set it at build time
with `VITE_NRESE_API_BASE_URL` and `VITE_CONSOLE_BASE_PATH`.

### Run the server

```bash
cargo run -p nrese-server
```

With only Rust, you get the server and its API on `127.0.0.1:8080`, from memory (nothing
is written to disk) and with reasoning off. Set `NRESE_STORE_MODE=on-disk` and
`NRESE_REASONING_MODE` to change that ([configuration](../ops/config-reference.md)), and
`--release` builds it optimised. With reasoning off, the quick start's first query answers no
rows, as `complete`, because nothing is inferred. Start the server with
`NRESE_REASONING_MODE=owl2-dl` and `NRESE_SHACL_GATE=enforce` for the quick start's example and
the [tutorial](../tutorial.md). The operator page `/ops` is built in, and the browser console is
embedded only when it was built first ([above](#build-the-user-frontend)). Without it,
`/console` says how.

Run with an explicit config file:

```bash
cargo run -p nrese-server -- --config ./config.toml
```

### Frontend routes

- `/console` is the user's console for query, tell, update, the Graph Store and AI-assisted
  query suggestions.
- `/ops` is the operator's console and diagnostics page.
- `/` redirects to `/console`.

### Frontend CLI

The frontend package also ships a small CLI on top of the same TypeScript client boundary
(`npm run cli -- help` prints its commands and options):

```bash
cd apps/nrese-console
npm install
npm run cli -- capabilities
npm run cli -- query --text "SELECT * WHERE { ?s ?p ?o } LIMIT 5"
```

| Command | What it sends ([HTTP interface](../ops/http-api.md)) |
|---|---|
| `runtime`, `capabilities`, `reasoning` | `GET /api/v1/health`, `/api/v1/capabilities`, `/api/v1/repositories/nrese/reasoning`; prints the JSON |
| `query` | the query to `/dataset/query`, asking for `--accept` (default `application/sparql-results+json`) |
| `update` | the update to `/dataset/update` |
| `tell` | the RDF to `/dataset/tell`, into `--graph-iri` or the default graph |
| `graph-read` | `GET /dataset/data` ([Graph Store Protocol](../ops/http-api.md#graph-store-protocol)) of `--graph-iri` or the default graph |
| `graph-write` | the RDF to `/dataset/data`, `--method PUT` (the default; needs `--graph default` or `--graph-iri`) or `POST` |
| `graph-delete` | `DELETE /dataset/data`; needs `--graph default` or `--graph-iri` |

- A command takes the text it sends from `--text <text>` or `--file <path>`.
- Every command takes `--base-url <url>` (else `NRESE_API_BASE_URL`, else
  `http://127.0.0.1:8080`), `--token <token>` (a bearer token, else `NRESE_API_TOKEN`) and
  repeated `--header name:value`.
- The commands that send text or graphs print `status=<code>` and then the body, or for an
  error one line `error: title: detail (request <id>)`.
- The exit status is 0, or 1 for an error status or a wrong argument (refused before any
  request), or 3 with `--require-complete` when the answer isn't shown complete
  (`NRESE-Completeness`). Without the option, that is a warning on stderr.

These environment variables are optional.

- `NRESE_ONTOLOGY_PATH` is the path to an ontology file to preload.
- `NRESE_REASONING_MODE` is `disabled`, `rdfs`, `rdfs-full`, `rdfs-plus`, `owl-horst`,
  `owl2-ql`, `owl2-rl`, `owl2-dl` or `custom`.
- `NRESE_REASONING_RULES` names a Notation3 file of user rules, added to the mode's ruleset, or
  the whole program with `custom`.
- `NRESE_DEPLOYMENT_POSTURE` is for example `read-only-demo`, `internal-authenticated` or
  `replacement-grade`.
- `NRESE_SPARQL_PARSE_ERROR_PROFILE` is for example `problem-json` or `plain-text`.
- `NRESE_STORE_MODE` is for example `in-memory`.
- `NRESE_BIND_ADDR` is for example `127.0.0.1:8080`.
- `NRESE_AI_ENABLED` is for example `true`.
- `NRESE_AI_PROVIDER` is for example `gemini`.
- `GOOGLE_API_KEY` is used as the Gemini API key when `NRESE_AI_GOOGLE_API_KEY` is not set.

Every setting is in [docs/ops/config-reference.md](../ops/config-reference.md), and this list
gives only the common ones.

### Compare answers with a reference engine

The bench harness's `pack-matrix` command runs workload packs on NRESE and on a reference engine
and compares the answers. It is run by hand
([benches/nrese-bench-harness/README.md](../../benches/nrese-bench-harness/README.md)).

### Run with durable storage

```bash
NRESE_STORE_MODE=on-disk NRESE_DATA_DIR=./data cargo run -p nrese-server
```

On-disk mode is always available. It writes a write-ahead log and checkpoints under
`NRESE_DATA_DIR`, locks the directory against a second process and recovers the last committed
revision on start.

## Development

### Where to start

If you want to work on storage, dataset state or SPARQL execution:

- Start in [crates/nrese-store/src/lib.rs](../../crates/nrese-store/src/lib.rs).
- Then look at the store service and the staging, query, update and graph-store modules.

If you want to work on reasoning:

- Start in [docs/design/reasoner-v2.md](../design/reasoner-v2.md) and [crates/nrese-reasoner/src/lib.rs](../../crates/nrese-reasoner/src/lib.rs).
- Rules are data ([ir.rs](../../crates/nrese-reasoner/src/ir.rs), [rulesets.rs](../../crates/nrese-reasoner/src/rulesets.rs)), and list axioms are compiled per axiom ([lists.rs](../../crates/nrese-reasoner/src/lists.rs)).
- [naive.rs](../../crates/nrese-reasoner/src/naive.rs) is the reference evaluator every executor is tested against.
- [batch.rs](../../crates/nrese-reasoner/src/batch.rs) materialises (grounding, parallel semi-naive evaluation, transitive and equality modules), and [delta.rs](../../crates/nrese-reasoner/src/delta.rs) maintains the closure per commit (DRed with B/F proofs).
- Reasoning over the store's engine (compile, materialise, maintain on commits, explain) is in [crates/nrese-reasoner/src/engine/](../../crates/nrese-reasoner/src/engine/mod.rs), and the store decides when it runs ([crates/nrese-store/src/reasoning.rs](../../crates/nrese-store/src/reasoning.rs) keeps its reports).

If you want to work on HTTP, auth or the operator pages:

- Start in [crates/nrese-server/src/lib.rs](../../crates/nrese-server/src/lib.rs).
- Routing and handlers are under `crates/nrese-server/src/http/`.
- The names of the environment variables and the entry points of config parsing are all under `crates/nrese-server/src/config/`.
- The AI provider integrations are under `crates/nrese-server/src/ai/`.

If you want to work on the user frontend:

- Start in `apps/nrese-console/src/App.tsx`.
- The contract between frontend and backend is in [docs/dev/frontend-backend-contract.md](../dev/frontend-backend-contract.md).
- API calls and the frontend's transport helpers are under `apps/nrese-console/src/lib/`.
- Endpoint ownership is kept in one place, `apps/nrese-console/src/lib/endpoints.ts`.
- The shared TypeScript client of the frontend is in `apps/nrese-console/src/lib/client.ts`.
- The browser's runtime config is in `apps/nrese-console/src/lib/runtimeConfig.ts`.
- The CLI's entry points are in `apps/nrese-console/src/cli/`.
- UI components are under `apps/nrese-console/src/components/`.
- Language strings are under `apps/nrese-console/src/i18n/`.
- Styling tokens and layout files are under `apps/nrese-console/src/styles/`.
- How to extend it is in [docs/dev/frontend-extension-guide.md](../dev/frontend-extension-guide.md).

If you want to work on benchmarks, start at [benches/README.md](../../benches/README.md) (the map of every kit) and [benches/PROTOCOL.md](../../benches/PROTOCOL.md) (the rules).

### Test layout

The repository has two kinds of tests. Integration-style crate tests are under `crates/*/tests`,
and unit tests for internal behaviour are in their own test files next to the modules, for
example under `crates/nrese-reasoner/src/tests`.

The aim is that runtime code stays focused on runtime behaviour, that tests stay close to the
concern they check and that private module behaviour can be tested without bloating production
files. Shared minimal RDF fixtures live under `fixtures/ontologies/` and exercise service-level
behaviour with real Turtle input.

The convention follows Cargo's split between unit and integration tests.

- Keep small unit tests close to the owning module or topic folder.
- Move larger internal behaviour tests into their own `src/tests/*` files once runtime files
  start mixing behaviour and verification.
- Keep black-box checks of a crate or its API under `crates/*/tests`.
