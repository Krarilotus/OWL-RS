# Vision and Scope

## Product vision

NRESE is an RDF database and ontology platform that **reads like QLever and governs data like GraphDB**, and means to lead in reasoning:
- QLever-class query speed and scale: dictionary-encoded permutation indexes, merge and worst-case-optimal joins, integrated text search, fast bulk loading.
- GraphDB-class semantics and governance: queryable RDFS/OWL 2 RL materialisation with incremental maintenance, SHACL validation on commit, transactional updates whose cost follows the change, not the dataset.
- Reasoning as the lead feature: from the materialisable profiles to OWL 2 DL (classification, consistency, explanation), drawing on the widest research field there is for it.
- Hardware-aware and scalable: it uses every core, the memory, the CPU features and GPUs of the machine it runs on, and scales out to clusters where a use case gains from it; each derived setting can be overridden.
- One consolidated engine API: the protocols and the one frontend are thin translations of the core's operations.

Its first users are ResearchSpace (adapted to NRESE, ADR-0006) and the datamodel workflow, which exports reviewed, provenance-carrying data into it; both must work end to end. Beyond them, the connectors reach as many users as possible (GraphDB Workbench and RDF4J clients, Jena, rdflib, Protégé).

## In scope

- Own storage engine with MVCC snapshots, WAL durability and compaction (ADR-0001, ADR-0002)
- SPARQL 1.1 query, update, protocol, Graph Store Protocol, federation; RDF4J protocol
- Rule-based reasoning (RDFS, OWL-Horst, OWL 2 RL, OWL 2 QL materialisable profiles) with truth maintenance (ADR-0003)
- SHACL Core and SHACL-SPARQL as a commit gate (ADR-0005)
- Full-text search, GeoSPARQL, vector similarity
- OWL 2 DL reasoning (classification, consistency, explanation), with the materialised profiles where they suffice
- Explanation of inferences and of rejected commits, in the API and in the frontend
- One frontend for every role (reading, editing, administration), on the same engine API as the connectors
- Hardware acceleration (SIMD, GPUs) and scale-out to clusters, configurable per use case
- Operational tooling: backup/restore, metrics, tracing, access control

## Out of scope (for now)

Nothing of the above. Until 2 October 2026 this list excluded OWL 2 DL reasoning, clustering and an explanation UI; the owner put all three in scope (the v1 `owl-dl-target` scaffold stays removed: DL reasoning is designed anew).

## Guiding constraints

- Native Rust for all runtime paths; reuse standards-grade crates where they aren't a differentiator (ADR-0001).
- Result-based error handling; no panic-driven control flow.
- The ownership, functional-design and DRY principles in [ARCHITECTURE.md §6](../ARCHITECTURE.md#6-design-principles).
- Inferred data is always distinguishable from asserted data.
- Every capability claim is backed by evidence recorded in the [capability matrix](06-target-capability-matrix.md).

## Success criteria

See [ROADMAP.md §1](../ROADMAP.md#1-goal-and-how-well-know-were-there).
