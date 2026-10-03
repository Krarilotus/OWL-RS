# Status

The one place for the state of the work. Each item is **done** (implemented, tested,
in the branch), **deferred** (not done, with the reason and where it went), or **open**.
"Done" means done: what isn't, says so here.

- What the product can do, capability by capability: the
  [capability matrix](spec/06-target-capability-matrix.md).
- Why and in which order: [the roadmap](plan/2026-10-02-roadmap.md) (the order from 2 October on), and the plans it builds on ([audit work](plan/2026-10-02-audit-work.md),
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
| A3 CI at milestones | done | `ci.yml` and `oracle.yml` on `workflow_dispatch`; runs: 36998782678, 37007059759, and on the A+B milestone `04f36eb` CI 37021511088 and the oracle 37021515465 (all green). The oracle compared nothing from 1 October (renamed tests) until `1516dce`; it now fails on an empty comparison, and compares NRESE on the canonical copy too. On `04f36eb`, locally and in the workflow: 9,390 queries, 9,077 agree, 313 explained, none unexplained. Perf lab, `04f36eb` against `cd75751` (olympics, 10 M entities; two alternating rounds of five): same rows on every query, summed medians 1.02× and 0.99×, per-query differences within the rounds' spread. Merged into `main` as `c9ee847` (2 October): `04f36eb` with the review's R1–R4 cherry-picked (`merge/milestone-ab`, CI 37053802107 and oracle 37053806175 green), then `main` merged back into this branch |
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
| Step 2: rewrites on the plan | started | Named rewrites, in order, for queries and update `WHERE`s alike: `join-groups` (groups joined to each other become one basic graph pattern; inputs with an `ORDER BY` keep their place), `eager-aggregation`, `filter-pushdown`; EXPLAIN lists those that changed the query (`rewrites`). Open: pushdown on the plan itself, and the executor's match-guard strategies (paths from their bound end, limit placement, bind joins to `SERVICE`) as rewrites |
| Step 3: estimates on the plan, EXPLAIN before running | started | `plan_query` and `explain=plan`: the rewritten plan, each node with estimated rows from the store's statistics (triple patterns exact, basic graph patterns by the join orderer, the rest by documented rules in `native/estimate.rs`), without running; every random query of the differential tests is planned. Characteristic sets of the default graph (per read model; built at once up to 1 M statements, beyond on a thread of their own, kept until the size drifts by a quarter, none past 131,072 distinct sets) estimate stars in the join orderer and in EXPLAIN. Open: characteristic pairs (chains), the physical operator choices, estimates checked against actual rows over the benchmark queries |
| Step 4: executor on the plan | open | |
| `nrese-sparql` split, numeric promotion once | open | |

### D. Reasoning (with G3)

| Item | State | Notes |
|---|---|---|
| Inferences and graph access | done | `inferred = "supported"` in the access policy: a user who may read the default graph sees an inferred statement when one of its derivations uses graphs the user may read alone (support graph sets, decided 2 October). `nrese_reasoner::v2::graph_sets` computes the minimal sets per fact (annotated semi-naive over the ground program, schema premises and their alternative groundings included; equal to the closure of every set of graphs over 30 seeds × 150 random ontologies; LUBM(10) 1.1 s against 0.2 s for the materialisation). `nrese_store::support` computes them per revision on the first read that needs them, and each reader's view once: the snapshot with the invisible inferred statements removed (`Snapshot::with_inferred_subset`, tombstones or a stack of the visible ones, whichever is smaller), so queries, counts, sorted scans, statement reads, graph reads, pending session reads and explanations agree. `reasoner.support_sets` caps the sets per statement (default 16; fewer only hide; graphs in IRI order, so the cap is deterministic). Commits update the sets for what they can affect and each reader's view follows by a run per change stacked over the stack (LUBM(10), 8 graphs: first restricted read 2.4 s, after a one-statement commit 2 ms, after a thousand statements 53 ms). Tested in the store, through HTTP with the policy file and the commit path, and differentially (updated against computed afresh, in the reasoner and through the pipeline). The sets are prepared in the background at start, when the policy turns to `supported`, and after rematerialisations. Open: keeping the sets across restarts (now recomputed in the background), `compact` equality (its users see no inferences yet), explanations that prefer readable derivations |
| Faster deletes (provenance, [design](design/reasoner-provenance.md) step 1) | done | Recursion decided per ground key (predicate, or `rdf:type` with its class; equality's `sameAs` premise and class or property `sameAs` in the key graph). Overdeletion keeps a candidate that a non-recursive rule still derives in one step from what is left; that check and the B/F proofs of the rest run in parallel (a prover per worker). LUBM(10) deletes per commit: 1 → 0.9 to 0.4 ms, 100 → 15 to 1.9 ms, 1,000 → 111 to 6 ms, 10,000 → 134 to 13 ms (the rematerialisation: 225 ms); LUBM(100): 1,000 → 6.7 ms, 100,000 → 129 ms (2.3 s). Stored derivation counts (Hu, Motik and Horrocks) were built, measured slower at scale and removed |
| `v2` flattened, `nrese-core` folded in | open | |
| OWL 2 DL | open | [ADR-0009](adr/0009-owl2-dl-reasoning.md) accepted on 2 October and revised after seven research reports; the order is [the roadmap](plan/2026-10-02-roadmap.md), phases 2–4; `owl2-dl` always reports completeness, certain answers by default only where a complete path exists (decided 2 October) |

### E. Benchmarks

Open, in bulk at the end of a batch. Added on 2 October (the competitor check, `output/OWL-RS-rust-competitors-2026-10-02.md` outside the repository): the Rust stores with reasoning (sparq, open-ontologies, Open Triplestore) and the reasoners (reasonable, Nemo, VLog) run on the same machine as NRESE, result counts compared before times; the scorecard's Nemo time on LUBM(100) (319 s) checked against sparq's validated encoding (55 s) before anything is published.

### F. Assurance

| Item | State | Notes |
|---|---|---|
| F1 `cargo-fuzz` targets for the parsers | done but the long runs | `nrese-fuzz`: 12 targets (N-Triples, N-Quads, Turtle, TriG, N3, RDF/XML, JSON-LD, SPARQL query and update, results in JSON, XML, TSV), each checking no panic and that what parses writes back and reads back the same. On stable from the W3C suites and mutations of them (`cargo test -p nrese-fuzz`, in the fuzz campaign); as libFuzzer targets in `fuzz/` for `cargo fuzz` on nightly. About 27 M runs over 60 seeds found seven bugs (G1). Open: coverage-guided runs of hours on Linux (the office PC, with F2) |
| F2 Miri over the engine's unsafe modules, recurring on the office PC | open | |
| F3 Coverage measured once (`cargo-llvm-cov`) | open | |

## Engine gaps

| Gap | State | Notes |
|---|---|---|
| G1 Bug hunt | running | Random differential and property tests over many seeds (`scripts/fuzz-campaign.sh`). Fixed on 1–2 October: `ORDER BY` not total over terms of one canonical form (seeds 2329, 2458); a `LIMIT` cutting through equal sort keys; compact equality losing copies of a statement deleted from the default graph but asserted elsewhere (seeds 180, 192); path multiplicity between constants (seeds 1036, 1101, 1128); explanations differing between runs; the oracle's own mistakes (SAMPLE, CONSTRUCT with LIMIT, NOW, row order, GROUP_CONCAT seed 2051). Found by review on 2 October: sessions anyone could join (sequential ids, no owner); RDF4J computing graph access for the default repository instead of the one asked; request bodies read before authentication; ill-formed shapes stored; `SERVICE` open to every user; N-Triples `VERSION` lines panicking the line reader. Found by the check of the DL research reports: `/explain` ignored graph access (premises from unreadable graphs shown; an existence oracle for hidden statements) and took the writer slot to intern its rule constants; now scoped, with hidden steps, and lock-free. Found by the parser fuzz targets (F1): **a denial of service**, a 16 KB query of 1,500 chained `1 +` overflowing a 2 MiB thread's stack and taking the server down (`||`/`&&` chains now built balanced, arithmetic chains counted against the nesting limit); the RDF/XML reader taking any `xml:lang` as a language tag (`"bar"@es/wn`) and nodeIDs that are XML names but no blank node labels (`object.`), which no writer could write back; the SPARQL parser using a `BASE` with dot segments as written, and accepting a `GROUP BY` that binds a variable twice; the SPARQL writer printing queries that read back differently or not at all (nested `!` and `FILTER` brackets past the nesting limit, a blank node label spanning a FILTER before a path split into two groups). Found by the independent review of the A+B milestone (`cd75751..04f36eb`): long-list consistency rules piling up in the delta executor's program with every commit that touches a list (R1; the index compared by pointer); local logins lockable per user name by anyone, their failure counts unbounded, and their timing telling which names exist (R2–R4: now counted per name and client address with backoff and a per-address limit, bounded, with a dummy Argon2 check). Found by the Jena oracle once it compared again (it had compared nothing from 1 October): integer-derived literals outside their datatype's range (`"300"^^xsd:byte`) computed with as numbers (59 of 61 differences). Round 5 on the office PC (seeds from 3000) not yet reviewed (the machine is unreachable); locally, seeds 6000–6643: no failures |
| G2 One engine API | done but the browser part | Namespaces, sessions, repositories, imports as jobs, rematerialisation, explanations, running queries, graphs, users and workspaces, OpenAPI; implicit prefixes; shapes and user rules as checked managed objects; saved queries. Open: ResearchSpace's pages and forms in a browser (with the frontend) |
| G3 Reasoning that leads | open | Explanations through the API are done; inferences by premise graphs, OWL 2 DL and equality stage C are open (see D) |
| G4 Queries | started | A path bound at both ends follows from the smaller end; groups joined to each other are ordered as one pattern; stars estimated from characteristic sets; eager aggregation (`COUNT`, `COUNT(*)`, `SUM` over a join aggregate the side they read first: BSBM BI q4's shape, to be measured with the BI run). Open: `MIN`/`MAX`/`AVG` in eager aggregation, paths planned with the rest, characteristic pairs (with C) |
| G5 Vector search | open | |
| G6 Hardware and scale-out | open | |
| G7 Smaller items | started | RDF 1.2 `version` announced in results: done. Error recovery in the RDF/XML and JSON-LD parsers, multi-architecture images, missing benchmark kits, a second oracle, HTTP soak and fuzzing: open |

## Next batch

From the milestone review (R5–R9), low priority:

| Item | State |
|---|---|
| R5 The verified-credential cache keyed by a keyed hash (HMAC, a per-process key) instead of SHA-256 | done (HMAC-SHA-256 over `sha2`, checked against RFC 4231) |
| R6 Repository ids compared case-insensitively; Windows device names refused | done (also leading and trailing dots) |
| R7 Removing a repository by renaming it to `.trash-*` first, swept at start | done |
| R8 Long `owl:AllDisjointProperties` (a self-join of the triple table): a note or a time-bounded test | done (the note, in reasoning-semantics.md); a native rule driven by the index's members would remove the cost: open, with G3 |
| R9 `--set` refused or warned for secret settings | done (refused, pointing to the file or the environment variable) |
| R1 residue: a commit that adds a long list without deleting one keeps the older index rule beside the new one until the program is rebuilt (sound; bounded by list changes, not by commits) | open |

## Research designs

| Design | State | Notes |
|---|---|---|
| §1 Equality, stage B (`reasoner.equality = "compact"`) | done | Not yet: stable class ids, incremental merges and splits, late expansion (stage C), `strict`/`canonical` answers, bulk class building |
| §4 Access by named graph | done but inferences | Dataset restricted before evaluation, writes refused as a whole, caches keyed by access, policies in the store with history, `SERVICE` a privilege. Inferred statements are all-or-nothing per policy until support graph sets (see D) |
