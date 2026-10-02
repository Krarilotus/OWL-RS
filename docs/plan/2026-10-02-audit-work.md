# The outside audit of 2 October 2026, as work packages

An audit of the branch at `cd75751` (report outside the repository; the owner holds it)
found the engine strong and the engineering system around it weaker. Every finding was
checked against the code; the verdicts are below, and the work is merged into the order of
the [engine gap plan](2026-10-02-engine-gaps.md): the bug hunt and the engine and feature
gaps stay first, the frontend after. Security and correctness findings are bugs and go
first. This plan holds the design and the order; what is done is in
[STATUS.md](../STATUS.md).

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

- **B2.** Error enums: the reasoner's (rule files with line and column), the memory budget
  as a variant, `Forbidden` and `Configuration` typed.

  Decision: `Configuration(String)` stays a string: nothing branches on it, and the
  configuration's own parsers (B3) report its mistakes where they are.
- **B3.** One typed configuration tree, from defaults, the file, the environment and the
  command line by one mechanism; its JSON Schema generated for the docs and the frontend;
  the `NRESE_*` names kept.

  Decision: a registry, not one serde tree. `config/settings.rs` declares every setting
  once (file key, older keys, environment name, kind, secrecy, description), and the file,
  the environment and `--set` are read through it; the JSON Schema is generated from it.
  The typed parse (units, cross-field checks, auth modes) stays in the `*_env.rs`
  parsers, which already were the one place each value is checked: moving it into serde
  types would rewrite tested code for no new behaviour. Later, with the console: the
  schema and the effective settings over the engine API.
- **B4.** The engine API for real: the repository catalogue in the store, one `Operation`
  enum with one middleware (authorisation, limits, metrics, tracing, deadlines) that every
  protocol translates to.

  Decision for the middleware: authorisation already was one function over one enum
  (`PolicyAction`), so a new `Operation` enum would rename it. What needs one place is the
  order: authentication once, in a middleware on every non-public route, before the
  handler and before its body is read; the handlers check their action against it. The
  public routes are listed in one router. Metrics, tracing, request ids and body limits
  are one layer stack already; deadlines stay per operation kind.

### C. The query engine (with G4)

The design: [the query plan](2026-10-02-plan-ir.md). The algebra visitor, a logical and a
physical plan with the optimiser's special cases as
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

### F. Assurance

Requested by the owner on 2 October, alongside C:

- **F1. Fuzzing the parsers with `cargo-fuzz`.** Every parser of untrusted HTTP input gets
  a target: Turtle, TriG, N-Triples, N-Quads, N3, RDF/XML, JSON-LD, SPARQL queries and
  updates, and the SPARQL results formats (JSON, XML, CSV, TSV) read by federation. A
  target asserts no panic and, where a format can be written, that what is read writes
  and reads back the same. The corpora seed from the W3C test suites; the targets run in
  the fuzz campaign beside the differential tests.
- **F2. Miri over the engine's unsafe modules**, as a recurring job on the office PC (a
  nightly toolchain there, not in the gate): the dictionary's decoded-term arena, the
  memory-mapped checkpoint readers, the SIMD kernels' scalar paths, CPU detection. Its
  findings are bugs.
- **F3. Coverage, measured once** with `cargo-llvm-cov` over the whole workspace: which
  modules the tests don't reach, as a list of follow-ups, not a target number.
