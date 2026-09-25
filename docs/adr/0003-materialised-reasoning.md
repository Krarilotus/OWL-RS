# ADR-0003: Materialised, incrementally maintained reasoning with asserted/inferred separation

Status: accepted (2026-09-25)

## Context

v1 (`rules-mvp`) had three limits:
- It re-reasoned over the full dataset on every write.
- It ignored literals, blank nodes and named graphs, so Turtle `owl:propertyChainAxiom ( … )`, `owl:unionOf` and `owl:AllDifferent` never reached it.
- It discarded all inferences: the read model was `asserted-only`.

GraphDB's value is the opposite: forward-chaining materialisation that is queryable and maintained incrementally.

State of the art:
- semi-naive datalog evaluation
- parallel materialisation and `owl:sameAs` rewriting (RDFox: Motik et al., AAAI 2014, JAIR 2015)
- incremental maintenance by DRed and Backward/Forward (Motik, Nenov, Piro, Horrocks, AAAI 2015 / AIJ 2019)

## Decision

- Reasoner v2 is a datalog engine over `TermId`s:
  - rule sets are data: RDFS and OWL 2 RL, with GraphDB-compatible names where sensible
  - semi-naive evaluation
  - union-find `sameAs` rewriting
- Inferred quads live in a separate inferred layer maintained by the reasoner. Queries see `asserted ∪ inferred` by default, with an explicit switch for asserted-only.
- Consistency rules produce rejects with derivation-based explanations. These are rules with `false` heads: disjointness, `owl:Nothing`, `differentFrom`, irreflexive/asymmetric properties, property disjointness.
- Deletes are handled by Backward/Forward, the published algorithm behind GraphDB-style `isSupported` checks: a retracted consequence survives if another derivation still supports it. DRed is the fallback for rule shapes where B/F's backward step is too expensive.
- The inferred layer is a separate index stack with the same layout as the asserted one, not per-entry flags (roadmap decision D2).
- `rules-mvp` stays available until v2 passes its fixture suite, and is then removed.
- The `owl-dl-target` mode is rejected at startup until it exists, instead of silently skipping validation.

## Consequences

- Write-time reasoning cost becomes proportional to the consequences of the delta.
- Memory grows with the materialised inferences, as in GraphDB. The size is reported in stats.
