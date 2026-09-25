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

## Refinements (2026-09-25, reasoning plan)

The plan in [design/reasoner-v2.md](../design/reasoner-v2.md) makes these decisions concrete. None of them reverses the decision above.

- **Behaviour is configured, not hard-wired** (decision D7, [design §2.2](../design/reasoner-v2.md)).
  - Ruleset, timing (`commit`/`deferred`/`on-demand`), consistency handling, inference placement, sameAs and maintenance strategy are repository settings. The read model and explanations are per request.
  - The fixed invariants are: separate and disjoint stacks, atomicity of whatever a commit reasons, and inferences that can't be written directly.
- **Default timing: reasoning runs inside the commit.** The inferred delta is committed with the asserted delta under one revision, and the WAL carries both, so recovery never re-reasons. TBox changes over large extensions run as visible, cancellable reasoning jobs instead of silently rematerialising.
- **The inferred stack is disjoint from the asserted one** (`inferred = Mat(P, asserted) \ asserted`). With the default placement it holds one graph, so three permutations suffice; per-graph placement uses the quad layout.
- **The TBox is compiled before the instance fixpoint.** Its closure is computed first, instance rules are specialised into dispatch tables, and RDF-list axioms are compiled into fixed-arity rules. An outer fixpoint keeps this exact when instance rules feed the schema.
- **There are two executors for one compiled program:**
  - a sorted, vertically partitioned, parallel batch executor (sort-merge and Leapfrog Triejoin), used for loads and large deltas
  - an index-nested-loop delta executor over engine views, used on the commit path
- **Recursive shapes go to modules:** hierarchy, transitivity, equality and symmetry, following Hu, Motik and Horrocks, AAAI 2018 / AIJ 2022.
- **Maintenance order:**
  1. DRed first, as the correctness baseline.
  2. B/F and FBF as the default.
  3. Counting only if measurements require it, because it needs persistent per-fact state.
- **`owl:sameAs`:** facts are stored over representatives and expanded when read.
- **Explanations are recomputed by backward proof search, not stored.**
- **The storage part moves into M1 as E6**, before any v2 on-disk data exists.
