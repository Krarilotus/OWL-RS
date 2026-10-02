# The outside audit of 2 October 2026, as work packages

An audit of the branch at `cd75751` (report outside the repository; the owner holds it)
found the engine strong and the engineering system around it weaker. Every finding was
checked against the code; the verdicts are below, and the work is merged into the order of
the [engine gap plan](2026-10-02-engine-gaps.md): the bug hunt and the engine and feature
gaps stay first, the frontend after. Security and correctness findings are bugs and go
first.

## Verdicts on the ten main findings

| § | Finding | Verdict | Where |
|---|---|---|---|
| 2.1 | CI has never run (it triggers on `main` and pull requests only) | agree | A3 |
| 2.2 | The mTLS mode trusts a client-certificate header from any peer | agree: a security bug | A2 |
| 2.3 | Graph access is checked per handler, not by the store's read API | agree with the direction; no handler misses it today | B1 |
| 2.4 | The native SPARQL executor is a 5,400-line interpreter without a plan IR | agree; the main structural debt, and what blocks G4 | C (with G4) |
| 2.5 | The reasoner skips lists of more than 100 members; it reasons over the union of all graphs into the default graph | agree: the cap is a correctness bug (missed inconsistencies); the scope is a design gap now that workspaces exist | A2 (cap), D (scopes) |
| 2.6 | Errors are strings in the middle (the reasoner has no error type, the memory budget is recovered by downcast) | agree | B2 |
| 2.7 | Configuration exists three times (TOML, env-named strings, parsers) | agree; the documented `NRESE_*` names stay working | B3 |
| 2.8 | A read inside an RDF4J transaction replays all its operations while holding the single writer slot | agree on the problem, not on the proposed fix: keeping a transaction alive per session would hold the writer slot for the session. Instead: speculative transactions over a snapshot that never take the slot, cached per session by base revision and operation count; only the commit takes the slot | A2 |
| 2.9 | No algebra visitor; ten dead `_ => false` arms from spargebra | agree; the dead arms go now (new variants then fail to compile), the visitor with the plan IR | A2, C |
| 2.10 | The repository catalogue lives in the HTTP crate | agree (ADR-0007) | B4 |

Smaller findings taken over: Miri on the dictionary's `Decoded::keep` (a raw pointer into a
`Box` that is moved afterwards), the Docker image's `target-cpu=native` default, workspace
lints and dependencies, resolver 3, `cargo-deny`, `#[expect]` for `#[allow]`, test-only code
behind a feature, one integration-test binary per crate, `eprintln!` and environment reads
in the engine, numeric promotion implemented twice, vocabulary IRIs spelled out.

Deferred, with reasons:
- **The compatibility rewrites as a `CompatProfile`.** Both rewrites (`HAVING` on a `SELECT`
  alias, dropping Blazegraph's query hints) do change some standard queries' results, but
  only queries whose standard answer is always empty; low priority.
- **Every planner and executor constant as configuration.** They become hardware-derived
  settings with G6 (hardware and scale-out); one at a time where a use case needs it
  before. The explanation budget becomes a request parameter with B.
- **Moving the AI query suggestions out of the server:** not core; revisit with the
  frontend.
- **Benchmark credibility** (concurrency, BSBM and WatDiv, a billion-triple run, an outside
  check of the QLever setup, RDFox and GraphDB once licensed): with the benchmarks, in bulk
  at the end of a batch.

## Work packages, in order

### A. Hygiene and trust (now)

Status (2 October): A1 and A2 done; A3 done but the first CI run. Found on the way: the
`ORDER BY` order wasn't total over terms of one canonical form (fuzz seed 2329, fixed);
cargo-deny's first run found seven advisories in the dependency tree (`h2`, `rustls`,
`rustls-webpki`, `anyhow`, `rand`, and `quick-xml` 0.37 in `nrese-sparql`, quadratic on
crafted XML), all fixed by updates; of the ten wildcard arms six covered real variants
(`LATERAL`, triple terms, custom aggregates) and now name them.

- **A1. `tmp/` and the legacy tree.** The reusable runners and probes committed with usage
  lines (`scripts/office/`, `benches/probes/`, `benches/suite/batch.sh` and `summarise.py`,
  `scripts/researchspace-template-queries.py`); spent edit scripts, backups, bundles, logs and
  spec copies deleted; `artifacts/` (the Fuseki era's output), `ops/fuseki/` and
  `Spezifikation.md` removed (the index is [docs/README.md](../README.md) now).
- **A2. Security and correctness bugs.**
  - mTLS: `auth.mtls.trusted_proxies` (address ranges; loopback by default): the subject
    header counts only from them.
  - Reads inside client transactions (RDF4J, sessions) on speculative transactions that
    don't take the writer slot, cached per session.
  - The reasoner's list cap removed: `owl:AllDifferent`, `owl:oneOf`, `owl:hasKey`,
    `owl:unionOf`, `owl:intersectionOf` of any length, handled natively where expansion
    into rules is quadratic.
  - `Decoded::keep` without the moved `Box` (and Miri over the engine's unsafe code).
  - The ten dead wildcard arms deleted.
- **A3. One gate and the CI.**
  - `scripts/check.sh`: fmt, clippy on all targets, the tests, the lock check,
    `cargo-deny`; the gate for every commit, with a pre-push hook.
  - Workspace lints, dependencies inherited from the workspace, resolver 3, `rust-version`,
    duplicate versions aligned; the Docker image built for a portable CPU level by default.
  - CI at milestones only: `ci.yml` and the oracle workflow get `workflow_dispatch` on `main`
    (one small commit there); at a milestone they are started on the branch with
    `gh workflow run ci.yml --ref refactor/engine-v2`, then made green on Linux. The
    oracle's nightly schedule goes (weekly at most).
  - One integration-test binary per crate and `cargo-nextest`, where the link time pays.

### B. Typed boundaries (with the rest of G2)

- **B1.** A mandatory `ReadScope` on every store read and write (administrators pass
  `ReadScope::all()` explicitly), one `ReadContext` instead of the `_pending`/`_as`/`_str`
  variants, and statements streamed instead of collected. The shape: `ReadScope` is
  `All` or `Graphs(Arc<GraphAccess>)` (built from the requester's `AccessView`, so the
  inferred-statements rule travels with it); `ReadContext { scope, model, source: Latest |
  Pending(&StatementsRequest) | Session(id), cancel }`; one store method per operation
  (`query`, `statements`, `count`, `contexts`, `graphs`, `autocomplete`, `export`) taking a
  `&ReadContext`, none without one. The access fields of `StatementPattern` and the query
  requests go. Sessions keep their speculative state cached by base revision and operation
  count, so repeated reads don't replay (the rest of §2.8).

  Status: B1a (every read takes a `ReadScope`; whole-dataset operations refuse a
  restricted one) and B1b (`ReadContext { scope, infer, pending, cancel }` with
  `statements`, `count`, `write_statements` and `execute_graph_read`; RDF4J's statements
  streamed; a session's view kept by base version, operation count and the reader's access)
  and B1c are done. B1c: every write takes a `Requester { read: ReadScope, write:
  WriteScope }` (the pipeline's `apply`, `StoreService::apply`, the server's
  `mutation::run`); the access fields of `SparqlUpdateRequest`, `StatementsRequest` and
  `StatementPattern` are gone, so requests are data only. After any command, every graph
  the transaction changes is checked against the write scope (Graph Store writes, TELL and
  deletes included, which only the handlers checked before), and a restore needs `All`.
  Reads inside a session replay its operations with the reader's read scope; what they
  change is checked at the commit. Sessions also got random ids and an owner (they were
  `tx-1`, `tx-2`, … and anyone's).
- **B2.** Error enums: the reasoner's (rule files with line and column), the memory budget
  as a variant, `Forbidden` and `Configuration` typed.

  Status: done, but for `Configuration`. Rule errors carry a `Position` (native rules and
  `.pie`: the line of the rule or axiom; N3: the parser's line and column) and print it.
  `QueryEvaluationError::MemoryLimit(BudgetExceeded)` replaces the downcast from `Dataset`.
  `StoreError::Forbidden(Refusal)`: `Write(GraphName)`, `ReadAll`, `WriteAll`; the
  pipeline's final check names the graph too. `Configuration(String)` stays: nothing
  branches on it, and B3's typed configuration tree reports its mistakes where they are.
- **B3.** One typed configuration tree, from defaults, the file, the environment and the
  command line by one mechanism; its JSON Schema generated for the docs and the frontend;
  the `NRESE_*` names kept.

  Status: done as a registry, not as one serde tree. `config/settings.rs` declares every
  setting once (file key, older keys, environment name, kind, secrecy, description); the
  file is read through it (the 770 lines of raw structs and mapping are gone), the
  command line's `--set key=value` too, and `nrese-server config-schema` prints the JSON
  Schema generated from it. Tests check that every environment name the parsers read is a
  setting, that every listed choice loads, and that the operator reference names every
  key and variable. The typed parse (units, cross-field checks, auth modes) stays in the
  `*_env.rs` parsers, which already were the one place each value is checked: moving it
  into serde types would have rewritten tested code for no new behaviour. Left for B4:
  the schema and the effective settings over the engine API, for the console.
- **B4.** The engine API for real: the repository catalogue in the store, one `Operation`
  enum with one middleware (authorisation, limits, metrics, tracing, deadlines) that every
  protocol translates to.

  Status: B4a done: `nrese_store::catalog` holds every repository, the default one
  included (its stored settings, its write path), with `RepositorySettings` and a typed
  `CatalogError`; the server keeps the RDF4J and GraphDB configuration reading and maps the
  errors to HTTP. Tested in the store without a server.

  B4b done, differently from the plan's wording: authorisation already was one function
  over one enum (`PolicyAction`), so a new `Operation` enum would have renamed it. What
  was wrong was the order: each handler authenticated inside its body, after axum had
  read the request body, so a client without credentials could make the server buffer
  uploads up to the body limit, and an unknown repository answered 404 before 401. Now
  every mode authenticates once (`AuthConfig::authenticate` → `Authenticated`), a
  middleware on every non-public route does it before the handler, and handlers check
  their action against it (`AppState::authorize`; no second introspection under OIDC).
  The public routes are listed in one router. Metrics, tracing, request ids and body
  limits already were one layer stack; deadlines stay per operation kind.

### C. The query engine (with G4)

The algebra visitor, a logical and a physical plan with the optimiser's special cases as
named rewrites, `nrese-sparql` split (plan, execution, functions, federation), numeric
promotion once in `nrese-xsd`. Then cross-BGP join ordering, characteristic sets and plan
caching on it.

### D. Reasoning (with G3)

Reasoning scopes per graph set (input graphs, output graph) so that inferences and graph
access compose; `nrese_reasoner::v2` flattened, `v1_scenarios.rs` renamed, `nrese-core` and
`ReasonerService` folded in; then OWL 2 DL ([ADR-0009](../adr/0009-owl2-dl-reasoning.md)).

### E. Benchmarks (in bulk, at the end of a batch)

Multi-client mixed workloads, BSBM and WatDiv results, a billion-triple run, the QLever
setup checked by someone outside the project, RDFox and GraphDB once their licences allow.
