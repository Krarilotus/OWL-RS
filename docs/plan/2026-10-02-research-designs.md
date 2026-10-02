# Designs from the October 2026 research: equality, vectors, compression, graph access, execution, reasoning, transactions

Seven questions were researched in depth on 2 October 2026 (deep-research reports by the
project owner, plus our own reading of the primary papers). This page records what NRESE
takes from them, the decisions, and the order of work. Each design keeps the project's
rule: tuned defaults, every use-case-dependent trade-off configurable, no hard-coded
limits.

## 1. `owl:sameAs` and equality at Wikidata and LOD scale

**Evidence.** Motik, Nenov, Piro, Horrocks, *Handling owl:sameAs via Rewriting* (AAAI
2015): replacing every member of an equality class by a representative cut materialised
triples by up to 7.8× (OpenCyc 1.18 B → 142 M), rule derivations by up to 46×, and
materialisation time by 2.3–31×; the gain is mostly fewer redundant derivations, not
fewer stored triples. Two correctness traps are named there: rules must be rewritten
too (constants in rule bodies), and answers expanded after evaluating on representatives
are wrong for bag semantics and for builtins such as `STR` unless expansion happens
before them. The incremental side is *Combining Rewriting and Incremental Materialisation
Maintenance for Datalog Programs with Equality* (IJCAI 2015, B/F≈): deleting an equality
splits a class, and facts merged earlier must be restored. GraphDB 11 implements the same
idea (one node per class, explicit statements kept, only affected classes rebuilt on
delete); Stardog keeps an eagerly maintained sameAs index used at query rewriting;
Virtuoso expands at query time. RDFox 7.0 (January 2024) replaced its optimised equality
handling by explicit rules, without a published reason. LOD reality (Raad et al., ISWC
2018; sameAs.cc): 559 M `sameAs` edges, 49 M classes, the largest class 177,794 terms,
whose pairwise closure alone would be 31.6 billion pairs; erroneous bridge links create
such giants.

**Design** (extends the existing W4 stages A–D of the
[completion plan](2026-09-30-completion-plan.md)):

- A stable internal class id per equality class, apart from the union-find root and from
  the term shown in answers (display representative configurable: smallest id,
  first-seen, source priority). A merge then never rewrites indexes by representative
  name; redirects are flattened by compaction.
- The asserted quads stay as they are (exact terms, graph). The inferred stack holds the
  closure over class ids (stage B). Without the originals, a split can't be undone.
- Equality edges are stored with their supports: asserted (graph) and derived (functional
  and inverse-functional properties, keys, rules), so a deletion of an ordinary fact can
  retract a derived equality.
- Subject, predicate and object are all normalised, and rule constants with them.
- Merges: union-find (union by size, path halving), then the merge fed to the reasoner as
  a delta (new joins become possible). Deletes: drop a support; if the edge goes, check
  connectivity inside the old class (spanning forest, non-tree edges); on a split,
  rebuild the affected canonical facts from the asserted ones and re-derive (B/F style).
  Fully dynamic connectivity only if profiles demand it.
- Bulk loads build classes by sorting the equality edges and computing components (the
  Hogan et al. pipeline), not by billions of single unions.
- Queries join on class ids; expansion to member names happens at the barriers that
  observe names or multiplicities (`STR` and other term functions, `DISTINCT`, grouping
  and aggregates, `ORDER BY`, projection in strict mode). Two answer semantics, chosen
  per store and query: `strict` (as if the full closure existed) and `canonical` (one
  name per class: analytics). `?x owl:sameAs ?y` is a virtual relation over the class
  rosters, quadratic only in what it outputs.
- The planner gets both canonical and expanded cardinalities (class sizes).
- Configuration: `reasoner.equality = off | axiomatic | rewrite`, `equality.answers =
  strict | canonical`, `equality.scope = dataset | named-graph`, representative policy;
  memory budgets with spilling, an error (never a silently partial closure) past them.
- Benchmarks: UOBM and Claros-like data (RDFox's), LDBC SPB, a sameAs.cc-shaped identity
  graph, and our own torture test (class-size distributions up to 10^6 members, chain /
  star / mesh topologies with the same closure, merges, redundant deletes, bridge
  deletes splitting two 100 k components, strict `COUNT`/`DISTINCT`/`STR` queries).

## 2. Vector similarity search

**Evidence.** Index families and their regimes: exact SIMD scans below about 10^5–10^6
candidates (and after selective filters); HNSW for RAM-resident dynamic data;
Vamana/DiskANN (MIT reference code) for 10^8–10^9 vectors on NVMe with compressed
navigation vectors and exact re-ranking, FreshDiskANN's base-plus-delta for updates;
IVF with interchangeable codecs (SQ8, PQ, RaBitQ: Apache-2.0 reference, now in Faiss);
CAGRA/cuVS (Apache-2.0) as GPU accelerator; Matryoshka embeddings for truncated
dimensions. Filtering is the systems problem: post-filtering needs about k/s candidates
for selectivity s; ACORN, Filtered-DiskANN and NHQ push predicates into traversal.
QLever's QUIVER (September 2026) exposes a native ANN index through a virtual `SERVICE`
and reports up to 355× over non-indexed evaluation; Stardog pushes selective ids from
graph patterns into its semantic search. Runtimes for local embedding models: `ort`
(ONNX Runtime, MIT/Apache-2.0) as default, `candle` as pure Rust, `fastembed-rs`;
models with permissive licences include BGE-M3 (MIT), Nomic Embed v1.5 and Arctic Embed
L v2.0 (Apache-2.0); model licences are checked apart from runtime licences.

**Design:**

- A logical `VectorTopK` operator in the algebra (not a scalar function), reached through
  a virtual `SERVICE nrv:search { … }` with the graph patterns outside it; the planner
  derives an accept set of term ids from those patterns and chooses per query: filter
  then exact scan, filter-aware ANN, or ANN with adaptive over-fetching and post-filter.
  `nrv:exact`, `nrv:searchBudget`, `nrv:recallTarget` let a query pin the trade-off.
- Index = navigation (flat | HNSW | Vamana | IVF) × codec (f32 | f16 | SQ8 | binary |
  PQ | RaBitQ) × re-rank (none | f16 | f32), behind one Rust trait with an accept-set
  predicate in its search options.
- Embeddings as views derived from RDF (fields, languages, template version, model hash),
  re-embedded only for terms whose sources changed; vector generations tied to snapshots
  (`strict` or `eventual` freshness); immutable memory-mapped segments plus a mutable
  delta and tombstones.
- Several named vector spaces per term (text, KG-structure, image, custom); scores fused
  by rank (reciprocal rank fusion), never added raw.
- Presets: `embedded`, `interactive-ram`, `ssd-billion`, `write-heavy`, `filter-heavy`,
  `gpu-throughput`, `ultra-compressed`, `cluster`, and `auto` from data size and hardware.
- Order: algebra and the shared filter and storage model; exact scan and HNSW; SQ8 and
  RaBitQ codecs; Vamana on disk; GPU and experimental quantisers last.
- Benchmarks: ANN-Benchmarks for recall/QPS curves, BigANN '23 filtered and streaming
  tracks, and our own RDF+vector benchmark (filter selectivity 10^-6–1 from triple
  patterns, stars and paths; recall@k, latency percentiles, build time, peak memory,
  bytes read per query, freshness).

## 3. Smaller stores at full scan speed

**Evidence.** Measured here: palettes alone save 8.6 % of Wikidata lexemes' store with
queries even, 3.3 % of DBpedia core's with queries 7 % slower
([suite results](../reviews/2026-10-02-suite-results.md)); now `store.index_encoding =
"compact"`. QLever's current writer stores a permutation relation by relation (the
leading key per block in metadata, two columns per row), compresses each id column of a
block with Zstd, and compresses its vocabulary with FSST applied twice (FSST: MIT).
Integer codec literature: SIMD-BP128 and SIMD-FastPFOR decode fastest at good ratios;
partitioned Elias-Fano for long sparse monotone lists; roaring for dense sets.

**Design and order:**

1. Measure first: bytes per permutation attributed to the leading key, the second key
   and the rest, against QLever's files on the same data.
2. A hierarchical block layout: distinct leading keys with offsets, distinct second keys
   per leading key with offsets, then the terminal sorted lists (HDT and QLever exploit
   the same redundancy). Expected to be the largest index saving; it changes probes from
   O(1) per key to a bounded decode per 128-value mini block (restart points).
3. Patched FOR beside FOR, palette and deltas, chosen per block by a cost function of
   bytes, scan and lookup time (weights per profile), not bytes alone.
4. Vocabulary: FSST-compressed, individually addressable strings with compact offsets;
   the string-to-id index apart. FSST once vs. twice measured.
5. Profiles `fast | balanced | compact | network` over these, the store readable on any
   hardware (decoders chosen at start: portable, AVX2, AVX-512, NEON).

## 4. Access control by named graph

**Evidence.** Stardog, Virtuoso, MarkLogic and AllegroGraph restrict the dataset a user
evaluates against; GraphDB's forward-materialised implicit statements lose the graphs
of their premises; Oracle documents why a single dominating label is imprecise with
alternative derivations. Survey: Kirrane et al., SWJ 2017.

**Design** (as agreed with the project owner on 2 October):

- Roles map to graph IRIs or prefixes (a user's queryable dataset); policies kept as
  transactional server metadata, importable and exportable as RDF; identity and roles
  from tokens, rights resolved on the server; deny by default, explicit deny wins.
- Enforcement is a restriction of the dataset before evaluation, pushed into the
  graph-aware scans (the mechanism protocol datasets already use): forbidden graphs are
  absent, `GRAPH ?g` binds readable graphs only, aggregates count readable solutions
  only; reading a forbidden graph looks like reading an empty one.
- Inferred statements: visible when at least one derivation has all premises readable.
  The reasoner records per inferred statement its support graph sets (minimal sets,
  shared and interned); a configurable compatibility mode shows all inferences.
- Updates: the WHERE part reads with read rights; every target needs write rights; a
  forbidden target aborts the whole update. `SERVICE` is a separate privilege.
- Caches keyed by the security context and a policy epoch.

**Status (2 October).** Delivered: the policy as a file per server (roles → graph IRIs
and prefixes, explicit deny, default graph, deny or allow by default), the dataset
restriction before evaluation, writes refused as a whole whatever the data holds, query
cache keyed by the access, SPARQL, Graph Store and RDF4J paths, and the policy granting
reads and graph-scoped writes to its roles. Inferred statements are all-or-nothing per
policy (`inferred = "hidden" | "visible"`) until the reasoner records support graph sets
(§6). Open: policies as transactional metadata importable as RDF, `SERVICE` as a separate
privilege.

## 5. Query execution and optimisation

**Evidence.** HyPer, Umbra and DuckDB run morsel-driven, vectorised pipelines with explicit
pipeline breakers; Free Join (2023) shows binary joins win on most acyclic queries and
worst-case-optimal joins on cyclic ones, a hybrid matching or beating both. Characteristic
sets and pairs (RDF-3X lineage) estimate star and chain cardinalities far better than
independence; sampling covers the rest. Factorised representations keep `A × B` as its two
inputs, and aggregates over them need no enumeration (`COUNT(A × B) = |A|·|B|`, sums
scaled by multiplicities). EXISTS and subqueries decorrelate into semi- and anti-joins.
Sparqloscope (ISWC 2025, Wikidata truthy, 8 B triples): QLever 2.3 s geometric mean over
105 queries, Virtuoso 10.1 s, MillenniumDB 23.3 s; most systems struggle below 10 B.
MillenniumDB plans property paths with the rest of the query; reachability indexes and
path-cardinality statistics help `P31/P279*`.

**Design:**

- Executor: columnar id batches through morsel-driven pipelines; materialisation where a
  breaker needs it (sort, hash build), not per operator. A first step is in: GROUP BY
  over a basic graph pattern streams in morsels when it outgrows memory, GROUP BY over a
  cross product groups in chunks.
- Optimiser over whole algebra regions (groups, OPTIONAL, subqueries, paths, EXISTS as
  semi- and anti-joins), choosing per node the join family (merge, hash, index probe,
  leapfrog, semi-join reduction, factorised) and the representation (flat, stream,
  factorised).
- Characteristic sets and pairs, with sampling where they don't apply; plans re-optimised
  at pipeline breakers when the observed cardinality is far off.
- An aggregate algebra over products and joins (decomposable aggregates, multiplicities),
  checked against SPARQL's bag semantics, errors and unbound values: BSBM BI q4's 155 M
  rows need not exist.
- Paths planned with the rest of the query, from their bound end; reachability labels for
  hierarchies.
- Benchmarks: Sparqloscope, WatDiv, BSBM BI, the Wikidata query logs.

## 6. Parallel incremental reasoning

**Evidence.** Hu, Motik and Horrocks (2018/2019): counting is nearly always worth it;
which recursive repair wins depends on the workload (1,000 deletes: B/F with counting 1.2 s
against DRed with counting 179 s on UOBM, but 247 s against 10.5 s on SSPE), and
rematerialising wins from a few percent of deletes where they invalidate much of a
recursive closure. Modular materialisation avoids generic rule firings. RDFox reached
6.1 M derived triples/s and 87× speed-up on 128 cores at 36.9 bytes per triple. RDFox 7.0's
removal of equality rewriting has no published reason; the likely one is the cost of a
special global rewrite path against MVCC reads, explanations and stratification.

**Design:**

- Maintenance chosen per rule module and commit by estimated impact: B/F or DRed with
  counting for small commits, rematerialisation of the affected modules for large ones;
  `maintenance = auto | bf-count | dred-count | remat` to pin it.
- Support counts always; derivation records (witnesses, support graph sets) as the
  provenance level chosen (`count | witness | acl | full`): the ACL design (§4) needs
  `acl`.
- Equality (§1) keeps logical identity apart from physical canonicalisation: stable term
  ids in storage and provenance, a per-epoch canonical map as an accelerator that old
  snapshots keep until no reader needs them.
- One serialised closure per commit with many workers inside it; leapfrog joins for cyclic
  rule bodies, binary joins otherwise.
- An incremental benchmark of our own: the same commits as 1 to 1 M inserts and deletes,
  plus pathological ones (a bridge in a transitive closure, a sameAs class split, a fact
  with many supports, a schema change).

## 7. Transactions, loading and scale-out

**Evidence.** QLever builds Wikidata (21 B triples) in about 5 h with about 20 GB RAM into
about 500 GB, with a vocabulary merged from per-batch vocabularies. RDFox needs 45–85 bytes
per fact in memory. RocksDB groups commits for one fsync; Virtuoso updates in place;
QLever's delta triples suit small deltas.

**Design** (NRESE's run structure is already a delta-main LSM):

- Commit sequence numbers for snapshots; group commit (`durability = sync-low-latency |
  sync-grouped | async | bulk-build`, a 0.5–5 ms window) for many small transactions;
  `write_isolation = snapshot | serial-update`.
- Bounded deltas: compaction debt as a metric that throttles writers; long-lived snapshots
  reported and optionally capped.
- Bulk load: per-batch sorted vocabularies merged into dense ids (bounded memory, the
  measured gap: the dictionary during loads), ids stable afterwards (new terms from a
  delta dictionary), permutations built by external sort.
- Targets: full Wikidata in 4–8 h within 24–32 GB RAM into 500–750 GB; at least 10,000
  small durable commits per second with group commit; reads under an update stream at
  most 10–20 % slower (p95).
- Scale-out after standalone and replicated modes: subject-owned shards with all
  permutations locally, a coordinator for atomic updates, an entity-stream mode for
  Wikidata-style replays.

## Order of work

1. Equality stage B with class ids, strict and canonical answers (the reasoning
   pipelines' largest lever), with the torture benchmark.
2. Optimiser regions and decorrelated EXISTS, then characteristic sets, then the
   aggregate algebra over products (§5).
3. Store layout: measurement, hierarchical blocks, FSST vocabulary; the vocabulary merge
   for bounded loads (§3, §7).
4. Support counts and provenance levels in the reasoner (§6), then graph access control
   on them (§4).
5. Group commit and commit sequence numbers (§7).
6. Vector search, from the algebra and filter model outward (§2).
