# ADR-0004: What NRESE learns from for performance and for semantics

Status: accepted (2026-09-25); framing revised 2026-11: other systems are named as inspiration, not as targets.

## Context

v1 measured itself against Fuseki. Since then the store takes its performance ideas from QLever's design and its semantics and governance features from the RDFS/OWL 2 RL materialising stores.

## Decision

- **Performance, after QLever's design:**
  - load throughput and index size per triple
  - latency on WDQS/WDBench-style query mixes
  - integrated text search
  - streaming of very large results
- **Semantics and governance:**
  - queryable RDFS/OWL 2 RL materialisation with incremental maintenance
  - SHACL on commit
  - full-text search
  - transactional updates with sub-linear cost
  - the named-graph workflows ResearchSpace uses
- Fuseki stays usable as a *correctness* reference for plain SPARQL protocol behaviour, but not as a target.
- The harness uses engine-neutral "reference endpoint" profiles. Results are recorded per target in the capability matrix (`docs/spec/06-target-capability-matrix.md`).

## Consequences

- Specs and the gap tracker are rewritten against these targets.
- Benchmarks run on shared hardware with published datasets, so others can repeat them.
