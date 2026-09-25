# Vision and Scope

## Product vision

NRESE is a single-node RDF database that **reads like QLever and governs data like GraphDB**:
- QLever-class query speed and scale: dictionary-encoded permutation indexes, merge and worst-case-optimal joins, integrated text search, fast bulk loading.
- GraphDB-class semantics and governance: queryable RDFS/OWL 2 RL materialisation with incremental maintenance, SHACL validation on commit, transactional updates whose cost follows the change, not the dataset.

Its first users are ResearchSpace (adapted to NRESE, ADR-0006) and the datamodel workflow, which exports reviewed, provenance-carrying data into it.

## In scope

- Own storage engine with MVCC snapshots, WAL durability and compaction (ADR-0001, ADR-0002)
- SPARQL 1.1 query, update, protocol, Graph Store Protocol, federation; RDF4J protocol
- Rule-based reasoning (RDFS, OWL-Horst, OWL 2 RL, OWL 2 QL materialisable profiles) with truth maintenance (ADR-0003)
- SHACL Core and SHACL-SPARQL as a commit gate (ADR-0005)
- Full-text search, GeoSPARQL, vector similarity
- Operational tooling: backup/restore, metrics, tracing, access control

## Out of scope (for now)

- Full OWL 2 DL reasoning (tableaux). The v1 `owl-dl-target` scaffold was removed; OWL 2 RL is the reasoning target.
- Distributed clustering and high availability.
- A proof-explanation UI beyond derivation trees in API responses.

## Guiding constraints

- Native Rust for all runtime paths; reuse standards-grade crates where they aren't a differentiator (ADR-0001).
- Result-based error handling; no panic-driven control flow.
- The ownership, functional-design and DRY principles in [ARCHITECTURE.md §6](../ARCHITECTURE.md#6-design-principles).
- Inferred data is always distinguishable from asserted data.
- Every capability claim is backed by evidence recorded in the [capability matrix](06-target-capability-matrix.md).

## Success criteria

See [ROADMAP.md §1](../ROADMAP.md#1-goal-and-how-well-know-were-there).
