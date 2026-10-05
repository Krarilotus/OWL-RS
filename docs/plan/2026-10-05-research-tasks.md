# Research tasks after the first coverage-and-performance batch (5 October 2026)

For the owner's paper studies and the research agent, which sorts the results into the
literature wiki. Each task names what NRESE needs to decide or build with it. Where it
says "per layer", the layers are those of [design/performance.md](../design/performance.md):
layout, evaluation, parallelism, incremental maintenance, equality, machine level.

## Where knowledge is missing

1. **Scaling across machines** (coverage plan §6): no design yet for partitioning,
   distributed query evaluation, distributed reasoning, replication and failover.
2. **How in-memory leaders get their speed** (RDFox first; the owner's target is to beat
   it with less memory): what is published, what is measured, under which
   configurations.
3. **Store size** below QLever's: which layout and codecs, and at what scan and lookup cost.
4. **OWL 2 QL**: NRESE materialises; existentials on the right of `SubClassOf` aren't
   materialised, so certain answers through them are missing. Query rewriting (or a
   combined approach) is not designed.
5. **Classification and realisation** at Konclude's level (package 3.4).
6. **Incremental reasoning** at high update rates, against RDFox's and GraphDB's methods.
7. **Machine-level kernels** for the hot paths the profiles show (sorting wide keys,
   membership probes in sorted arrays, intersections).

## General tasks

**G1. Fast rule reasoning, the state of the art per layer.**
- Systems: RDFox, VLog and Trident, Nemo, Soufflé, DDlog and DBSP, GraphDB's engine,
  Inferray, WebPIE.
- For each: data layout, evaluation strategy, parallelism, incremental maintenance,
  equality, memory per fact, and its published benchmarks with configurations.
- Needed for: beating RDFox (memory `owl-rs-beat-rdfox`), the paper's related work.

**G2. Distributed RDF storage and reasoning.**
- Partitioning: hash, semantic, graph-based and workload-aware.
- Distributed query processing: TriAD, AdPart, WORQ, Wukong (RDMA), Virtuoso Cluster.
- Distributed datalog: BigDatalog; distributed RDFox (Ajileye, Motik and Horrocks).
- Replication, high availability and consistency for knowledge graph stores.
- Needed for: the design of G6 (coverage plan §6), the Draco runs.

**G3. Compact data structures for RDF.**
- Formats and stores: HDT, k²-triples, QLever's permutations and vocabulary.
- Codecs and indexes: compressed tries, integer codecs (SIMD-BP128, FastPFor,
  partitioned Elias-Fano, Roaring), FSST, learned indexes.
- Each with its decode cost in scans and in lookups.
- Needed for: P2 (store size and serving memory), the hierarchical block layout of
  research designs §3.

**G4. OWL reasoning engines: classification, DL, and QL.**
- Classification and DL: Konclude (saturation-based caching, parallel classification),
  HermiT (hypertableau, caching), ELK, Sequoia, MORe and PAGOdA hybrids.
- OWL 2 QL: query rewriting (Ontop, Rapid, Presto) against materialisation.
- Needed for: packages 3.4 and 4.x, the QL gap above.

## Specialised tasks

**S1. Lock-free parallel against sort-based materialisation.** RDFox (AAAI 2014) against
columnar and sort-based engines (Nemo, VLog): contention, determinism, memory per fact.
Can partitioned hash indexes be combined with radix-sorted deltas?

**S2. Worst-case-optimal and free joins in rule evaluation.**
- The joins: Leapfrog Triejoin, Generic Join, Free Join (SIGMOD 2023), factorised
  representations.
- Where they pay for OWL RL rules (mostly binary joins) and for SPARQL.

**S3. Incremental maintenance theory.** DBSP (VLDB 2023), differential dataflow,
Forward/Backward/Forward and backward/forward chaining (Motik et al.), counting under
recursion: which suits which update rate and ruleset.

**S4. Machine-level kernels.**
- Sorting and search:
  - radix sorts of 128- to 256-bit keys (IPS2Ra, Regions sort, vqsort with Highway);
  - Eytzinger and B-tree layouts for probing sorted arrays (Khuong and Morin);
  - software prefetching.
- Intersections: SIMD set intersection (Lemire et al.), branchless galloping.
- Builds: profile-guided optimisation and BOLT for Rust, hardware dispatch.
- Each against the profile of a LUBM 1000 materialisation.

**S5. Equality at scale.** `owl:sameAs` in linked open data (sameAs.cc, Beek et al.),
union-find with rollback, rewriting with late expansion in query answering.

**S6. Reasoning larger than memory.** Out-of-core datalog (VLog on disk, Trident),
memory-budgeted semi-naive evaluation, spilling and compressing working sets.

**S7. Datatype reasoning in DL.** The OWL 2 datatype map, FaCT++'s intervals, HermiT's
datatype manager, `owl:real` and `owl:rational`, satisfiability with counts.

**S8. Benchmark methodology.**
- Protocols: LDBC SPB's audit rules, ORE's protocols, OWL2Bench.
- Fair configurations: equal resources against each system's best configuration.
- Memory measures: resident against committed against proportional.
- Statistics: interleaving, confidence intervals.
- Needed for: the fair benchmark on Draco.

**S9. GPU acceleration of joins and datalog.** GPU datalog engines and hash joins: when
the transfer pays for itself.

**S10. Vector search with knowledge graphs.** Filtered approximate nearest neighbours
(ACORN, Filtered-DiskANN), RaBitQ: for G5 with filters from SPARQL patterns.
