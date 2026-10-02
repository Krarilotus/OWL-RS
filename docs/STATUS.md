# Status

The one place for the state of the work. Each item is **done** (implemented, tested,
in the branch), **deferred** (not done, with the reason and where it went), or **open**.
"Done" means done: what isn't, says so here.

- What the product can do, capability by capability: the
  [capability matrix](spec/06-target-capability-matrix.md).
- Why and in which order: the plans ([audit work](plan/2026-10-02-audit-work.md),
  [engine gaps](plan/2026-10-02-engine-gaps.md), [research designs](plan/2026-10-02-research-designs.md),
  [query plan](plan/2026-10-02-plan-ir.md)). They hold design and order, no status. Older plans
  (30 September, 1 October) are records of their date; their status columns are not kept
  up to date.

Last updated: 2 October 2026, branch `refactor/engine-v2`.

## Audit work (the outside audit of 2 October)

### A. Hygiene and trust

| Item | State | Notes |
|---|---|---|
| A1 `tmp/` and the legacy tree | done | Runners and probes committed with usage lines; spent files, `artifacts/`, `ops/fuseki/`, `Spezifikation.md` removed |
| A2 mTLS subject header from trusted proxies only | done | `auth.mtls.trusted_proxies`, loopback by default |
| A2 Reads in client transactions without the writer slot | done | Speculative transactions; the view cached per session by base version, operation count and the reader's access |
| A2 The reasoner's list cap | done | Lists of any length; native rules where expansion is quadratic |
| A2 `Decoded::keep` without the moved `Box` | done | Raw pointers with a `Drop`; a test |
| A2 Miri over the engine's unsafe code | deferred | Never ran (Miri isn't installed on the main PC). Planned as a recurring job on the office PC: [F2](plan/2026-10-02-audit-work.md#f-assurance) |
| A2 Dead wildcard arms | done | Four deleted; six covered real variants and name them |
| A3 `scripts/check.sh` gate, pre-commit and pre-push hooks | done | The pre-commit gate tests the changed crates and every crate depending on them |
| A3 Workspace lints, resolver 3, `rust-version`, `publish = false` | done | Every suppression is an `#[expect]`; no `#[allow]` left |
| A3 Dependencies declared once in the workspace | done | Every external dependency two or more crates use (20 workspace entries) |
| A3 Duplicate dependency versions | partly | Our direct dependencies are aligned; duplicates pulled in by third-party crates remain (`rand` 0.9/0.10, `getrandom` 0.2/0.3/0.4, `digest` 0.10/0.11, `syn` 2/3, `thiserror` 1/2, `winnow` 0.7/1.0) |
| A3 Docker image for a portable CPU level | done | `NRESE_TARGET_CPU=x86-64-v3` by default |
| A3 CI at milestones | done | `ci.yml` and `oracle.yml` on `workflow_dispatch`; runs: 36998782678, 37007059759 (green). The oracle compared nothing from 1 October (renamed tests) until `1516dce`; it now fails on an empty comparison, and compares NRESE on the canonical copy too. Locally on `1e5a22b`: 9,390 queries, none unexplained |
| A3 One integration-test binary per crate | done | `tests/it` in nrese-store, nrese-server, nrese-sparql, nrese-engine, nrese-rdf-io, nrese-shacl; the W3C and GeoSPARQL conformance suites stay binaries of their own |
| A3 `cargo-nextest` | deferred | It was for link time, which one binary per crate already removed; per-test processes aren't needed by any test today. Revisit if a test needs process isolation |
| Test-only code behind a feature | done | The reasoner's oracle (`v2::naive`) behind the `oracle` feature; `v2::testing` was production code and is `v2::vocabulary` |

### B. Typed boundaries

| Item | State | Notes |
|---|---|---|
| B1 Every store read takes a `ReadScope`, every write a `Requester` | done | `ReadContext` for statement reads; RDF4J statements streamed; requests carry no access fields; the pipeline checks every changed graph against the write scope |
| B2 Typed errors | done but one | Rule errors with line and column, `MemoryLimit`, `Forbidden(Refusal)`. `Configuration(String)` stays: nothing branches on it |
| B3 One configuration mechanism | done | A settings registry (file, environment, `--set`), JSON Schema (`config-schema`); the typed parsers stayed (decision recorded in the plan) |
| B4a The repository catalogue in the store | done | `nrese_store::catalog`, every repository including the default |
| B4b One authentication before handlers and bodies | done | Instead of a new `Operation` enum (authorisation already was one); public routes in one router |

### C. The query plan (with G4)

| Item | State | Notes |
|---|---|---|
| Design | done | [The query plan](plan/2026-10-02-plan-ir.md): a migration in steps |
| Algebra walker | done | `nrese_sparql_syntax::visit`; the executor's plain searches use it |
| Step 1: the logical plan | done | `nrese_sparql::plan`: built from the algebra and lowered back, the identity on every random query of the differential tests |
| Steps 2–4: rewrites, physical plan, executor on the plan | open | |
| `nrese-sparql` split, numeric promotion once | open | |

### D. Reasoning (with G3)

| Item | State | Notes |
|---|---|---|
| Inferences and graph access | open, needs a decision | Two designs on record: support graph sets (research designs §4/§6) and reasoning scopes (the audit plan); both change the engine's inferred stack |
| `v2` flattened, `nrese-core` folded in | open | |
| OWL 2 DL | open | [ADR-0009](adr/0009-owl2-dl-reasoning.md) proposed, awaiting the owner's review |

### E. Benchmarks

Open, in bulk at the end of a batch.

### F. Assurance

| Item | State | Notes |
|---|---|---|
| F1 `cargo-fuzz` targets for the parsers | open | |
| F2 Miri over the engine's unsafe modules, recurring on the office PC | open | |
| F3 Coverage measured once (`cargo-llvm-cov`) | open | |

## Engine gaps

| Gap | State | Notes |
|---|---|---|
| G1 Bug hunt | running | Random differential and property tests over many seeds (`scripts/fuzz-campaign.sh`). Fixed on 1–2 October: `ORDER BY` not total over terms of one canonical form (seeds 2329, 2458); a `LIMIT` cutting through equal sort keys; compact equality losing copies of a statement deleted from the default graph but asserted elsewhere (seeds 180, 192); path multiplicity between constants (seeds 1036, 1101, 1128); explanations differing between runs; the oracle's own mistakes (SAMPLE, CONSTRUCT with LIMIT, NOW, row order, GROUP_CONCAT seed 2051). Found by review on 2 October: sessions anyone could join (sequential ids, no owner); RDF4J computing graph access for the default repository instead of the one asked; request bodies read before authentication; ill-formed shapes stored; `SERVICE` open to every user; N-Triples `VERSION` lines panicking the line reader. Found by the Jena oracle once it compared again (it had compared nothing from 1 October): integer-derived literals outside their datatype's range (`"300"^^xsd:byte`) computed with as numbers (59 of 61 differences). Round 5 on the office PC (seeds from 3000) not yet reviewed (the machine is unreachable); a local run from seed 6000 is going |
| G2 One engine API | done but the browser part | Namespaces, sessions, repositories, imports as jobs, rematerialisation, explanations, running queries, graphs, users and workspaces, OpenAPI; implicit prefixes; shapes and user rules as checked managed objects; saved queries. Open: ResearchSpace's pages and forms in a browser (with the frontend) |
| G3 Reasoning that leads | open | Explanations through the API are done; inferences by premise graphs, OWL 2 DL and equality stage C are open (see D) |
| G4 Queries | started | A path bound at both ends follows from the smaller end. Join ordering across groups, characteristic sets, aggregates over products: open (with C) |
| G5 Vector search | open | |
| G6 Hardware and scale-out | open | |
| G7 Smaller items | started | RDF 1.2 `version` announced in results: done. Error recovery in the RDF/XML and JSON-LD parsers, multi-architecture images, missing benchmark kits, a second oracle, HTTP soak and fuzzing: open |

## Research designs

| Design | State | Notes |
|---|---|---|
| §1 Equality, stage B (`reasoner.equality = "compact"`) | done | Not yet: stable class ids, incremental merges and splits, late expansion (stage C), `strict`/`canonical` answers, bulk class building |
| §4 Access by named graph | done but inferences | Dataset restricted before evaluation, writes refused as a whole, caches keyed by access, policies in the store with history, `SERVICE` a privilege. Inferred statements are all-or-nothing per policy until support graph sets (see D) |
