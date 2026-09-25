# ADR-0004: Parity targets are QLever (performance) and GraphDB (semantics), not Fuseki

Status: accepted (2026-09-25)

## Context

v1 measured itself against Fuseki. The product goal is now a store that reads like QLever and governs data like GraphDB.

## Decision

- **QLever parity (performance):**
  - load throughput and index size per triple
  - latency on WDQS/WDBench-style query mixes
  - integrated text search
  - streaming of very large results
- **GraphDB parity (semantics and governance):**
  - queryable RDFS/OWL 2 RL materialisation with incremental maintenance
  - SHACL on commit
  - full-text search
  - transactional updates with sub-linear cost
  - the named-graph workflows ResearchSpace uses
- Fuseki stays usable as a *correctness* reference for plain SPARQL protocol behaviour, but not as a target.
- The harness uses engine-neutral "reference endpoint" profiles. Results are recorded per target in the capability matrix (`docs/spec/06-target-capability-matrix.md`).

## Consequences

- Specs and the gap tracker are rewritten against these targets.
- Benchmarks must run on shared hardware with published datasets, so the numbers are comparable to the QLever/GraphDB literature.
