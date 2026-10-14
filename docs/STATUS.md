# Status

The one place for the state of the work. Each item is **done** (implemented, tested,
in the branch), **deferred** (not done, with the reason and where it went), or **open**.
"Done" means done: what isn't, says so here.

- What the product can do, capability by capability: the
  [capability matrix](spec/06-target-capability-matrix.md).
- Why and in which order: [the roadmap](ROADMAP.md); the designs in [design/](design/).

What is left before v2 goes to `main`, with owners and state, is the status table of the
[merge checklist](plan/2026-10-05-v2-merge-checklist.md) §1; the sections from "Audit
work" on are the record by goal.

## Execution and performance (PR #23)

The PR's implementation base is `0f46563`. The measurements are rows of the
[lab log](design/performance.md#6-lab-log-of-the-performance-phase-from-5-october-2026),
each with its run record.

| Area | Implemented | Open |
|---|---|---|
| QL preparation and cache identity | One preparation per execution snapshot, its report kept with the result; compiler input identified by the exact schema; failed probes never cached as negative answers | Schema reading and rewriting don't poll cancellation |
| Cancellation and deadlines | A request's flag reaches nested DL work; one absolute deadline covers pending and prior consistency checks and rejection evidence | Normalisation and compilation don't poll |
| Execution | Queries run on their caller; parallel kernels on Rayon's global pool; one catalog-wide native query budget; `Workers` sets only the DL batch width; one RAII reservation type; a Rayon worker never waits on a cache flight, it computes the part itself (the bounded rule, [decided](design/research-designs.md#8-query-progress-under-shared-cache-waits-decided-the-bounded-rule) 12 October) | The continuation design, only if embedding needs it |
| Context task memory | Optional owner-based capacity estimates sampled at work checkpoints, incremental index totals, message payloads that carry their charge; `dl.memory = 0` is unlimited; within a request, reserved from its envelope | Can overshoot; excluded allocations |
| Atomic reasoning updates | Split and revival closure run in the pending transaction and its gates; the post-commit repair is gone | The whole-dataset fallback holds the writer slot (unmeasured; `eq-giant-split` times it); incremental split maintenance |
| Kernels and allocation | Moving projection, shared forward search, direct raw spill, ordered dedup without a sort; one SPARQL numeric implementation | Work and copy savings carry no end-to-end timing claim |
| DL bounds and exact work | ID-level results with dictionary checks; a true lower ASK skips the upper evaluation; one compiled program per candidate batch; the bounds, the gap, compilation scratch and the engines in the request's one envelope | Base search and compilation repeated between batches; the upper table is materialised |
| DL classification and realisation | The initial search kept as Base; one fallback allowance; realisation reuses the deterministic start; only complete results cached | Stages compile separately; cooperative checkpoints; partial aggregate counters |
| Process memory | The server installs the process limit; store-local watches reach bounds and diagnostics; one envelope per request within the catalog's budget (queries, updates and their DL gate, DL entailment and explanations) | Cooperative only; a request's envelope and process memory are separate contracts; classification and realisation (shared per revision) are under `dl.memory` and the process limit, not a request's envelope |
| Benchmarks | Fast suite with exact counters and QPS per width; [hosts.toml](../benches/hosts.toml); one run-record schema; the answer recorder (strict and value matches, legal `LIMIT` subsets) | One case registry, one result row and one `compare` ([benches/README.md](../benches/README.md#where-this-stands)); checkpointed level-3 chunks |

### Open work

- **Measured recovery, integrated acceptance still open:** the later Horn comparison
  against `0f46563` records +1.3 % serial and -2.1 % at 16 threads (within the reported
  build-to-build spread), with all 72 taxonomies byte-exact
  ([run record](../benches/runs/2026-10-12-horn-takeover-dl.toml)). The 95-case PR comparison
  found no regressions beyond its thresholds for `157931a` versus `aabd47a`
  ([record](../benches/runs/2026-10-12-pr23-acceptance-ab-phuoc.toml)); it does not certify
  the current integrated revision against `0f46563`. Its "same" verdict permits small
  slowdowns, and its cgroup peaks include page-cache charges (see the
  [memory investigation](../benches/runs/2026-10-12-memory-variance.toml)).
- **Resource ownership in `nrese-dl`:** the envelope needed and has: an engine's budget is
  an owned value (no longer `Copy`), its observer and the tableau reserve from the request's
  envelope and return it when the engine drops, and exhaustion stays sticky per engine (an
  envelope's refusal included). `context::Engine::with_budget` no longer exists.
  `Individuals` borrows its engine and returns its retained charge on drop, including
  unwinding; its context list is read-only. Open: a result retained beyond its request
  keeps its
  engine's reservation (request-scoped saturations are dropped at the request's end;
  cached classification runs without an envelope); changed policy and historical peaks
  need an API contract first.
- **Performance acceptance of v1.5's robustness:** the A-B-B-A `fast.py compare` of the
  envelope and the bounded rule on the affected cases (queued on office-pc; lab log, 12
  October).
- **Vector search:** the filtered search that goes first widens its beam by the joined
  pattern's share (12 October): `vector-knn-filtered`'s recall 0.56 → 0.98, its gate passes
  ([record](../benches/runs/2026-10-12-vector-filtered-fix-phuoc.toml)). On its uniform 64-d
  vectors the graph search costs what the exact scan does (10.5 against 10.4 ms).
  The operation-contracts branch implements a cost-routing candidate, with separate
  index-filter and later-probe estimates and a forced-ANN guard; its same-host A/B is
  still required before acceptance ([design](design/execution-core.md#vector-join-direction-and-physical-search)).
  The graph's own recall at the default beam (0.44) remains open.
- **Competitor answers:** rerun with the committed recorder to decide the unordered
  `LIMIT` case; Konclude intermittently drops 107 asserted pairs from the Horn reference
  taxonomy, cause unknown.
- **Measurement:** container lifetime peaks (cgroup) on office-pc need system-manager
  permissions; a large-answer recorder that streams instead of retaining; the result
  cache's overhead (keys, ranking, ID copies, encoding) is unattributed.
- **Later structure:** the physical plan executed directly ([query-plan.md](design/query-plan.md)),
  calibrated routing, NUMA, GPU and distribution ([hardware-and-scaling.md](design/hardware-and-scaling.md)).
- **GraphDB:** 1M/10M writes, sustained timing and incremental reasoning
  ([coverage](../benches/runs/2026-10-09-graphdb-coverage.toml)).

## OWL 2 DL (v1.5, branch `v15/dl-complete`, 12 October)

**Implemented coverage.** The engines have decision paths for the OWL 2 DL constructs
covered by the campaigns below. Those finite campaigns are strong evidence, not a proof
of correctness or termination for arbitrary ontologies. The documented boundaries
include unsupported input, resource limits and reader diagnostics
([design](design/owl2-dl.md) §3, §6):
- *ruled out by OWL 2 DL, answered `unsupported`:* `owl:topObjectProperty` where a simple
  property is required (it is composite, *Structural Specification* §11.1, §11.2: in a
  cardinality; functional, inverse-functional, irreflexive, asymmetric, disjoint);
  `owl:topDataProperty` other than above another data property (§11.2); datatypes outside
  the map and undefined, facets outside a datatype's facet space, cyclic datatype
  definitions (§11.2, §7.5); an irregular property hierarchy (§11.2); literals beyond what
  §4 asks an implementation to support (numbers past 128 bits, more than 18 fractional
  digits of a second);
- *a budget the configuration set, answered `gave-up` naming its setting:* none is built
  in (13 October, branch `v15/dl-budgets`): the time, memory, nodes, branch points, merges,
  candidates and explanation tests are settings, each accepting `unlimited`; by default the
  server keeps its time limit and a query's candidates, memory comes from the request's
  envelope, and the deterministic budgets are unlimited ([config
  reference](ops/config-reference.md), `dl.*`; the guard `nrese-dl/tests/tableau/limits.rs`);
- *a representation's capacity, answered `gave-up`:* more than 12 patterns and language
  ranges in one combined set of values (per combination, no longer per ontology); a
  component of over a million pairwise-unequal data values.
- *closed on 12 October:* U as a subproperty, in a chain, an equivalence or an inverse
  pair, and in keys; the datatype theory's search for distinct values (exact to the
  deadline) and its pattern limit (per combination).

**Measured finite coverage** ([design](design/owl2-dl.md) §11; [the campaigns](../benches/reasoning/dl/README.md#the-small-scope-12-october-2026-main-pc)):
every OWL 2 DL ontology of the small scope `4,4,4` (2,761,335 after symmetry reduction:
every constructor and axiom type, one axiom up to size 4, two and three up to total size
4) and a covering array of every pair and triple of the 67 features beyond it (3,354
ontologies of 3 to 6 axioms, all 47,905 triples), each answered by NRESE's consistency,
classification and realisation against HermiT and Openllet: NRESE decides every case of
the scope, and of the covering array all but one within the campaign's 20 s (that one in
20.6 s); it agrees with the references wherever they agree with the semantics; each of
the 560 + 41 disagreements is settled by the semantics in NRESE's favour (the references mishandle U in keys and as a
subproperty, and err on bottom properties, nominal disjoint unions and `≥ 0 r.⊥`), with
NRESE's checked model refuting the reference where it has one. The campaign found and
fixed four defects (disjoint operands U's rewriting made equal; keys over transitive
properties; keys over U alone in the context core and in realisation). In the gate: the
scope `3,3` (35,505 cases) against the recorded references in the push tier, its 8,578
one-axiom cases in the commit tier.

**Certificates** ([design](design/owl2-dl.md) §10): "consistent", satisfiable classes and
non-entailments of the hypertableau come with a model the independent checker
(`nrese_dl::certificate`) verifies: 99.84 % of such claims on the small scope without data
values (8,990,815 of 9,005,280); the number module's validated models likewise. Data values
since 13 October (the models carry the values the datatype theory assigns: 196 of the W3C
suite's 199 consistent answers with a model certified, the 3 others listed with why). Not
yet: "inconsistent" and entailments (the references decide them in the campaigns).
A checked certificate establishes its individual claim; the percentage is
coverage of this campaign's positive claims, not a proof that every engine path is correct.

**Incrementality** ([design](design/owl2-dl.md) §9): U1 and the RL closure maintained per
commit; the consistency gate after a commit of assertions only, from a consistent
revision, checks nothing for removals and only the ABox islands the additions touch;
schema changes, the taxonomy, classification and realisation recompute. Each maintained
path is tested against a cold rebuild (store: 3,200 random commits, every one accepted
exactly when a fresh store accepts its data). Not built: the context core's incremental
saturation, bounded-cone TBox deletion, footprint caches.

**Open:** a hard tiny case where NRESE's search is slow (`cover:3` case 2413, five axioms,
a pigeonhole through a nominal: 20.6 s against HermiT's 1.8 s and Openllet's 69 ms; [DL
performance lab log](design/owl2-dl-performance.md#8-lab-log-measured-changes-kept-and-rejected));
certificates for data values and for "inconsistent".

The packages of [design/owl2-dl.md](design/owl2-dl.md) §14 (the performance gates are
[owl2-dl-performance.md](design/owl2-dl-performance.md) §6's):

| Package | State | Correctness gate | Performance gate |
|---|---|---|---|
| 3.0 DL lab | done in part | the reference runner (HermiT, ELK, Openllet, Konclude), the W3C runner, the ORE development subset (80 tasks, adjudicated), the stratified-set tool (`ore.py strata`), the fast suite's DL cases, the small scope; the ≥ 300-task set's and OWL2Bench DL / UOBM reference results aren't committed | bars recorded for the 80-task subset only |
| 3.1 Context core, Horn stage | done | Horn and EL taxonomies equal to the references'; parallel; proofs into the proof IR. The EL classifier is still `nrese-reasoner`'s, not the core's EL mode | EL: ahead of ELK 0.6 on the development subset; the full EL track not run |
| 3.2 Bounds | done | U1 maintained per commit, equal to U1 afresh; consistency through U1 and the Horn core | LUBM(800) / UOBM(500) against Konclude: not measured |
| 3.3 Hypertableau | done | the W3C DL suite: every case decided (410 right, one disputed in the suite's own answer); the small scope | within 3× of Konclude: not measured on the 300-task set |
| 3.4 Classification and realisation | done | taxonomies equal to the references' on the development subset; the small scope's classification and realisation | within 1.5× of Konclude: not measured |
| 3.5 Datatype theory | done | the W3C DL suite; exact to the deadline, the pattern limit per combination (12 October) | datatype strata not reported apart yet |
| 3.6 Context core, SRIQ and SROIQ stages | open | the Horn stage with the Eq rule for functional properties; disjunctive heads and nominals go to the hypertableau | – |
| 3.7 Caches and reuse | in part | completion-graph reuse (`tableau::Base`) across classification and realisation; the Horn lower bound coupled; footprint caches, SIMD: open | – |
| 3.8 Justifications | in part | black-box minimal axiom sets over the hypertableau (`/explain`, the gate's rejection evidence); glass-box certificates open | – |
| 3.9 Conflict-driven search | research | conflict-guided choice order and dynamic backtracking behind switches, off by default; learned nogoods open | – |
| 4.1 L and U1 | done | U1 ⊇ certain answers ⊇ L on LUBM, UOBM, OWL2Bench | – |
| 4.2 Query path | done in part | closed predicates, the bounds, `ExactGroundEntailment`, `ExactInternalisableCQ`, a status on every answer; `ExactFragmentCQ`, U2 and RSA bounds open | – |
| 4.3 Consistency gate | done | on commit, quarantine, minimal rejection evidence; incremental for assertions (12 October), tested against cold rebuilds | – |
| 4.4 Explanations | done | DL answers' justifications through the proof IR (`/explain`'s `owl2_dl`) | – |

## Beyond OWL 2 DL (branch `v15/dl-beyond`, 13 October)

**Decided where it is sound, said why where it isn't** ([design](design/owl2-dl.md) §16;
[campaigns and ORE](../benches/reasoning/dl/README.md#inputs-outside-owl-2-dl-13-october-2026-office-pc-queue-jobs)):
`nrese_dl::beyond` answers consistency and classification of inputs the reader finds
outside OWL 2 DL. Every "consistent" rests on a model the independent checker verifies
against every axiom and rule (data values included since 13 October), every
"inconsistent" and entailment on monotonicity (the DL part, with rule instances that
follow from their rules); nothing is assumed.
- *DL-safe rules* (SWRL; ORE's only construct outside OWL 2 DL: 22 of 1,920 ontologies):
  read, and decided by grounding the instances the DL part's models break. ORE: 714
  consistent (a certified model satisfying its 8 rules; HermiT and Konclude fail on it);
  237 and seven like it inconsistent by their DL part, each with a four-axiom
  justification; 10860 inconsistent with 7 instances of its rules (its one rule with
  SWRLTab's `abox:hasClass`, not a SWRL built-in, left out and named). Consistency decided
  for 21 of the 22 (12 consistent, 9 inconsistent), classification exact for 20 (12451:
  sound, its refinement's budget leaves pairs open). Undecided, with why: 13129 (an
  ill-typed literal, §5.7).
- *Repairs:* a disjointness repeating an operand reads as that operand being empty (it
  was deduplicated into nothing: a wrong "consistent" on four W3C tests); SQWRL queries
  assert nothing; as before, a property typed by its use.
- *Left out, said so:* the reserved vocabulary as an entity (§2.4: it was read as fresh
  names, a wrong "non-entailment" on five W3C tests), ill-typed literals (§5.7), what the
  reader can't map; "inconsistent" and entailments from the rest still hold.
- *Evidence:* five differential campaigns of 1,000 random ontologies with rules against
  HermiT and Openllet: every disagreement is a probed deviation of a reference from
  SWRL's semantics (empty heads, unbound equality variables, declared individuals,
  inverse atoms) or a reference error settled by hand; on the two clean campaigns 970 and
  972 of 1,000 agree, the rest undecided or unanswered. The W3C suite's species-Full tests
  with direct-semantics answers run in the gate: no wrong answer but two the suite
  mislabels (listed with why).
- *Open:* models the certificate builder can't fold (counting cases; 66 of the campaigns'
  5,000), native rules in the hypertableau should rule-heavy inputs need many rounds.

## Audit work (the outside audit of 2 October)

### A. Hygiene and trust

| Item | State | Notes |
|---|---|---|
| A1 `tmp/` and the legacy tree | done | Runners and probes committed with usage lines; spent files, `artifacts/`, `ops/fuseki/`, `Spezifikation.md` removed |
| A2 mTLS subject header from trusted proxies only | done | `auth.mtls.trusted_proxies`, loopback by default |
| A2 Reads in client transactions without the writer slot | done | Speculative transactions; the view cached per session by base version, operation count and the reader's access |
| A2 The reasoner's list cap | done | Lists of any length; native rules where expansion is quadratic |
| A2 `Decoded::keep` without the moved `Box` | done | Raw pointers with a `Drop`; a test |
| A2 Miri over the engine's unsafe code | deferred | Never ran (Miri isn't installed on the main PC). Planned with the refactoring after the merge to `main` ([roadmap](ROADMAP.md) step 4) |
| A2 Dead wildcard arms | done | Four deleted; six covered real variants and name them |
| A3 `scripts/check.sh` gate, pre-commit and pre-push hooks | done | The pre-commit gate tests the changed crates and every crate depending on them |
| A3 Workspace lints, resolver 3, `rust-version`, `publish = false` | done | Every suppression is an `#[expect]`; no `#[allow]` left |
| A3 Dependencies declared once in the workspace | done | Every external dependency two or more crates use (20 workspace entries) |
| A3 Duplicate dependency versions | partly | Our direct dependencies are aligned; duplicates pulled in by third-party crates remain (`rand` 0.9/0.10, `getrandom` 0.2/0.3/0.4, `digest` 0.10/0.11, `syn` 2/3, `thiserror` 1/2, `winnow` 0.7/1.0) |
| A3 Docker image for a portable CPU level | done | `NRESE_TARGET_CPU=x86-64-v3` by default |
| A3 CI at milestones | done | `ci.yml` and `oracle.yml` on `workflow_dispatch`; runs: 36998782678, 37007059759, and on the A+B milestone `04f36eb` CI 37021511088 and the oracle 37021515465 (all green). The oracle compared nothing from 1 October (renamed tests) until `1516dce`; it now fails on an empty comparison, and compares NRESE on the canonical copy too. On `04f36eb`, locally and in the workflow: 9,390 queries, 9,077 agree, 313 explained, none unexplained. Perf lab, `04f36eb` against `cd75751` (olympics, 10 M entities; two alternating rounds of five): same rows on every query, summed medians 1.02× and 0.99×, per-query differences within the rounds' spread. Merged into `main` as `c9ee847` (2 October): `04f36eb` with the review's R1–R4 cherry-picked (`merge/milestone-ab`, CI 37053802107 and oracle 37053806175 green), then `main` merged back into this branch |
| A3 One integration-test binary per crate | done | `tests/it` in nrese-store, nrese-server, nrese-sparql, nrese-engine, nrese-rdf-io, nrese-shacl; the W3C and GeoSPARQL conformance suites stay binaries of their own |
| A3 `cargo-nextest` | deferred | It was for link time, which one binary per crate already removed; per-test processes aren't needed by any test today. Revisit if a test needs process isolation |
| Test-only code behind a feature | done | The reasoner's oracle (`naive`) behind the `oracle` feature; `testing` was production code and is `vocabulary` |

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
| Design | done | [The query plan](design/query-plan.md): a migration in steps |
| Algebra walker | done | `nrese_sparql_syntax::visit`; the executor's plain searches use it |
| Step 1: the logical plan | done | `nrese_sparql::plan`: built from the algebra and lowered back, the identity on every random query of the differential tests |
| Step 2: rewrites on the plan | started | Named rewrites, in order, for queries and update `WHERE`s alike: `join-groups` (groups joined to each other become one basic graph pattern; inputs with an `ORDER BY` keep their place), `eager-aggregation`, `filter-pushdown`; EXPLAIN lists those that changed the query (`rewrites`). Open: pushdown on the plan itself, and the executor's match-guard strategies (paths from their bound end, limit placement, bind joins to `SERVICE`) as rewrites |
| Step 3: estimates on the plan, EXPLAIN before running | started | `plan_query` and `explain=plan`: the rewritten plan, each node with estimated rows from the store's statistics (triple patterns exact, basic graph patterns by the join orderer, the rest by documented rules in `native/estimate.rs`), without running; every random query of the differential tests is planned. Characteristic sets of the default graph (per read model; built at once up to 1 M statements, beyond on a thread of their own, kept until the size drifts by a quarter, none past 131,072 distinct sets) estimate stars in the join orderer and in EXPLAIN. Characteristic pairs (3 October; a second scan when the sets are built, within 8 M subjects and 1 M pairs) estimate chains: a star pattern joined after an edge `?x p ?y` counts only the links whose object has its predicate (exact on the chain test, where independence was 7 times off). `perf_lab --qerror` reports the planner's q-error per operator over a query set: on LUBM(10) with OWL 2 RL (`benches/baselines/perf-lab/lubm10-owl2rl-qerror-2026-10-03.json`; all 14 queries 14.6 ms at p50) scans are exact and index joins within 2× but for one, while 5 joins are 12.6× off, all from Q8's star with two constant objects (`?Y a Department ; subOrganizationOf <University0>`): characteristic sets know no value correlations. EXPLAIN after running (`explain=true`, `explain_query`) shows the same estimates beside the actual rows for every operator, those inside an operator included (5 October): the joins of a pattern evaluated from rows (`sideways`), `limit pushdown`, `closure` (by strongly connected components), `semi join`/`anti join` (EXISTS as sets) and the rest report themselves, each with a plan guard; only full-text and vector searches have no estimate (no statistics). Open: correlated constants in stars, the physical operator choices |
| Step 4: executor on the plan | open | |
| The result cache, step 1 (merge checklist §2 item 7) | done | `nrese_sparql::cache` replaces the store's whole-query cache of serialised answers. Every operator's result and every prefix of a basic graph pattern's joins is a part kept as id columns, shared by every query of the store: keyed by the part (variables numbered, so renamed parts share) and by everything its result depends on, from the snapshot's identity (revision and content: a transaction's pending state never meets a committed part) to the user's access. Admitted by recompute cost against bytes within a byte budget (an allocator-checked guard), computed once when requested concurrently, pinnable by name (`/api/v1/…/cache/pins/{name}`, `budgets.result_cache_pins`); EXPLAIN marks parts from the cache. Cache on and off agree on random queries over random commits. A whole answer's bytes are kept per output format beside its id columns (owner's decision, 6 October), so repeats are copies. Replayed logs (performance.md lab log, 6 October), against the whole-query cache: first rounds 1.8× faster (olympics 0.253 → 0.134 s, yago-tiny 0.722 → 0.394 s), repeats as fast (0.007 → 0.007 s, 0.015 → 0.014 s). Open (v3): invalidation by footprint, delta maintenance, common subexpressions, operator internals, a compressed cache |
| `nrese-sparql` split, numeric promotion once | open | |

### D. Reasoning (with G3)

| Item | State | Notes |
|---|---|---|
| Inferences and graph access | done | `inferred = "supported"` in the access policy: a user who may read the default graph sees an inferred statement when one of its derivations uses graphs the user may read alone (support graph sets, decided 2 October). `nrese_reasoner::graph_sets` computes the minimal sets per fact (annotated semi-naive over the ground program, schema premises and their alternative groundings included; equal to the closure of every set of graphs over 30 seeds × 150 random ontologies; LUBM(10) 1.1 s against 0.2 s for the materialisation). `nrese_store::support` computes them per revision on the first read that needs them, and each reader's view once: the snapshot with the invisible inferred statements removed (`Snapshot::with_inferred_subset`, tombstones or a stack of the visible ones, whichever is smaller), so queries, counts, sorted scans, statement reads, graph reads, pending session reads and explanations agree. `reasoner.support_sets` caps the sets per statement (default 16; fewer only hide; graphs in IRI order, so the cap is deterministic). Commits update the sets for what they can affect and each reader's view follows by a run per change stacked over the stack (LUBM(10), 8 graphs: first restricted read 2.4 s, after a one-statement commit 2 ms, after a thousand statements 53 ms). Tested in the store, through HTTP with the policy file and the commit path, and differentially (updated against computed afresh, in the reasoner and through the pipeline). The sets are prepared in the background at start, when the policy turns to `supported`, and after rematerialisations. Open: keeping the sets across restarts (now recomputed in the background), `compact` equality (its users see no inferences yet), explanations that prefer readable derivations |
| OWL 2 QL through existentials ([design](design/ql-rewriting.md)) | done | Tree-witness rewriting (Kikot, Kontchakov, Zakharyaschev; Ontop's) of each basic graph pattern over the `owl2-ql` or `owl2-rl` closure (`reasoner.ql_rewriting`: `auto`, the default, rewrites under `owl2-ql` only; `on` under `owl2-rl` too; `off`; per repository): the schema read at the snapshot and compiled in `nrese-owl::ql`, cached per store; class atoms get the memberships the closure lacks; sets get the union, bags their materialised rows and each further answer once; bounded; EXPLAIN shows `ql-tree-witness`. Checked against a bounded chase on random QL cases (600 queries in the gate, 24,000 more on 5 October: no difference) and on the W3C QL cases (51 pass, 9 listed with causes, none about the rewriting). LUBM(10): 6,960 answers more on queries through its existentials. Incompleteness is never silent (6 October): where axioms outside QL meet the anonymous individuals (transitivity, chains, functional properties, keys, cardinalities, RL constructs on the left), or a pattern reached a bound or runs without the rewriting, every answer says `sound-only` with the reasons (`NRESE-Completeness`, shared with the DL bounds; EXPLAIN's `ql`); checked on mixed ontologies against a chase with equality (23,828 queries: none wrong, 349 misses all flagged). NPD is the stress case (implied witnesses dropped; the bounds trigger within 0.7 ms). The printer (6 October): a rewritten query as standard SPARQL 1.1 for stores without reasoning (property paths over the schema, or the hierarchy enumerated in `VALUES`; `nrese-server print-query`), what it can't write exactly reported; the same rows as NRESE on the random pure and mixed cases and on every query of LUBM and OWL2Bench-QL. Inside `EXISTS`, `NOT EXISTS`, `MINUS` and `GRAPH` naming the default graph too (6 October), a negated pattern's incompleteness making the answers `unsound`; checked against the chase. NPD on its data (2.0 M statements, materialised by Ontop): all 31 queries give Ontop's answers. Open: following RL rules over anonymous individuals instead of flagging. No cost where the rewriting adds nothing: a witness the data already has wherever it folds is left out (7 October; NPD 1.27× → 1.00× of the rewriting off) |
| Faster deletes (provenance, [design](design/reasoner-provenance.md) step 1) | done | Recursion decided per ground key (predicate, or `rdf:type` with its class; equality's `sameAs` premise and class or property `sameAs` in the key graph). Overdeletion keeps a candidate that a non-recursive rule still derives in one step from what is left; that check and the B/F proofs of the rest run in parallel (a prover per worker). LUBM(10) deletes per commit: 1 → 0.9 to 0.4 ms, 100 → 15 to 1.9 ms, 1,000 → 111 to 6 ms, 10,000 → 134 to 13 ms (the rematerialisation: 225 ms); LUBM(100): 1,000 → 6.7 ms, 100,000 → 129 ms (2.3 s). Stored derivation counts (Hu, Motik and Horrocks) were built, measured slower at scale and removed |
| Materialisation memory (P1): the transient fold, unused object orders | done (branch `p1/transient-fold`, to be merged) | Guards for every win of [performance](design/performance.md) §0 (F1, F4, F8, F9/F10, F11), each shown to catch its revert. A heap profile by phase found the peak in round 2's candidates, then the fold: candidates kept per morsel, runs in chunks folded a chunk at a time, unread `(object, subject)` orders not kept. LUBM 1000, the branch against f2c5dea: committed memory while reasoning 13.49 → 9.08 GB, reasoning 49.2 → 43.1 s (interleaved medians, n = 3; LUBM 100 1.99 → 1.37 GB, 3.48 → 3.38 s). The process's peak is now the load's |
| Rule work (rules package, investigation of 6 October) | done (branch `perf/rules`, to be merged) | Counters of the joins' work (bindings per round and rule, old checks, closure pairs, partner scans) and the `rule_work` probe. R1: tautological ground instances dropped, LUBM 10 bindings 6.74 → 4.85 M, LUBM 1000 reasoning 38.6 → 35.4 s. R4: `sameAs` merges rebuilt within the semi-naive rounds, one materialisation for any cascade (LUBM 100 + 16 merges 38.1 → 3.9 s, 3.95 → 1.39 GB), from the grouped input (G8). R10, R12 small wins; R2 (projection) and R11 (incremental closure) measured and not kept. G5: compact-mode merging commits atomic (LUBM 10: 5.5–6.7 s in two revisions → at most 0.42 s in one). G7: one union and one SCC kernel in `nrese-exec`. G13 was already fixed (da4e92c). Open: `sameAs` splits in compact mode (B3), cross-rule redundancy (B2), `nrese-dl`'s own Tarjan |
| `v2` flattened, `nrese-core` folded in | done | The reasoner's modules sit at its root (`nrese_reasoner::batch`, `::delta`, `::explain`, ...); `pie` and `recursion` are private; the oracle tests live in `src/tests/`; `nrese-core`'s types are `nrese_reasoner::capability` (3 October) |
| OWL 2 DL | done for coverage (v1.5); performance gates open | Coverage, its proof to a bound, certificates, incrementality and the state of each package: [OWL 2 DL (v1.5)](#owl-2-dl-v15-branch-v15dl-complete-12-october). History: [ADR-0009](adr/0009-owl2-dl-reasoning.md) accepted on 2 October and revised after seven research reports; the engineering design, component by component with gates, is [design/owl2-dl.md](design/owl2-dl.md) (3 October); `owl2-dl` always reports completeness, certain answers by default only where a complete path exists (decided 2 October). **2.1 done:** `nrese-owl` reads the OWL 2 structural model from triples by the reverse RDF mapping (every axiom with its source triples and graphs; OWL 1 forms the mapping keeps; diagnostics for what isn't OWL 2 DL, including non-simple properties where simple ones are required) and writes it back; all 403 RDF/XML ontologies of the W3C OWL 2 test cases of species DL read and round-trip (three are errors in the test data, listed); random models round-trip (5,000 cases). **2.2 done:** normalisation into DL-clauses (negation normal form, structural transformation by polarity, clausification with absorption of negative literals, `≤ n` spelled out up to a configurable bound and as at-most atoms above it, non-simple roles by Horrocks–Sattler automata, the ABox as facts; every clause keeps its axioms and dispatch flags); checked against the semantics on 720,000 random finite interpretations (a model of the ontology iff it extends to a model of the clauses, plus exhaustive soundness on small cases), and every W3C DL ontology normalises. **2.4 done:** the proof IR (`nrese-owl::proof`): derivation hypergraphs, justifications by resolution (PULi-style, smallest first, each minimal; `one`, `core`, `union`, `top-k`, `all`, with budgets and a completeness flag), proofs and a proof checker; equal to a brute-force Horn MUS enumerator on 10,000 random hypergraphs; the RL reasoner's derivations moved into it (every rule grounding), served by `/explain` with a checked proof per set. The EL classifier's derivations follow its move onto the clauses (3.4); lifting statements to OWL axioms comes with the store's DL mode (4.4). **2.5 done:** the SROIQ(D) fuzzer (`nrese_owl::fuzz`: random ontologies within the global restrictions, an ordered RBox with regular chains, EL and SROIQ(D) profiles) and metamorphic transformations (renaming, reordering, redundant axioms, fresh names); in CI: fuzzed ontologies are OWL 2 DL (no diagnostic), round-trip and normalise, and the EL classifier's taxonomy survives every transformation (5,000 ontologies); `--example fuzz` writes functional-syntax campaigns for the reference reasoners. It found a writer gap (an inverse pair of two inverses), fixed. **2.3 done:** the reference reasoners behind a runner of NRESE's own (`benches/reasoning/dl`: HermiT, ELK, Openllet on OWL API 5.1.20, KoncludeCLI as a process; canonical taxonomies by hash; DISPUTED records). Its W3C direct-semantics runner reproduces 351 of HermiT's 362 published passes of 2009 (the rest time out at 120 s, one crashes on OWL API 5) and 366 of Pellet's 369 with Openllet, and records 30 wrong answers of Konclude 0.7. On an ORE 2015 development subset (80 tasks), 79 agree across the references and NRESE's EL classifier, after a fix to its output (classes equivalent to `owl:Thing`). Phase 2 is complete. **Phase 4, in the store (G3, `g3/dl-store`, 6 October):** `reasoner.mode = "owl2-dl"` ([semantics](spec/reasoning-semantics.md)): the lower bound L (the RL closure plus the memberships the TBox's DL taxonomy adds), the upper bound U1 (PAGOdA's strengthening, every disjunct derived) in a stack beside the engine's, maintained per commit by the delta executor and never visible as data (equal to U1 evaluated afresh after random commit sequences); consistency on commit by U1 where it proves it, else the context core or the hypertableau, rejected with the minimal inconsistent axiom set (LUBM(1): a single-assertion commit 0.79 ms against 467.7 ms with the hypertableau over the whole ontology, `owl2-rl` 0.37 ms); the query path (closed predicates in one evaluation; the bounds compared; `ExactGroundEntailment` and `ExactInternalisableCQ` on the gap; a status on every answer: the `NRESE-Completeness` header and EXPLAIN, on the status QL's rewriting reports too (`nrese_sparql::Completeness`, one owner); `dl.answers` `certain-where-complete`, `sound`, `exact`; LUBM's 14 queries all complete with RL's counts); `/classification` and `/realisation` through `nrese_dl::classify`; axiom entailment (the 7 RL W3C cases the rules can't show pass through it); justifications of DL answers through the proof IR (`/explain`'s `owl2_dl`); the per-axiom profile checker (`nrese_owl::profile`, 277 of 297 W3C cases agree with the annotations, the 20 others contradict the Recommendation or cover unread imports) and routing (an RL ontology to the rules alone, no U1). The review's checks hold (user rules: never complete; ⊥ blocking a disjunct; Skolem constants never answers). Budgets on every path (`dl.timeout`, `dl.max_candidates`, `dl.max_nodes`, `dl.max_branch_points`), graph access (answers beyond L for readers of every graph only), read replicas (same answers and status; nothing interned). Follow-ups (`g3/follow-ups`): every status names its regime (`owl2-ql`, `owl2-rl`, `owl2-dl`, `rdfs`, `custom`: what `complete` refers to); exact answers that can't be proven answer 422 (`incomplete-answer`, naming the unresolved candidates); EXPLAIN's `candidates` (proved, refuted); U1 versioned with the snapshot (tested while commits run, in transactions, on replicas, across a restart), which found and fixed a race: a streamed answer read a later revision than the one its status was decided on. Open: incremental L beyond the taxonomy (realisation in L), per-module dispatch inside one ontology (routing is per ontology), tighter bounds (U2, RSA) and `ExactFragmentCQ`, the context core's glass-box proofs in explanations, the bounds-equal queries' second evaluation (1.3–3× `owl2-rl` on LUBM) |

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
| G1 Bug hunt | running | Random differential and property tests over many seeds (`scripts/fuzz-campaign.sh`). Fixed on 1–2 October: `ORDER BY` not total over terms of one canonical form (seeds 2329, 2458); a `LIMIT` cutting through equal sort keys; compact equality losing copies of a statement deleted from the default graph but asserted elsewhere (seeds 180, 192); path multiplicity between constants (seeds 1036, 1101, 1128); explanations differing between runs; the oracle's own mistakes (SAMPLE, CONSTRUCT with LIMIT, NOW, row order, GROUP_CONCAT seed 2051). Found by review on 2 October: sessions anyone could join (sequential ids, no owner); RDF4J computing graph access for the default repository instead of the one asked; request bodies read before authentication; ill-formed shapes stored; `SERVICE` open to every user; N-Triples `VERSION` lines panicking the line reader. Found by the check of the DL research reports: `/explain` ignored graph access (premises from unreadable graphs shown; an existence oracle for hidden statements) and took the writer slot to intern its rule constants; now scoped, with hidden steps, and lock-free. Found by the parser fuzz targets (F1): **a denial of service**, a 16 KB query of 1,500 chained `1 +` overflowing a 2 MiB thread's stack and taking the server down (`||`/`&&` chains now built balanced, arithmetic chains counted against the nesting limit); the RDF/XML reader taking any `xml:lang` as a language tag (`"bar"@es/wn`) and nodeIDs that are XML names but no blank node labels (`object.`), which no writer could write back; the SPARQL parser using a `BASE` with dot segments as written, and accepting a `GROUP BY` that binds a variable twice; the SPARQL writer printing queries that read back differently or not at all (nested `!` and `FILTER` brackets past the nesting limit, a blank node label spanning a FILTER before a path split into two groups). Found by the bench agent (7 October): **a denial of service**, a 150 KB chain of `UNION`s overflowing the stack after parsing; sequences (`UNION`, joins, paths, `BIND` runs, SELECT lists) are now n-ary in the algebra and the nesting limit counts the algebra ([ADR-0012](adr/0012-query-depth.md)). Found with it: **planning a large basic graph pattern occupied a request without a deadline**, a 1,000-pattern star 457 s, a 1,000-step path 7 s, 1,000 single-variable checks over 112 s (release); the join orderer extends a star's statistics and the checks' shares a step at a time with a bounded effort, a pattern's result-cache keys are chained, and planning observes the query's cancellation (0.59, 0.55 and 0.36 s). Found by the push tier the same day: **coverage that depended on the seed**: 38 random tests asserted coverage after a fixed number of draws, and a seed could fail any gate with every answer right (66810: 1,333 queries with solutions in 4,000 where a third were asked; 42817 in the DL fuzz test); they now draw on, up to ten times the planned cases, until their coverage holds. Found by the independent review of the A+B milestone (`cd75751..04f36eb`): long-list consistency rules piling up in the delta executor's program with every commit that touches a list (R1; the index compared by pointer); local logins lockable per user name by anyone, their failure counts unbounded, and their timing telling which names exist (R2–R4: now counted per name and client address with backoff and a per-address limit, bounded, with a dummy Argon2 check). Found by the Jena oracle once it compared again (it had compared nothing from 1 October): integer-derived literals outside their datatype's range (`"300"^^xsd:byte`) computed with as numbers (59 of 61 differences). Round 5 on the office PC (seeds from 3000) not yet reviewed (the machine is unreachable); locally, seeds 6000–6643: no failures |
| G2 One engine API | done but the browser part | Namespaces, sessions, repositories, imports as jobs, rematerialisation, explanations, running queries, graphs, users and workspaces, OpenAPI; implicit prefixes; shapes and user rules as checked managed objects; saved queries. Client kits run end to end (`benches/clients/run-all.sh`, 3 October): RDF4J's Java client, Apache Jena's RDFConnection with three result formats and the Graph Store Protocol, rdflib's SPARQL store and Dataset, and the Datamodel Workflow's export replay, each against a fresh server; all pass. They found that Jena's and rdflib's names for the default graph (`urn:x-arq:DefaultGraph`, `urn:x-rdflib:default`) were taken for named graphs: now the default graph in GRAPH patterns, update data and templates, query datasets and the Graph Store Protocol (`nrese_sparql::compat`). Open: ResearchSpace's pages and forms in a browser (with the frontend) |
| G3 Reasoning that leads | started | Explanations through the API are done, and justifications (`explain?justifications=one|core|union|top-k|all`, 3 October: minimal sets of asserted statements, smallest first, each with a checked proof); inferences by premise graphs; OWL 2 DL in the store (`owl2-dl`, `g3/dl-store`: bounds, the query path with a completeness status on every answer, consistency on commit, classification, realisation, entailment, DL justifications, routing by profile; see D); equality stage C has started (see D) |
| G4 Queries | done | A path bound at both ends follows from the smaller end; groups joined to each other are ordered as one pattern; stars estimated from characteristic sets; eager aggregation (`COUNT`, `COUNT(*)`, `SUM`, `MIN`, `MAX` and `AVG` over a join aggregate the side they read first; `AVG` as partial sums and counts divided after the group, with SPARQL's promotion and errors: checked against the reference evaluator on random groups of mixed numbers, durations, strings and unbound values, 5 October; BSBM 10 M BI q4's average per feature 214 → 56 ms); paths planned with the rest (5 October: a join of triple patterns and paths is ordered as one by the join orderer, a closure from a constant estimated by a bounded search; DBpedia path joins 5.7-37.9 → 0.1-0.15 ms, YAGO 82 → 2.4 ms); characteristic pairs (3 October, be7aeef: chains estimated in the join orderer and EXPLAIN, see C). What the query plan keeps open (correlated constants in stars, the physical operator choices, step 4) is in C |
| G5 Vector search | started | Step 1 of research designs §2: vectors as literals (`"[…]"^^nrv:vector`, `PREFIX nrv: <urn:nrese:vector:>`), indexed per dimension over the dictionary like the text index (`nrese_engine::Snapshot::vector_search`); `nrese-vector` holds the kernels (cosine, dot, Euclidean, vectorised by the compiler), the exact parallel scan and an HNSW of its own (parallel build, filtered search; recall@10 ≥ 0.95 in its test). Queries: `SERVICE nrv:search { ?v nrv:near … ; nrv:k ; nrv:score ; nrv:rank ; nrv:metric ; nrv:exact ; nrv:searchBudget }`, candidates from the patterns that bind `?v` first (exact scan below 2% of the space, else the graph with that filter), query vectors bound outside the block, only literals the query's dataset uses (graph access holds). Graphs of spaces past 50,000 vectors are built on a thread while searches scan; a graph covers the space's first vectors and the newer ones are scanned exactly (base and delta), a copy extended past a tenth more; vectors and graphs are kept across restarts. Open: SQ8 and RaBitQ codecs, Vamana on disk, embeddings computed in the store (`ort`), several vector spaces fused by rank, the ANN and RDF+vector benchmarks |
| G6 Hardware and scale-out | started | **Read replicas (3 October):** a primary ships its write-ahead log (`/api/v1/replication/log`, and an image to start from), replicas apply it commit by commit with the primary's revisions and term ids, in their own log too, take no writes and don't reason ([ops/replication.md](ops/replication.md)); tested end to end over HTTP (image bootstrap, inferences, named graphs, restart, status) and in the engine (gaps refused, truncated logs detected). Not yet: automatic re-bootstrap and failover, partitioning across machines |
| Persisted derived indexes | done | The full-text indexes (literals, IRIs' local names) and the vector index (vectors and HNSW graphs) are written to `derived/` of the data directory after each checkpoint (atomically, with a checksum and a fingerprint of the dictionary entries they cover) and read at their first use after a start, then extended with the terms interned since: no rebuild at start. A file of another store, an older dictionary or a damaged one is ignored and the index built anew. `nrese_derived_indexes_loaded_total` counts the reads |
| G7 Smaller items | started | RDF 1.2 `version` announced in results: done. Error recovery (5 October: a recovering parse skips the rest of the outermost RDF/XML node element an error is in, and a JSON-LD top-level element that fails to expand; bulk loads use it): done. Bulk loads quarantine what they can't read (13 October, docs/design/quarantine.md): every valid statement loads, each invalid one is kept verbatim per repository (text, file, line, reason, load), invisible to queries, reasoning, exports and statistics, reported with the first examples, and re-admitted through the commit pipeline (fixed, or repaired on request) or discarded; `store.invalid_statements = "strict"` or `--strict` stops at the first as before. The HTTP and CLI resolution endpoints are proposed, not built: open. HTTP fuzzing (5 October: seeded random requests over every route of the router's source, with and without bearer tokens, in `scripts/fuzz-campaign.sh`) and a soak probe (`benches/probes/soak.py`: errors, memory slope, lost writes) found two bugs, fixed: two image backups within a second collided (500), and an answer sent before a small request body was read closed the connection under clients that send the body after the headers. A 30-minute soak on the main PC (8 workers, about 1.35 M requests): no errors, the soak graph exactly as committed, resident memory flat (91 MiB after the warm-up, 88 MiB at the end), handles and threads steady. The office soak's slow growth (6 October, 5 h: 287 → 308 MiB resident) is the allocator's, not a leak (7 October, performance.md lab log): the program holds about 2 MiB and never more than 15 MiB; mimalloc's transparent huge pages hold the rest, bounded by its reserve; `budgets.huge_pages = "off"` serves the same load in about 100 MiB at 26 % slower materialisation; a guard keeps the program's heap level under the soak's mix. Multi-architecture images (5 October): the Dockerfile cross-compiles for linux/amd64 and linux/arm64 (x86-64-v3 and Neoverse N1 by default; the arm64 image checked under emulation), `.github/workflows/images.yml` builds both and pushes only when asked, and the CPU check reads arm64's real capabilities from the kernel. Missing benchmark kits, a second oracle, a soak of hours on the benchmark machine: open |
| G8 Property graphs | open | Owner's goal (5 October): compete with Amazon Neptune (openCypher, Gremlin, openCypher over RDF) and Google Spanner Graph (ISO GQL, SQL/PGQ). Property graphs over the same store, mapped onto RDF 1.2 (edge properties on reifiers); ISO GQL and an openCypher subset compiled to the same algebra, so reasoning applies; paths, lists and maps as values; graph algorithms; Neptune and Neo4j CSV and GraphML import; LDBC SNB as the workload. Design from research first (coverage plan §4) |
| G9 Enterprise operations | open | Automatic failover and a write cluster beyond read replicas; backups to object storage; Kubernetes and Helm packaging; LDAP, SAML and SCIM besides OIDC; tenants with quotas and isolation (a managed offering) |
| G10 Data integration | open | R2RML and RML mappings from relational, CSV and JSON sources; virtual graphs over live SQL sources; a SQL (JDBC/ODBC) endpoint for BI tools; a GraphQL API generated from the schema; change streams (Kafka, webhooks) |
| G11 AI integration | open | An MCP server for agents; questions in natural language to SPARQL (and GQL); GraphRAG over the graph, its vector and text indexes; embeddings and LLMs run locally (CPU and GPU, quantized) besides hosted ones. Owner's direction (5 October): integrate the methods that measure best today, not the original GraphRAG, with minimal resource overhead; research first (four prompts: GraphRAG methods, vector and hybrid retrieval, local model integration, KG construction checked by reasoning) |
| G12 Exploration and history | open | Visual exploration for non-experts in the console; versioned graphs with queries at a revision, diffs and branches; entity resolution (suggesting `owl:sameAs` matches) |
| G13 Ontology work and publishing | open | SKOS taxonomy editing with change requests, approval and governance; an ontology editor in the browser (with G2's frontend); Linked Data publishing (resolvable IRIs, content negotiation); SHACL rules, SWRL import and Datalog with negation and aggregation where user rules don't reach; a Python SDK and notebook integration |

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
| §1 Equality, stage B (`reasoner.equality = "compact"`) | done | **Stage C started (3 October):** late expansion (`reasoner.equality_expansion = "late"`, the default: basic graph patterns, joins, OPTIONAL and UNION evaluated over representatives and expanded once; checked against stage B and replication on random data and queries of every shape) and `strict`/`canonical` answers (`reasoner.equality_answers`). Measured on 40,000 identities in classes of 8 with OWL 2 RL (main PC, p50 of 5): a typed chain 273 → 20 ms, OPTIONAL 306 → 19 ms, a star 393 → 119 ms, a two-hop chain under COUNT 2.61 → 2.00 s (its rows are the answer's; a count over the expansion without making it is the next step). Not yet: late expansion through named graphs and the inferred model, stable class ids, incremental merges and splits, bulk class building, the planner's expanded cardinalities |
| §4 Access by named graph | done but inferences | Dataset restricted before evaluation, writes refused as a whole, caches keyed by access, policies in the store with history, `SERVICE` a privilege. Inferred statements are all-or-nothing per policy until support graph sets (see D) |
