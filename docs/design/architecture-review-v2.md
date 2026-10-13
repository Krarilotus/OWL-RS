# Architecture review before v2 (12 October 2026)

An independent review of the core: can it carry v2.0 (OWL 2 DL, SPARQL 1.1/1.2, the
hardware round, distribution, compaction), v2.1 (property graphs) and the products that
will be built on it in other repositories? The evidence is the code at `9d5a65fd`, cited
by file and line. No code was changed and nothing was built.

## Executive summary

1. **The core is sound and worth building on.** Layers point down, the engine is one LSM over `[TermId; 4]` with MVCC, and the reasoner and executor work on ids. No core rewrite is needed for v2.0 or v2.1.
2. **Scale: one place decides it, the term identity.** Dictionary ids are a dense, local, insertion-ordered sequence that the WAL replays (`dictionary.rs:14-16`, `codec.rs:6-14`). Reserving partition bits in the id payload now costs nothing on disk. Changing ids later means re-encoding every store.
3. **Sharded writes and distributed query are not a v2.0 item.** They need a distributed commit, exchange operators and a global term identity. v2.0 should add the seams and replica failover, not sharding.
4. **Property graphs: the RDF 1.2 reifier mapping fits the shapes** (edge properties, multi-edges, labels). But the executor reaches a reifier's triple only by scanning every `rdf:reifies` statement and decoding it (`triple_terms.rs:1-11`, `expr.rs:650`), so one hop costs O(all edges). Id-level triple-term probes are v2.0 work.
5. **The executor still interprets SPARQL algebra** (`native/mod.rs:1-12`). For GQL and openCypher to compile "to the same algebra", v2.0 has to declare that algebra (the logical `Plan`) the language-neutral contract, with an `evaluate_plan` entry point and a front-end extension point.
6. **Public API: not yet a product API.** It has a workable HTTP v1 with OpenAPI. On the Rust side there is no façade: `nrese-store` exports about 500 public items, lower-layer types leak, and public write methods skip every gate (`service.rs:1203-1216`). The licences are fine: permissive only, enforced by cargo-deny.
7. **Layering is mostly clean.** The violations are small: the server builds the reasoner, process-wide globals, a stray `env` read, an EL classifier that duplicates `nrese-dl`, and a stale ownership row. The biggest compaction wins are the 5.6k-line executor module and the store façade.
8. **v2.0 needs about 4–5 focused developer-weeks** of core work beyond the open merge checklist (§5). That is more than one week for one person, but it fits a week with 4–5 people working in parallel, because the items are largely independent. v2.1 (GQL plus an openCypher subset, paths, updates, algorithms, SNB) is about 6–9 developer-months. Gremlin and SQL/PGQ come on top of that.

## 1. Scale: several machines

**What carries over.** Inline values (integers, decimals, dates, booleans) are globally
canonical and need no dictionary (`term/mod.rs:42-71`). A `Snapshot` is a cheap, immutable
`Arc` of the runs plus revision (`snapshot.rs:30-39`), so it already serves as a read epoch on one
node. The reasoner reads through traits (`nrese-reasoner/src/eval.rs:52` `Source`,
`delta.rs:51` `Base`), so a partitioned source is possible without touching the rules. The
WAL ships byte-identical frames to replicas (`engine/replication.rs:44,112`), so failover and
re-bootstrap are operations work, not core work.

**The one place that has to change: term identity.**

- A dictionary-backed `TermId` is a 4-bit kind plus a 60-bit payload. The payload is the
  dictionary index (`term/mod.rs:31`).
- Ids are "dense and assigned in insertion order, which is what the write-ahead log relies
  on" (`dictionary.rs:14-16`). Each WAL record carries its new keys as one contiguous range
  from `dictionary_start`, and its quads as raw ids (`codec.rs:6-14, 29-38`).
- A raw `u64` therefore means something only next to its own dictionary. The design doc
  says the same (`hardware-and-scaling.md:96`).

Two shards would give the same term different ids, and a shared sequencer would turn
interning into a cluster-wide single writer.

- **Fix:** each dictionary term gets a *home partition*, and the partition is encoded in
  the high payload bits, for example 8 bits of partition and 52 bits of local sequence
  (4.5·10¹⁵ terms per partition).
- **Why now:** with one partition every existing id already has partition 0. Reserving the
  bits is an invariant, an assertion and a helper that masks the index. It needs no format
  change.
- **Why not later:** once ids exceed 2⁵² or the bits are used for something else, the same
  change means re-encoding the WAL, checkpoints, derived indexes and caches. The project
  has done that three times already (`wal.rs:24-33`).
- **Blank nodes:** they need the same home-partition rule.

**What else stays single-node by design:**

- *Writes:* one writer slot per engine (`engine/mod.rs:653`), with a local WAL as the
  commit point.
- *Planning and execution:* the executor walks algebra and has no exchange operator
  (`query-plan.md:3-11`).
- *Inferences:* the inferred stack holds default-graph triples only
  (`engine/mod.rs:89`, `codec.rs:12`, `transaction.rs:386`).

Sharding needs subject-owned shards with all permutations, a coordinator for atomic commits,
exchange operators with global completion rules (ORDER BY, DISTINCT, aggregates, OPTIONAL and
MINUS), and distributed fixpoints for reasoning. The design doc already says this
(`hardware-and-scaling.md:94-98`, `research-designs.md:237-239`). It is a project of
2–3 developer-months after H1–H4. Nothing in the core blocks it, provided term identity is
settled first.

**Recommendation for v2.0:**

- the id-layout ADR (home-partition bits, and a budget for the three free kind tags);
- WAL records with tagged, skippable sections;
- remote deadlines passed as durations;
- replica failover (G9).

Sharding stays a v3-class project unless the owner decides otherwise (§6).

## 2. Property graphs: what v2.0 must get right for v2.1

### 2.1 The mapping against the workloads

Mapping: an edge `(a)-[:KNOWS {since}]->(b)` becomes `a :knows b`,
`e rdf:reifies <<( a :knows b )>>` and `e :since …`. Labels become `rdf:type`, property
keys become IRIs, and values become XSD literals.

| Workload shape | Fits the mapping? | What the core lacks |
|---|---|---|
| Node properties, multiple labels | yes (plain triples, `rdf:type`) | nothing |
| Edge properties (SNB `knows.creationDate`, `hasMember.joinDate`, `studyAt.classYear`) | yes: triples on the reifier | **access path:** a pattern `<<( :a :knows ?b )>>` is rewritten into a scan of every `rdf:reifies` statement plus `SUBJECT()`/`OBJECT()` on decoded terms (`triple_terms.rs:1-11`, `expr.rs:650-661`). There is no id-level probe from components to the triple term: `lookup_bounded` takes a `TermRef` (`dictionary.rs:749-770`), and the only component index, `Dictionary::triple_terms`, is a linear filter with no callers (`dictionary.rs:815-850`). Cost: O(all edges) per hop |
| Multi-edges | yes: several reifiers of one triple term | **update semantics:** the base triple is a set member. Deleting one edge must keep it while another reifier remains, and a SPARQL delete of the base triple leaves dangling reifiers. The mapping ADR has to define the invariant, and the pipeline enforces it |
| Undirected `knows`, either-direction match | yes (SPOG and OSPG) | nothing |
| Variable-length paths `*3..4` (BI 10), shortest paths (IC13), weighted shortest paths (IC14, BI 15/19/20) | **no**: SPARQL paths return sets of end pairs, with no path variable, no edge filter inside the path, and no shortest or weighted search (`native/paths.rs:1-10`) | path operators with bindings, quantifiers and path modes (trail, acyclic, shortest); path values in results |
| Lists, maps, `collect()`, NULL | partly: absence models NULL; lists and maps have no RDF value | composite values as computed ids (the `COMPUTED_TAG` range exists, `nrese-exec/src/lib.rs:23-25`, `:52`) and a literal form for storing them |
| 64-bit integers, doubles, temporal values | correct; values beyond 60 bits and all doubles live in the dictionary (`inline.rs:1-20`) | performance only. An inline double would use one of the 3 free tags |
| SNB Interactive updates (inserts, cascading deletes) | through the mutation pipeline | `MutationCommand` is a closed per-protocol enum (`mutation/command.rs:16-24`); throughput is bound by one writer and one fsync per commit (group commit is only designed, `research-designs.md:226`) |
| Graphalytics (BFS, PR, WCC, CDLP, LCC, SSSP) | not queries: whole-graph kernels | a CSR projection over a snapshot. `nrese-exec::graph` already has adjacency, SCC and closures |
| Reasoning over the property graph | only if the graph lives in the default graph (the inferred stack is default-graph only) | a decision (§6) |

### 2.2 v2.0 must provide (otherwise v2.1 reworks the core)

1. **Id-level triple terms (engine and executor).**
   - Engine: `Snapshot::triple_term(s, p, o) -> Option<TermId>` (a hash probe with an
     id-built key, `dictionary.rs:1794`) and `components(TermId) -> [TermId; 3]`, both
     without decoding.
   - Executor: patterns `<<( s p o )>>` with bound parts probe these instead of scanning,
     and `SUBJECT`/`PREDICATE`/`OBJECT` work on ids.
   - Clean-up: delete the unused linear `triple_terms`.
   - A hop then costs three probes per edge. SPARQL 1.2 (a v2.0 goal) needs this anyway.
2. **A language-neutral executor contract.** An ADR declares that the logical `Plan`
   (`nrese-sparql/src/plan/mod.rs:32-102`) is the input every front-end lowers to.
   - SPARQL is one front-end. New node and value kinds (paths with bindings, composite
     values) are added as `Plan`/algebra variants, not as a second executor.
   - A public `evaluate_plan` takes a `Plan` built without SPARQL text. Today every entry
     point takes `&nrese_sparql_syntax::Query` (`query.rs:106-112`).
   - Lowering already exists (`Plan::into_pattern`), so the entry point is small. Finishing
     physical-plan execution is *not* required for v2.0.
3. **A front-end extension point for writes.** A mutation command that carries any
   front-end's delta planner, applied to the transaction with the requester and update
   context. Then GQL `CREATE/SET/DELETE` goes through the same gates without another enum
   variant per language.
4. **One write path in the public API** (§3). PG updates have to reach the pipeline.
5. **Extensible storage sections.** WAL records and checkpoints have fixed sections for two
   stacks (`codec.rs:6-14`), and `Stack` is a closed two-variant enum
   (`engine/mod.rs:131`).
   - With tagged, skippable sections, v2.1 can add an edge-occurrence stack
     `[s, p, o, reifier]` without a format break. The run, merge and compaction machinery
     is already generic over `[TermId; 4]` and a `Layout` (`index/mod.rs:48-57`).
   - Named-graph inferences would use the same sections. Whether the occurrence stack pays
     is measured in v2.1 on SNB.
6. **The id-layout ADR** (§1), which also allocates the three free kind tags (13–15) so
   that v2.1 does not fight over them.

### 2.3 v2.1 work, estimated

Estimates are in focused developer-weeks for one experienced developer. Agents parallelise
the work, but oracles and review bound it.

| Item | Effort |
|---|---|
| Mapping ADR: edges, multi-edges, labels, lists and maps, NULL, the base-triple invariant, LPG import (Neptune CSV, Neo4j CSV, GraphML) | 1–2 w (+2 w importers) |
| ISO GQL parser to `Plan` (the read core, `MATCH`, quantified paths, `RETURN`, aggregates) | 5–8 w |
| openCypher subset over the same front-end | 3–5 w |
| Path operators: bounded BFS with edge filters, bidirectional shortest path, weighted shortest paths, path values; a reference evaluator for differential tests | 5–7 w |
| Composite values in id tables and results; a GQL results format (Bolt is a separate decision) | 2–3 w |
| Updates through the pipeline (`CREATE`, `MERGE`, `SET`, `DELETE`, `DETACH DELETE`) | 2–3 w |
| Graph algorithms over a CSR projection (Graphalytics' six) | 3–4 w |
| LDBC SNB Interactive and BI drivers, validation against the reference results | 2–3 w |
| **Total for v2.1 as scoped above** | **about 6–9 developer-months** |
| Gremlin (a step-based traversal machine; a different front-end shape) | +2–3 months |
| SQL/PGQ (requires a SQL front-end, which does not exist; overlaps G10) | +3 months |

## 3. Building on top: public API, extension points, licences

**HTTP: good.** One versioned API, `/api/v1/…`, with a generated OpenAPI 3.1 document, a
deprecation policy and problem+json errors (ADR-0007, `http/api_v1.rs:1-4`). The standard
protocols (SPARQL, GSP, RDF4J) are covered. Products that integrate over the network can
start on this today.

**Rust: no stable surface yet.**

- **Not distributable:** every crate is `publish = false` at version `0.1.0`
  (`Cargo.toml:29-32`), so products in other repositories depend on git revisions with no
  semver.
- **No façade:** the façade crate (H7, `hardware-and-scaling.md:196-197`) does not exist.
  To embed the store, a product does what the server does: it builds a `ReasonerService`
  from `nrese-reasoner` and passes it to the store's `MutationPipeline`
  (`main.rs:223`, `mutation/pipeline.rs:61`).
- **Leaky surface:** `nrese-store` has about 500 public items, and `StoreService` alone has
  about 70 public methods (`service.rs`, 1,614 lines). Engine and SPARQL types leak through
  it (`read_snapshot -> nrese_engine::Snapshot`, `service.rs:397-402`;
  `use_reasoning_rules(nrese_reasoner::RuleProgram)`, `service.rs:451`).
- **Ungated writes:** `StoreService::apply`, `execute_update`, `execute_graph_write` and
  `execute_graph_delete` commit with no gates, as `Requester::all()`. Reasoning is
  invalidated instead of maintained, and SHACL is skipped (`service.rs:1203-1263`; the
  comment at `:1219` says they are "the server's own and tests'"). For an external product
  this is a correctness trap.
- **Process-wide globals:** several settings are global to the process, not to an engine:
  `set_index_encoding` (`index/keys.rs:66`), `set_vocabulary_encoding`
  (`term/vocabulary.rs:50`), `set_process_limit` (`nrese-exec/src/memory.rs:62`) and
  `set_release` (`nrese-engine/src/memory.rs:22`). Two embedded stores cannot differ in
  these settings.

**Extension points:**

| Point | Today | Needed for products |
|---|---|---|
| Query front-ends | SPARQL only; entries take `&Query` | `Plan` contract plus a front-end trait (v2.0, §2.2) |
| Functions | `Function::Custom` hard-coded (GeoSPARQL `geof:`, casts; `native/expr.rs:486-490`) | a registry of id/value functions by IRI, with purity and cost declared (LLM and embedding functions). v2.0 |
| Indexes | text and vector are built into the dictionary (`Dictionary::text_search`/`vector_search`, `dictionary.rs:866-947`); derived-index files are generic (`term/derived.rs:1-13`) | a derived-index trait over dictionary terms (vectors, text), then one over statements. v2.x |
| Federation | `ServiceClient` trait (`nrese-sparql/src/service.rs:32`) | fine |
| Reasoners | closed `ReasoningMode`; user rules in N3 | fine for v2. A pluggable reasoner is not needed |
| Commit gates | reasoning and SHACL, fixed (`mutation/pipeline.rs`) | a gate trait (custom validation). v2.x |
| Change feed | raw WAL frames with ids (`replication_log`, `service.rs:1138`) | a decoded change stream (data integration, RAG indexing). v2.x |
| Storage | one concrete `Engine` and `Snapshot` | correct: no backend trait; the cost is per-row dispatch |
| Auth | closed `AuthConfig` enum (`nrese-server/src/auth/mod.rs:21`) | an identity-provider trait. v2.x (G9: LDAP, SAML, SCIM) |

**Licences: no obstacle.** `deny.toml` allows only permissive licences (MIT, Apache-2.0,
BSD, ISC, Zlib, Unicode, CC0, BSL, CDLA-Permissive), has no MPL exceptions, and denies
unknown sources. NRESE is MIT OR Apache-2.0. The notable dependencies are permissive:
`fsst-rs` (Apache-2.0), `geo`, `rstar`, `wkt`, `rust-stemmers`, `ring`, `rustls`,
`webpki-roots` 1.x (CDLA-Permissive-2.0). Products can be built on it under any licence,
closed ones included. Keep cargo-deny in the gate, and give the console's npm dependencies
the same check if it ever ships inside a product.

## 4. Ownership and layering

Checked against `ARCHITECTURE.md` §2–§6 and `AGENTS.md`.

| # | Finding | Evidence | Fix |
|---|---|---|---|
| L1 | Public write paths bypass the mutation pipeline, the declared single owner of write semantics (`ARCHITECTURE.md:63-71`) | `service.rs:1203-1263` | make them crate-private or test-only; the server's own writes go through the pipeline |
| L2 | The server constructs the reasoner and holds it in its state (L4 reaching past L3); the store's catalog also constructs reasoners per repository, so there are two construction paths | `state.rs:4,40-47`, `main.rs:223`, `catalog.rs:170-173,336` | the store builds it from typed config; the server passes config only |
| L3 | Users, passwords (argon2) and login live in the store, but the ownership table says the store must not own auth | `access/mod.rs:1`, `nrese-store/Cargo.toml:28`, `ARCHITECTURE.md:51` | ADR-0008 decided this; update the table: the store owns authorisation state, the server owns authentication backends |
| L4 | Hidden process-wide globals, against §6.2 "no hidden globals" | §3 list | move them into `EngineConfig`/`StoreConfig` |
| L5 | `std::env` read outside `nrese-server/src/config/` | `nrese-dl/src/classify/mod.rs:235`, `nrese-reasoner/src/pie.rs:707` | pass them as options, or allow-list debug tracing explicitly |
| L6 | An L1 crate depends on an L2 crate (dev-dependency, for an example) | `nrese-dl/Cargo.toml:27` (`examples/context_classify.rs:191-237`) | goes with D1 |
| L7 | The engine depends on `nrese-rdf-io` only to parse legacy triple-term keys | `dictionary.rs:1813-1824` | parse with `nrese-rdf`'s term parser, or migrate the legacy keys when the store opens |
| L8 | Stale comment about a "v1 reasoner" that reads the whole dataset | `mutation/pipeline.rs:27` | delete |

**Duplicates:**

- **D1.** The OWL 2 EL classifier in `nrese-reasoner/src/classify.rs` (1,209 lines, its own
  normaliser "as CEL and ELK do") duplicates `nrese-owl`'s normaliser and `nrese-dl`'s
  context core. Its only user is a `nrese-dl` example. Delete it, or move it into test
  oracles.
- **D2.** Equality has three owners: the rules' representatives, the store's compact mode
  and the tableau's merges. This is known and in progress (ADR-0011, G7); keep it on plan.
- **D3.** The `Dictionary` owns term encoding *and* the orchestration of the text and
  vector indexes (2,051 lines). Split the secondary indexes into a registry next to it.
  That split is also the index extension point.

**Compaction, largest payoff first:**

1. `nrese-sparql/src/native/mod.rs` (5,556 lines): split it by operator as the executor
   moves onto `Plan` (query-plan.md's migration). Doing it before v2.1 adds path operators
   makes that addition local.
2. The store façade: `pub mod` changed to `pub(crate)` where only the server uses a module,
   and one narrow `Store` type. This removes most of the 500 public items from the
   contract.
3. D1 (about 1.2k lines plus tests) and L7 (one dependency edge).
4. The AI suggestions in the server (`nrese-server/src/ai`, 540 lines): the LLM products
   live in other repositories after v2, so move this out with them.

## 5. The plan

**v2.0, must** (they shape the core, are cheap now and expensive later). "d" means a focused
developer-day.

| # | Item | Effort | Done when |
|---|---|---|---|
| 1 | Id-layout ADR: home-partition bits in dictionary payloads, the tag budget, blank-node ownership | 1–2 d | ADR merged; assertion on dictionary growth beyond 2⁵²; a helper replaces raw payload-as-index uses; test |
| 2 | Id-level triple terms (§2.2.1) | 3–4 d | a guard test: `<<( :a :p ?b )>>` with `:a` bound decodes 0 terms and probes O(edges of `:a`); differential test against the reference; performance.md §0 guard |
| 3 | `Plan` as the executor contract; public `evaluate_plan` (§2.2.2) | ADR 1 d + 2–3 d | a test builds a `Plan` without SPARQL text and equals the reference; the GQL lowering target is named in the ADR |
| 4 | One gated write path (L1) plus the front-end mutation extension point (§2.2.3) | 2–3 d | no public method commits outside the pipeline (a grep test in the gate); server and test writes migrated |
| 5 | Façade crate `nrese` over store and pipeline: open, load, query or plan, update, validate, reason, explain, stats; boundary types only; the server builds through it | 5–8 d | the server and one external example crate compile against `nrese` alone; a written semver and deprecation policy; a release tag |
| 6 | WAL and checkpoint sections tagged and skippable (§2.2.5) | 2–3 d | WAL v7 with an upgrade test from v6; a test-only extra section round-trips through replay, checkpoint and replication |
| 7 | Process globals into config (L4); `env` reads (L5) | 1–2 d | two engines with different encodings in one process (test) |
| 8 | Small layering fixes L2, L3 (docs), L6–L8, D1 | 2–3 d | the dependency graph matches `ARCHITECTURE.md` §2; the gate is green |

That is about 4–5 developer-weeks in total, in parallel across about 4 owners: engine
(1, 2, 6, 7), sparql (2, 3), store (4, 5, 8), docs and ADRs (1, 3, L3). This is in addition
to the open rows of the merge checklist (DL classification, test cleanup, kernels, equality,
the fast suite).

**v2.0, should** (no rework if they slip):

- a function registry;
- replica failover (G9);
- splitting `native/mod.rs` along the operators already migrated;
- moving the AI module out of the core.

**v2.1:** everything in §2.3. First the mapping ADR and the SNB harness, so that every later
step is measured. The occurrence stack is built only if SNB shows that the three-probe hop
is the bottleneck.

**Later (v2.x/v3):** sharding (2–3 months, after H1–H4 and item 1); group commit; the
decoded change feed; the gate and identity-provider traits; statement-level index plugins;
Gremlin; SQL/PGQ together with G10's SQL endpoint.

**Risky:**

- **Item 3 grows into "finish the physical plan".** Keep it to the contract and the entry
  point; the migration stays incremental (query-plan.md).
- **Item 5 freezes a surface too early.** Mark the façade `0.x` with an explicit
  stability list; products pin a release.
- **Item 6 is an on-disk format change.** The upgrade fixtures of `v2/upgrade-path` must
  cover it.
- **v2.1 path semantics:** GQL's path modes and NULL handling against SPARQL's error and
  unbound semantics need a reference evaluator first, like `nrese-sparql-reference`.
  Otherwise there is no correctness oracle.
- **Calendar:** "the core done by next week" holds for the must-list only if 4–5 people
  work in parallel. The DL rows of the merge checklist are on another path and set the date
  just as much.

## 6. Decisions for the owner

1. **Distribution in v2.0:** seams plus replica failover (recommended), or sharded writes and
   distributed query (2–3 months more, and it delays v2.0)?
2. **Where property graphs live:** in the default graph, so that the reasoner applies
   (recommended), or in named graphs per graph, which needs named-graph inferences
   (a format change, so better decided before item 6 closes)?
3. **The scope of v2.1's languages:** GQL plus an openCypher subset (recommended); Gremlin,
   SQL/PGQ and Bolt each as a separate later decision.
4. **The Rust API promise:** a `0.x` façade with a stability list in v2.0 (recommended), or
   crates published to crates.io.
5. **The AI assistant:** stays in the core server, or moves to the product repositories
   (recommended).

### Decided by the owner (13 October 2026)

1. **Distribution:** v2.0 builds the foundation; distribution follows on it, in two tracks:
   - containers in the cloud, against the managed graph services;
   - volunteer computing across end-user machines worldwide (the Folding@home model).

   Both are informed by distributed-systems and parallel-computing research; the
   estimate is to be checked against it. Specific hardware is optimised wherever it
   helps. The direction is NRESE as a distributed world memory for AI agents (user story
   to be derived). Re-encoding stores is no concern before the first release, so the id
   layout follows the design, not compatibility.
2. **Property graphs:** in the default graph, so the reasoner applies, with every synergy
   between property graphs and reasoning used. Before the format is fixed, the
   named-graph option is to be evaluated as a mapping onto one physical layout (graph ids
   carried by inferred statements; per-dataset reasoning scope as a setting) rather than
   a separate format, for maximal flexibility.
3. **Languages:** the newest standards of every query language, each for its use cases:
   GQL, openCypher, Gremlin, SQL/PGQ, Bolt and SPARQL. Each at the best speed and
   memory footprint.
4. **Interfaces:** a clean Rust interface, defined feature-complete first, plus HTTP and
   language bindings (Python and others) for using the store. Publishing crates on
   crates.io is decided at v2.3, once the specification and its first implementation
   exist.
5. **AI:** AI first. Agents are a service layer, not one assistant. The core exposes what
   agents need (introspection, explanations, provenance, cost estimates, a function
   registry for LLM and embedding functions), and the LLM client leaves the server
   binary for that service layer.

**Rules (owner):** user rules grow into full logic programming in v2.2: n-ary relations,
negation, arithmetic, aggregation, and what constraint logic programming and Prolog
offer, across the standards (N3, SWRL, RIF, SHACL rules, Datalog). It is built on the
whole stack, so its synergies compound, and extensions such as macros or unification are
proposed where they serve users. **Every version from v2.1 to v2.3 ends with an
optimisation round.**

### Further decisions (owner, 13 October 2026, later)

- **Named graphs: yes,** as one physical layout. Inferred statements carry their graph id,
  the reasoning scope is a per-dataset setting, and property graphs go in the default graph
  or their own graphs as the use case needs.
- **Distribution at the macro level:** named graphs, the scheduler and the query planner's
  splitting make independent units of work (reasoning per graph scope, query parts per graph)
  that can run on different machines without a distributed commit. The aim is to split long
  jobs across machines, not to shave seconds. Fine-grained distribution can follow later.
- **Modular installation:** NRESE ships as selectable components. A small installation
  serves where space or compute is limited, and components can be added later.
- **A transformation layer in the specification:** every storage-format change comes with a
  specified transformation from the previous format. That is how components are added later,
  and it settles backward compatibility.
- **AI as a modular service,** split off from the core; its design waits for dedicated
  research on how it connects with real-world use.
- **Adoption:** a usability and connection layer good enough for quick adoption is a goal of
  its own, beside the papers and the benchmark lead.
