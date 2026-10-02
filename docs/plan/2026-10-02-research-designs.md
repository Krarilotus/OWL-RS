# Designs from the October 2026 research: equality, vectors, compression, graph access

Four questions were researched in depth on 2 October 2026 (deep-research reports by the
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

## Order of work

1. Equality stage B with class ids, strict and canonical answers (the reasoning
   pipelines' largest lever), with the torture benchmark.
2. Store layout: measurement, hierarchical blocks, FSST vocabulary.
3. Graph access control (needs the reasoner's support sets, shared with equality's
   support tracking).
4. Vector search, from the algebra and filter model outward.
