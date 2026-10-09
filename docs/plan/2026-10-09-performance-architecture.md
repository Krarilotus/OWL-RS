# Performance and architecture: discussion proposal

Status: **proposed, not an accepted architecture change or implementation commitment**.
Prepared 9 October 2026 on `plan/engine-v2-performance-architecture`, based on
`refactor/engine-v2` at `6b81ccb5baebdb72daf8eb3216378a6f00ed3e6c`.

The aim is to make existing capabilities faster, cheaper to run, and easier to maintain.
Improve the amount of work first, its representation second, and its machine execution
third. Preserve semantics, compatibility and the measured wins already in the repository.

This proposal follows source and history inspection, not a new benchmark campaign.
Potential gains below are hypotheses until measured. The original refactor checkout has
an unfinished merge of `dl/classify` at `4c5bc56`, including number-module changes and a
conflict in `docs/design/performance.md`. Those changes are not in this branch's base.
Reconcile the final merge before implementation; do not redo its number-module work.

## 1. Scope and interpretation

- Existing RDF/SPARQL, rule reasoning, OWL 2 DL, validation, storage and serving paths.
  Design extension boundaries for later distributed/hardware packages now; implementation
  of new product features, query languages, a distributed engine or GPU backend stays separate.
- "Kernel level" means execution kernels, data layout, allocation, CPU scheduling and
  storage I/O. It does not imply an operating-system kernel module or an assembly rewrite.
- All levels and use cases are first-class performance targets. Configurability comes
  first: select algorithms and search strategies from request needs, data shape and
  available resources, with explicit overrides. Each workload has its own regression
  gate; an aggregate improvement cannot conceal a slower workload.
- Performance changes require a demonstrated benefit and no repeatable regression on the
  agreed coverage. Structural changes require a concrete reduction in maintenance cost
  and performance neutrality within measurement resolution. Neither is assumed in advance.
- Correctness repairs are prerequisites, not advertised speedups. Comparing a correct
  operation with an earlier incomplete answer is not a valid performance comparison.
  Any cost of restoring a binding contract must be reported separately for discussion.
- Documentation and responsibility-based splits accompany the work they explain. The
  plan does not authorise a general rewrite, extra frameworks, or generic APIs without users.

The [documentation hierarchy](../README.md) remains authoritative. Existing roadmap
decisions still put major refactoring after the v2 merge. Bringing a particular cleanup
forward is a decision to discuss, not a change silently made by this proposal.

## 2. What the review means for the plan

| Evidence at the base commit | Classification | Consequence |
|---|---|---|
| Compact equality can request rematerialisation after the asserted commit; the 6 October merge fix explicitly left splits on this path | Binding atomic-publication gap | P1: repair within the transaction; do not document the defect away |
| QL TBox and realised-witness caches use revision or revision plus counts; pending views have separate content identities | Cross-component correctness gap; the local per-revision design is incomplete | P1: use the engine's existing identity contract and check all view-dependent caches |
| QL status creates a fresh cancellation token; a DL candidate check receives `cancel: None` | Binding propagation gap | P1: carry the operation's cancellation and remaining resources through nested work |
| QL preparation is reached from reporting and evaluation; DL bounds are decoded and collected before gap processing | Concrete repeated work; payoff not measured here | P2/P3: prepare once and retain ID-level results |
| Exact entailment and nonempty checks clone their premise ontology | Concrete copying; replacement safety needs design | P4: immutable compiled base plus isolated assumptions, if measurement supports it |
| Query-plan conversion back to algebra | Approved migration stage | P8: finish incrementally; do not label every adapter a defect |
| DL bounds, candidate handling and exact services reside in the store | Explicitly assigned by the current DL design | P4: review a narrower mathematical boundary by ADR before moving ownership |
| Terms from aborted transactions remain in the dictionary | Accepted replay tradeoff, ADR-0002 | P6: account for memory and document lifecycle; no opportunistic ID reclamation |
| Result-cache footprint invalidation and delta maintenance | Explicitly deferred to v3 | Keep deferred unless measurements justify a separate scope decision |
| Monotone, rollback and persistent equality state | Different lifetime requirements; ADR-0011 is proposed | Share proven primitives and laws, not one stateful universal implementation |
| `execution-core.md` still contains the retired evaluator fallback and an engine dependency absent from the actual kernel crate | Documentation drift | P0/P9: describe the current architecture and label remaining proposals |

Key sources: [mutation pipeline](../../crates/nrese-store/src/mutation/pipeline.rs),
[QL caches](../../crates/nrese-sparql/src/ql/mod.rs),
[pending snapshot](../../crates/nrese-engine/src/engine/transaction.rs),
[query options and reporting](../../crates/nrese-store/src/query_executor.rs),
[DL query path](../../crates/nrese-store/src/dl/query.rs),
[entailment](../../crates/nrese-store/src/dl/entailment.rs),
[query-plan migration](../design/query-plan.md),
[DL ownership](../design/owl2-dl.md), [storage ADR](../adr/0002-engine-storage-lsm-permutations.md),
[v2 checklist](2026-10-05-v2-merge-checklist.md).

## 3. First principles and responsibility boundaries

1. **Identity follows meaning.** A revision alone is not a view. Any reusable result must
   account for the data view, visibility, reasoning regime and options that affect it.
   Reuse `SnapshotIdentity`; specialised owners supply their semantic key components.
2. **State follows lifetime.** Persistent data, a compiled ontology, a pending transaction,
   one query and one backtracking search need different state. Share immutable inputs;
   keep speculative mutations local. A shared interface must not erase those differences.
3. **Work follows the affected data.** A small update should not scan unrelated data.
   Where locality cannot be established safely, use a documented, budgeted fallback;
   preserve atomic publication and completeness reporting.
4. **Representations follow the next consumer.** Keep IDs and useful sort order through
   scans, joins and bound comparisons. Decode when value semantics or output require it.
   Do not remove a cached representation merely because it duplicates bytes: account for
   the work it saves, its memory and its invalidation cost.
5. **Resources follow the operation.** Nested work shares an end-to-end deadline, cancellation
   and memory envelope. A per-candidate limit is not a limit on the whole request. Available
   resources constrain strategy selection; useful throughput and latency, not maximum
   worker count or largest possible buffers, determine whether they are used well.
6. **Abstractions follow stable responsibilities.** Extract shared mathematics or a reusable
   primitive only where callers share its laws. Different semantic executors can use the
   same kernels without becoming one executor.

| Owner | Boundary to preserve or clarify |
|---|---|
| `nrese-engine` | View identity, IDs, indexes/cursors, immutable versions, commit publication and durability. No SPARQL or reasoning policy. |
| `nrese-exec` | ID tables, search/sort/join/closure primitives and generic accounting. No term decoding, storage or semantic policy. |
| `nrese-sparql` | Query/value semantics, logical and physical planning, operator selection, result dependencies and representation. |
| `nrese-reasoner` | Rule compilation, materialisation, delta maintenance, equality consequences and provenance. |
| `nrese-owl` / `nrese-dl` | Structural semantics and normalisation / satisfiability, classification and search. A proposed exact-test boundary must be expressed without storage or SPARQL types. |
| `nrese-shacl` | Shape semantics and affected-focus validation; retain reuse of SPARQL value semantics. |
| `nrese-store` | Product operations, mutation gates, reasoning-mode policy, resource allocation and coordination of a consistent view. |
| `nrese-server` / console / CLI | Transport, authentication and policy / client presentation. They do not repair engine semantics. |

Prefer extending the existing owners. This plan does not require a new crate. A small
generic cancellation/deadline primitive may belong beside execution accounting if its
callers justify it; the store still allocates policy budgets. Do not pass a giant request
context containing store, HTTP and query types through every lower-level function.

### Configurability and automatic strategy selection

This is a requirement across P1-P8, not a later tuning feature. The objective is minimum
useful completion time with efficient memory use for the requested operation, within its
semantic requirements and resource limits. Where time and memory trade off, report that
tradeoff and select within the supplied limits; do not claim both improved when they did not.

- **Inputs:** operation and answer requirements, ontology/profile/query shape, selectivity,
  skew, useful order, delta size, equality-class structure, bound-gap size, cache state,
  CPU capabilities and available memory/workers under concurrent load. Collect expensive
  statistics only when their expected decision value justifies their cost.
- **Ownership:** server parses limits and privileges; store allocates the operation's
  envelope; SPARQL chooses query operators; reasoner chooses maintenance strategy; DL
  chooses its reasoning/search route; engine/exec choose storage or kernel variants.
  Share capability/resource facts, not one universal dispatcher owning every algorithm.
- **Configuration:** typed at the owning layer, with one documented default. Existing
  server/repository/request precedence must be made explicit; requests cannot raise
  policy limits. Prefer automatic selection with bounded, reproducible expert overrides
  for diagnosis and unusual workloads. Expose choices with a meaningful tradeoff, not
  every internal branch as a permanent public setting.
- **Decisions:** use inexpensive evidence and measured crossover points. Start with the
  current valid strategy when evidence is insufficient. Test skew, tiny and large inputs,
  cold and warm execution, and resource pressure. Never dispatch by a benchmark's name.
- **Adaptation:** update strategy at safe batch, operator or search boundaries, charging
  the cost of switching. Bound probing/portfolio work and prevent repeated switching.
  Do not restart a task without accounting for work and memory already spent.
- **Semantics:** a cheaper correct algorithm is allowed; silently weakening completeness,
  exactness, durability, graph access or result multiplicity is not. Approximate vector
  search stays within the explicitly requested search contract.
- **Observability:** EXPLAIN/diagnostics report the selected route, decisive estimates,
  applicable limits, override and fallback reason. Record enough to reproduce a decision
  without exposing private data. Instrumentation itself must have measured overhead.
- **Validation:** compare automatic selection with each eligible forced strategy on the
  same checked cases. Measure selection cost and runtime/memory left on the table versus
  the best measured eligible choice. Keep an unseen workload set so crossover rules
  generalise. Configuration changes invalidate only artifacts whose meaning or compiled
  strategy depends on them, with that dependency owned explicitly.

No package is complete merely because its best manual setting is fast. Its automatic
path, constrained-resource behaviour and override path must be tested as well.

### Extension boundaries for hardware and distributed deployments

The existing engine is the foundation. Design current changes so later hardware and
distributed packages extend the relevant owner rather than replace query semantics,
reasoning or storage wholesale. This is an architecture-readiness requirement, not a
claim that partitioned writes, failover or accelerator execution already exist.

| Boundary | Account for in this plan | Later implementation; not built speculatively now |
|---|---|---|
| Physical execution | Keep ordering, partitioning/locality, cardinality and memory requirements explicit. Distinguish local from global properties; operator costs can include data movement. | Exchange/repartition operators, partial/final aggregates, distributed joins and skew handling. |
| IDs and snapshots | State the dictionary/ID domain and snapshot identity at boundaries. Raw IDs from unrelated shards cannot be compared as terms. Do not embed assumed global meaning in local `u64` values. | Dictionary translation/global identity policy and a consistent cross-partition read epoch. |
| Storage and durability | Keep index format, scan/seek, publication and WAL contracts separate from placement and transport. Version persisted representations independently of CPU-specific layouts. Preserve existing recovery/replica contracts. | Shard placement, tiering/object storage, consensus, fencing, atomic cross-partition writes and failover. These require explicit protocols, not an extension of the local revision counter. |
| Resource scheduling | Separate semantic work from its placement. Describe CPU/memory requirements, bounded tasks and data locality; observe NUMA placement and contention. | Per-node/tenant admission, cluster scheduling, elastic repartitioning and distributed backpressure. |
| Hardware kernels | Preserve stable input/output semantics with capability-selected implementations and a portable fallback. Include transfer, conversion, setup and scratch costs. | Additional SIMD/ISA paths, accelerator batches or offload where end-to-end measurements justify them. |
| Large/intermediate data | Prefer bounded batch or cursor contracts; document pipeline breakers. Do not require all intermediates to be a single resident decoded vector. | Spill-aware operators, remote batches and tier-aware execution where workload evidence warrants them. |
| Cancellation and observability | Keep operation identity, cancellation, resources and errors explicit across nested work. Reuse existing request/tracing facilities. | Network deadline translation, cancellation propagation, retry/idempotency and distributed tracing; local `Instant` values are not wire timestamps. |

A hardware or remote backend must not introduce per-row virtual dispatch, serialization,
network checks or extra copying into the local fast path merely to satisfy an interface.
Use existing boundaries first, concrete specialisation where useful, and coarse operator
or batch interfaces where an extension needs one. Do not generalise every internal type.

Scale-out correctness also constrains optimisation: OWL equality, recursive rule closure,
provenance and validation can cross partitions. Future exchanges cannot declare locally
complete work globally complete; partitioning and convergence semantics need their own
design. Local atomicity and cancellation work in P1 should expose clean boundaries without
pretending to solve distributed atomicity or termination.

**Readiness check:** during P0/P8 review, walk through a partitioned join, a cross-partition
reasoning update, and an accelerator-backed kernel using the proposed boundaries. List
which existing contracts suffice and which future decisions remain. Require no move of
semantic ownership or replacement of the local executor just to introduce those backends.
This is a design exercise, not three unused adapters committed to production.

## 4. Work packages, in dependency order

### P0. Establish the baseline and make contract status unambiguous

**Deliverable:** one reproducible base, a workload matrix across all levels, and an accurate map of
binding invariants, approved exceptions, migration stages and future targets.

Reconcile the pending DL merge through its maintainer before baselining. Record the exact
commit, build settings, machine, allocator, data hashes, correctness results and raw-run
locations. Reuse [the existing suite](../../benches/README.md),
[fast cases](../../benches/fast/cases.toml), and guarded wins in
[performance.md](../design/performance.md); do not build a competing harness.

Inventory the cached and derived state: result parts, QL schema/witnesses, compiled rules,
shapes, DL ontology/bounds, statistics, text and vector indexes. For each, identify its
owner, input identity, access scope, lifetime, invalidation trigger and accounting. Store
the durable explanation in its owning design, not in a second permanent checklist.

Map the current configuration and capability detection to each strategy decision. Name
missing controls, conflicting defaults and settings whose configured value differs from
the execution path using it. Define configuration precedence and automatic/forced modes
before adding algorithms. Reuse existing settings where they already express the choice.

Correct the stale execution-core dependency/fallback description and contradictory status
claims. Preserve historical benchmark evidence, dated and tied to its commit. An accepted
contract can change only through an explicit replacement decision, not a status edit.

**Exit:** named baselines and guards, known failures recorded rather than hidden, and a
reviewable list of genuine v2 blockers versus later improvements. No speed claim yet.

### P1. Close cross-component contract gaps

Implement as three independently reviewable changes after small failing reproductions.

| Change | Owning boundary | Acceptance |
|---|---|---|
| Atomic closure for equality splits and revived classes | Reasoner computes the complete change; store runs gates; engine publishes it once | Merges, splits, deletions and revival have one visible revision with the correct closure. SHACL sees the final prospective state. Abort/cancellation publishes nothing; recovery and replicas see the same revision. |
| Content-correct QL caches | Engine identity; SPARQL cache semantics; store supplies the proper view | Equal-count replacements, several pending states, rollback, access restrictions and concurrent committed readers never share incompatible cached answers. Warm hits remain on unchanged eligible views. |
| End-to-end resource propagation | Store operation policy; existing execution/DL budget mechanisms | Cancel during a running decision or QL data check, not just before entry. Nested branches share the remaining deadline and total envelope; rejection, cancellation and errors release reservations. Unknown remains unknown, never a false negative. |

For equality, combine the atomic repair with the already planned B3a cost fallback where
appropriate: stop pathological maintenance and recompute the affected part **inside the
same transaction**. The old post-commit repair path should disappear for this case.
Bounded work must not be restarted from scratch without charging the earlier work.

Do not expand this into the full persistent equality backend. The fallback's affected
scope, thresholds and unavoidable whole-schema cases need recorded evidence. Correctness
checks belong at their semantic owner; store integration checks cover publication and
composition, and a small HTTP check covers transport cancellation.

Expose the maintenance choice and its reason in existing diagnostics: incremental work,
bounded affected-region recomputation, or the explicitly justified larger fallback.
Automatic selection must obey the same correctness contract as each forced strategy.

### P2. Prepare once, then report and execute the same preparation

**Owners:** `nrese-sparql` with store orchestration.
**Starting points:** `native::{ql_report, ql_stage, native_pattern, optimise}` and
`nrese-store::query_executor`.

Introduce or refine an immutable preparation result containing the rewritten form, its
completeness evidence and planning information for the bound view. Reporting and execution
consume that result. Cache only the parts whose dependencies are known; schema-only
preparation is different from data-dependent witness checks.

The request's current cancellation and resources remain live inputs, not cached state.
Do not create a second executor or turn EXPLAIN into an extra full evaluation. Keep output
and status attached to the same snapshot throughout streaming.

Keep reusable logical preparation separate from physical choices affected by available
resources and current load. Physical-plan reuse must validate its strategy dependencies;
result-cache reuse must retain its semantic identity without needlessly losing valid hits.

**Remove:** duplicate rewrite/optimise work and temporary conversions made unnecessary by
this preparation boundary. **Guard:** one preparation and one set of data checks for the
same operation; unchanged answers, plans and completeness under QL negation, GRAPH, access
restrictions and pending reads. Measure cold preparation separately from warm execution.

### P3. Keep DL bound results in IDs and bound work before collecting it

**Owners:** SPARQL owns typed ID results; store owns lower/upper orchestration; exec owns
generic set/join primitives. Start at `dl/query.rs` and `query_executor::evaluate_prepared`.

- Short-circuit a lower-bound true `ASK` before evaluating the upper bound on eligible paths.
- Compare lower and upper results using ID columns, useful order and existing set kernels.
  Replace debug-string sorting used for gap deduplication with an explicit row operation.
- Produce and decide the gap in bounded batches. Enforce candidate and memory limits
  during production, not after collecting every candidate. Any omitted candidates keep
  the answer/status incomplete; exact mode must not present a truncated set as complete.
- Reuse an evaluated lower result for delivery where it is already available. Avoid a
  further evaluation simply to serialize the same answer.

An ID is meaningful only with its dictionary and computed-value context. Lower/upper
views must share or explicitly translate that context; equal raw integers from different
query-local computed tables are not equal terms. Preserve bags, unbound values, ordering,
LIMIT semantics, graph visibility and exclusion of internal Skolem names.

**Guard/counters:** upper evaluations skipped for true `ASK`; decoded terms and formatted
gap keys; peak live candidate bytes; evaluations per answer; correct results against the
existing term-based path during development. Exercise empty, equal, tiny and large gaps,
computed expressions, duplicates and every completeness outcome. Remove the replaced
production path when the migrated slice passes; keep an independent oracle.

### P4. Reuse compiled DL inputs without sharing speculative search state

**Starting points:** `dl/entailment::{entails, nonempty}`, `dl/query::decide`, and
`dl/upper::Stack`. **Risk:** higher than P2/P3; stage independently.

Measure ontology cloning, normalisation, clause compilation and search separately. Trial
an immutable base plus per-test assumptions or overlays, with local interners/trails where
needed. Existing class-probe or worker reuse should be assessed before inventing another
session abstraction. Reusing a base is safe only when added assumptions preserve or
correctly update its normalisation and global restrictions.

Use detected fragment, query shape, known bounds and available resources to route among
eligible rule, context/consequence-based and tableau services and their search strategies.
Preserve exactness/completeness constraints. A portfolio needs a charged shared work
budget, a measured advantage over choosing one route, and a reproducible forced mode.

Propose an ADR if pure entailment reductions should move from store into OWL/DL. The store
would retain query-shape adaptation, graph policy, routing, budgets and answer completeness;
the lower owner would receive storage-independent assumptions. Until that decision, the
current placement is authorised and should not be "fixed" by moving whole files downwards.

Measure U1's three `BTreeSet` indexes and snapshot copying. Reuse a lower-level relation
representation only if its lifetime, update pattern and scans fit and an A/B wins. U1
must remain an invisible derived upper bound, never ordinary inferred data. Likewise,
monotone rule equality and rollback tableau equality retain distinct state semantics.

**Exit:** fewer full-input copies/compilations across a candidate batch; bounded memory at
several worker counts; the same consistency, entailment, classification and proof results.
Adversarial consecutive tests must prove assumptions do not leak between searches.

### P5. Improve execution kernels where their consumers spend time

**Owner:** `nrese-exec`, with engine/SPARQL/reasoner adapters kept in their own crates.

| Candidate | Source and proposed check | Keep only if |
|---|---|---|
| One forward-search primitive | `exec/search.rs`, `exec/join.rs::gallop`, `sparql/native/wcoj.rs` | Boundary/duplicate/UNDEF semantics remain correct; specialised comparators inline; probes and wall time do not regress. |
| Fewer table copies | `exec/table.rs`: projection clones columns, sorting gathers new columns | Consuming projections or narrow borrowed views eliminate measured copies without excessive lifetimes or retained backing memory. |
| Better sort selection | `exec/table.rs` and `exec/sort.rs` | Real key-width, skew, already-sorted and small-input cases justify the choice; copying into the kernel costs less than the gain. |
| Bounded intermediates | Tables, hash builds, row permutations and operator buffers | Reservations cover capacity and coexistence of inputs/output before allocation; release tracks ownership and error paths. |
| CPU specialisation | Packed decoding, intersections, sort and vector distance only where profiles point | Scalar/original results agree, supported architectures have a fallback, and end-to-end gains survive dispatch and allocation costs. |

Start with work counters, bytes moved and allocation profiles. Then inspect release code,
cache/branch misses and memory bandwidth on the relevant machine. Compiler vectorisation
comes before intrinsics or assembly; any new unsafe code needs stated invariants and the
appropriate focused Miri/sanitizer coverage. Preserve existing safe fallback paths.

Do not replay rejected experiments without a changed premise: the performance log already
rejects broad radix adoption in rule morsels/pairs and identifies no meaningful sort/search
hotspot in the sampled DL workloads. Equal source shape is not evidence of equal cost.

### P6. Measure storage amplification and memory by lifecycle

**Owner:** `nrese-engine`. Inspect `index/{merge,compaction,keys,derive}.rs`,
`engine/{transaction,bulk,spill}.rs`, and `term/{dictionary,pending}.rs`.

The current signed merge costs O(k) per distinct key for k runs and has a single-run fast
path. Measure actual k, decoded keys, tombstones and read/write amplification before
trying a heap/loser-tree alternative. A more complex structure may lose for small k.
Compaction must retain visibility, exact-delta and snapshot-lifetime invariants.

Audit pending-snapshot construction, bulk-load spill buffers, dictionary growth, derived
indexes and old snapshots together. A quad-spill budget alone does not describe process
memory. Record which memory is live payload, cache, retained snapshot or allocator reserve.
Dictionary retention after abort is accepted: any reclamation redesign needs a separate
WAL/ID-lifetime decision, not deletion of apparently unused entries.

**Guards:** old snapshots survive compaction; scans agree with a simple model; term IDs stay
deterministic across thread counts and spilling; checkpoint/WAL replay and replicas agree.
**Measurements:** load/restart, bytes per visible fact, bytes rewritten per update, pinned
snapshot memory and small-update p50/p99 under reader load. Keep the existing minimal-sort
permutation derivation and sharded interning wins.

### P7. Bound parallel work across requests and nested engines

**Policy owner:** store; server supplies transport deadlines; lower engines expose controls.
Inspect `http/result_stream.rs`, `dl/query::decide`, `nrese-dl::classify::Workers`, the
context engine's pools, and bulk spilling's dedicated pool.

Measure queue time, active threads, allocations per worker and nested parallel work under
1/2/4/8/... clients and mixed writes. Avoid assuming every private pool is redundant: the
spiller's separate pool prevents a documented wait/deadlock pattern.

Align configured worker limits with the pool actually executing candidates. If contention
is demonstrated, use bounded operation-level admission or reuse an appropriate pool;
reserve per-worker scratch within the operation envelope. Preserve small-operation latency
and cancellation while avoiding one request consuming all cores under concurrency.

**Exit:** better throughput or tail latency for the stated mix with no single-client
regression; total memory and worker counts remain within the configured policy. Allocator
comparisons must run through the same production configuration, not compare a DL example
on the system allocator with the server on mimalloc and attribute the difference to search.

### P8. Finish the physical-plan migration in useful slices

**Owner:** `nrese-sparql`; follows P2 and the existing [query-plan design](../design/query-plan.md).

Move execution onto plan nodes only where a slice has complete semantics, useful physical
properties and a tested route. Carry ordering, cardinality, uniqueness and resource costs
so operators can avoid redundant sorting, hashing and materialisation. Keep current fast
paths: metadata counts, group walks, sideways probes, eager aggregation, LIMIT pushdown,
component closures and cyclic worst-case-optimal joins.

Each migrated operator retires its old production implementation or unnecessary adapter
in the same package. Mixed migration may temporarily retain lowering for unconverted
nodes; explicitly name that remainder and its retirement condition. The reference evaluator
remains independent. Do not create a parallel permanent executor to make comparison easy.

**Exit per slice:** differential agreement including bags/errors/negation/order, retained
route guards, fewer conversions or intermediate rows, and no performance regression.
Do not start fine-grained result-cache invalidation merely to accompany this migration.

### P9. Make the result understandable to a future maintainer

This is part of every package, followed by a final consistency pass, not cleanup postponed
until all algorithm changes finish.

- Split by responsibilities and lifetime boundaries. The main candidates include the
  6,198-line native executor, rule batch/evaluation modules, store service and DL query
  orchestration. File size locates a review target; smaller files alone are not the result.
- Unify duplicated SPARQL numeric promotion in expression/aggregate paths within the
  SPARQL owner. Retain XSD parsing/value-space ownership and OWL datatype semantics; they
  are not interchangeable with SPARQL error, coercion or aggregation rules. Guard mixed
  types, invalid lexical forms, overflow, NaN, unbound values and aggregate errors.
- Keep public facades narrow. Avoid forwarding layers, one-implementation traits and
  universal contexts unless they enforce a useful boundary or remove real duplication.
- Comments explain invariants, reasons, complexity, lifetime, unsafe assumptions and the
  reference behind a non-obvious algorithm. Remove narration of obvious code and obsolete
  migration commentary. Do not shorten names or omit error context to reduce line counts.
- Tests prove distinct behaviours. Retain independent semantic oracles and cross-component
  contract tests; consolidate duplicated fixtures and examples only when coverage remains.
  A lower-layer change breaking an integration invariant is legitimate: it is not, by
  itself, evidence that the integration test belongs in the lower crate.
- Reconcile the code-guideline claim about tests with the actual commit/push tiers. Keep
  fast deterministic guards in normal checks and benchmarks in separate batches. Ensure
  the owner-level integration checks run before a package is declared complete.
- Finish the planned focused unsafe review, dependency-version review and typed-error
  cleanup where touched; avoid an unrelated dependency upgrade sweep.

**Maintenance acceptance:** a reviewer can locate the owner, trace an input to its work and
failure outcome, identify the performance guard and reproduce the measurement without
reading this conversation. Each new abstraction names its callers and what it replaces.

## 5. Coverage and verification

The inspection is a system-level source review with targeted hot-path reads, not a claim
that every line was audited or that runtime failures were reproduced. Preserve coverage
outside the initial optimisation targets:

| Area | Plan treatment | Acceptance surface |
|---|---|---|
| Engine/storage and execution kernels | P1, P5, P6 | Model/differential tests, recovery, snapshot identity, work/allocation guards |
| SPARQL planning, evaluation and caches | P1, P2, P3, P5, P8 | Reference evaluator, W3C/Jena where available, bags/errors/order/access |
| Rule materialisation, deletes and equality | P1, P5, P6 | Exact closures, provenance, merge/split churn and tail work |
| OWL normalisation, DL bounds and search | P1, P3, P4, P7 | W3C, differential/metamorphic cases, ORE tiers, proofs and honest budget outcomes |
| RDF IO, syntax, XSD and JSON | Preserve; P9 only at an actual shared-semantics boundary | Parser fuzz/conformance/round-trip suites and load throughput |
| SHACL | Preserve its semantic owner; P1 checks final-state gate composition | Incremental versus full validation and rejection atomicity |
| Text/vector and persisted derived indexes | Regression coverage; kernel changes only if profiled | Recall/correct filtering, cold/warm rebuild, load and RDF+vector latency |
| Server, replication, backup and clients | P1/P7 boundary checks, no API redesign | Deadlines, cancellation, streaming, replicas/restart, existing client compatibility |
| Console and CLI | Keep transport/presentation boundaries; no cosmetic rewrite | Existing typecheck and public-behaviour tests when their contracts change |
| Documentation, build/test gates and benchmarks | P0/P9 | One current owner per question, working links, accurate status and reproducible runs |

### Measurement matrix

Use existing case names from the registries where present. Add missing focused cases to
those registries rather than another benchmark tool.

| Track | Representative checks | Report separately |
|---|---|---|
| Plain SPARQL | DBpedia/YAGO/BSBM mixes; cycles, paths, grouping, strings, eager aggregates, large results | Cache off/on, cold/warm; preparation, first row and full result; latency and peak memory |
| QL | NPD, realised/missing witnesses, negation, graph restrictions and pending changes | Rewrite/data-check counts, branches, allocations and answer completeness |
| RL/updates | LUBM/OWL2Bench, deep hierarchies, merges/splits, clique/layered deletes | Bindings/probes/facts touched; p50/p95/p99 and peak live bytes |
| DL | `dlmode-lubm1`, candidate-gap shapes, number/nominal/datatype cases, W3C and ORE | Solved/wrong/unknown/timeout counts, compilation versus search, clone bytes, tail latency |
| Storage | Load/spill/thread scaling, restart, tombstones, compaction and pinned snapshots | CPU/I/O time, bytes per fact, write amplification and retained memory |
| Concurrency | `clients-sweep`, `writes-under-readers`, `cache-replay`; cancellation during work | Queue/service time, throughput, p99, worker count and total accounted/physical memory |

Follow [PROTOCOL.md](../../benches/PROTOCOL.md): checked answers before timings, interleaved
release builds on the same machine, three repetitions for checks and ten behind published
claims, cold/warm separation, recorded noise and a blind holdout for tuned corpora. Run
hardware-sensitive claims on the designated Linux benchmark machine and verify supported
CPU/platform fallbacks. Do not compare unlike reasoning regimes or exclude slow/timeout
cases from a reported gain.

The existing protocol has aggregate regression and reporting thresholds. For this proposal,
also inspect every representative case: a repeatable regression blocks an optimisation,
even if a sum improves. If evidence is below resolution, label it inconclusive; do not keep
additional complexity on a performance claim. Counts can establish removed work, but do
not alone establish lower latency. Structural simplification may be accepted as neutral
with that distinction recorded. Avoid inventing numerical speed targets before baseline data.

Datasets stay once, read-only, in the shared dataset location; record hashes and share
manifests, not copied corpora. Stop owned containers/processes and remove disposable stores
and scratch after use, retaining planned rerun inputs and reusable build caches. Report
reclaimed space. No large datasets or raw licensed-system results enter the repository.

## 6. Documentation changes and deletion discipline

| Document | Intended update when the relevant work lands |
|---|---|
| `ARCHITECTURE.md` | Current ownership, identity/resource flow and atomic publication; accepted refinements only |
| ADRs | Explicit replacements/exceptions and rationale; retain historical decisions with supersession links |
| `design/execution-core.md` | Remove retired fallback and wrong dependency claims; distinguish shared kernels from semantic executors |
| `design/query-plan.md` | Completed migration slices, remaining lowering and retirement conditions |
| `design/ql-rewriting.md` | View-correct caches, single preparation and real cancellation propagation |
| `design/owl2-dl.md` | Agreed exact-service boundary, bound representation, limits and reuse lifetimes |
| `design/reasoner-v2.md` | Atomic split/revival path, cost fallback, explicit large-schema cases |
| `design/performance.md` | New measured wins and guards; retain rejected ideas and historical baselines |
| `STATUS.md`, coverage plan, v2 checklist and capability matrix | Consistent implemented/partial/proposed/deferred state, with guard or evidence references |
| `CONTRIBUTING.md`, code guidelines and ops/config docs | Actual gates and ownership workflow; configuration only once it works |

This proposal remains a temporary coordination document. As packages are accepted, their
decisions belong in the owners above and their state in the existing status/checklist.
Do not maintain a second architecture or duplicate the performance log here.

Every implementation package names the old function/path/representation or repeated work
it removes. If old and new must coexist during migration, state why, the affected callers
and the condition that removes the old path. Independent test oracles, protocol adapters,
specialised state lifetimes and measured cache representations are not redundant merely
because they resemble another implementation.

Retain multiple algorithms when each wins for a meaningful input/resource region. Their
selection policy and shared semantics need one owner; the algorithms themselves are not
redundant simply because they compute the same answer.

## 7. Suggested sequence and discussion decisions

1. **First batch: P0 and P1.** Agree scope, reconcile the base and establish the cross-path
   contracts. Three bounded repairs rather than a broad architecture rewrite.
2. **Second batch: P2 and P3.** Remove repeated preparation and bound-query work. These have
   direct source evidence and narrow measurable acceptance criteria.
3. **Profile-led batch: P4/P5/P6/P7.** Prioritise by measured end-to-end cost, bytes and tails;
   independent owners can work in parallel on separate branches and shared read-only data.
   Pool/resource changes require coordination across owners.
4. **P8 in bounded slices**, after P2; bring forward a slice only if it removes a measured
   bottleneck or materially simplifies a needed change. P9 accompanies every batch.
5. **Final integration:** the repository's milestone gate, performance comparison, soak,
   documentation reconciliation and maintainer review. The maintainer merges and pushes.

Each package supplies: problem and owner; current contract; work to remove; expected cost
change; correctness guard; performance result or explicit lack of one; deleted old paths;
documentation changes; residual limitations. No calendar estimate precedes the baselines.

Also report the automatic selection rule, supported overrides, decision overhead and
hardware/distributed extension assumptions. The sequencing is about dependencies and
evidence, not assigning one workload permanent priority over the others.

Decisions for discussion:

- Keep major structural work after the v2 merge, as the roadmap says, or bring specific
  packages forward when needed for the contract fixes?
- Accept the distinction between required correctness repair, performance-neutral
  simplification and measured optimisation? A blanket speedup promise would be misleading.
- Pursue a small pure exact-test API below the store after P4's evidence, or keep that
  responsibility in the store and improve its internal modularity first?

Confirmed direction: performance at every level, configuration first, automatic strategy
selection from request/data/resource needs, and architecture that later distributed and
hardware work can extend. Suggested start: P0/P1 first, P2/P3 next; retain the existing
crate structure and measured specialisations. Decide larger abstractions from the evidence
those steps produce. No implementation starts merely because it appears in this proposal.
