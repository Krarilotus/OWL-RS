# ADR-0009: OWL 2 DL reasoning

Status: accepted (2 October 2026). The owner gave the direction ("a complete engine first,
then the fast layers in front of it"; [the roadmap](../plan/2026-10-02-roadmap.md))
and left the way to it to the project. Revised the same day after seven research reports
and their check (outside the repository: `output/dl-research-A…G.md`,
`output/OWL-RS-dl-research-review-2026-10-02.md`); the revisions are marked below. The
owner's direction (2 October):
OWL 2 DL is in scope and is where NRESE means to lead: "the core feature catalogue to be
exceptional at reasoning, and the widest research field to draw from for improvements and
clever design techniques".

## Context

What NRESE reasons with today ([reasoner v2](../design/reasoner-v2.md)):

- **OWL 2 RL, RDFS, OWL Horst, user rules,** materialised in the store and kept current on
  every commit (delta executor, truth maintenance), with consistency gates, explanations of
  inferences and of rejects, equality by representatives. This is sound but incomplete for
  OWL 2 DL: disjunction, existentials that create anonymous individuals, cardinalities and
  most of nominals are beyond any rule set.
- **OWL 2 EL classification** (`v2/classify.rs`): the completion rules of Baader, Brandt and
  Lutz with ELK's normalisation. Axioms outside EL are skipped and counted.

What users of an ontology platform ask that this can't answer:

1. **Classification** of an OWL 2 DL ontology: every subsumption between named classes,
   the unsatisfiable classes (Protégé's "classify" with HermiT, Konclude, Pellet).
2. **Consistency** of TBox and ABox together, with a justification when it fails.
3. **Instance queries** with certain answers: `?x a :C` answered by everything that is a
   `:C` in every model, not only what rules derive.
4. **Explanations** as users of Protégé know them: the minimal sets of axioms an
   entailment follows from (justifications), and a readable proof.

How the field does it, and what an RDF store can take from it:

| Approach | Systems | Strength | Weakness |
|---|---|---|---|
| (Hyper)tableau | HermiT (Motik, Shearer and Horrocks, JAIR 2009; Glimm et al., JAR 2014), Pellet, FaCT++ | complete for SROIQ (all of OWL 2 DL); hypertableau avoids much of the don't-know nondeterminism of plain tableau | classification needs many satisfiability tests; large ABoxes are expensive |
| Tableau with saturation-based optimisations, parallel | Konclude (Steigmiller, Liebig and Glimm, JWS 2014) | the fastest in the ORE competitions (Parsia et al., JAR 2017) | intricate; a decade of engineering |
| Consequence-based calculi | ELK (Kazakov, Krötzsch and Simančík, JAR 2014) for EL; for Horn and disjunctive DLs Simančík, Kazakov and Horrocks (IJCAI 2011); for SRIQ Bate et al. (JAIR 2018); for SROIQ Sequoia (Tena Cucala, Cuenca Grau and Horrocks, 2019) | classification in one saturation, "pay as you go": as fast as ELK on the EL part, polynomial where the ontology is Horn | young beyond EL; ABox reasoning is not its strength |
| Modular combination | MORe (Armas Romero, Cuenca Grau and Horrocks, ISWC 2012), locality-based modules (Cuenca Grau et al., JAIR 2008) | the EL part by an EL reasoner, only the rest by a DL reasoner | gains depend on the ontology's shape |
| Datalog bounds with DL for the gap | PAGOdA (Zhou, Cuenca Grau, Nenov, Kaminski and Horrocks, JAIR 2015) | certain answers over large ABoxes: a datalog lower bound and upper bound, the DL reasoner only for answers between them, on a small fragment | needs a materialising datalog engine to begin with |

NRESE has what PAGOdA builds on: a fast, incremental, materialising datalog engine inside
the store. That is the lever.

## Decision

Reasoning in OWL 2 DL is added in layers that share one ontology model, each usable on its
own and each measured against the systems above:

1. **One OWL model from RDF.** The OWL 2 structural model (class expressions, property
   expressions, axioms of SROIQ, with datatypes) read from the store's triples by the
   reverse of the OWL 2 RDF mapping, with diagnostics for ill-formed input, and normalised
   into DL-clauses. The EL classifier's normaliser becomes a client of it.
2. **A complete engine: hypertableau.** Satisfiability and consistency for all of SROIQ
   with DL-clauses, anywhere blocking and individual reuse (HermiT's design), written for
   NRESE's ids and memory layout. It is the correctness anchor: every faster path is
   checked against it, and it answers what nothing faster can.
3. **Classification that scales.** *(Revised.)*
   - **One context-saturation core** (Bate et al., JAIR 2018; Sequoia, Tena Cucala et al.), not separate Horn, SRIQ and SROIQ engines. Its rule families switch on step by step: Horn, then disjunction, cardinality and equality, then nominals.
   - **The EL saturation stays a separate fast path.** Sequoia needed about 131 s on SNOMED, ELK 2 s.
   - **The hypertableau handles what the core leaves open.** The handover is decided *dynamically*, by the measured growth of clauses and equalities, not only by syntax; the telemetry for it (contexts created, equality literals, the widest clause head, …) is there from the first day.
   - **Commits:** additions are incremental. Deletions are incremental while their dependency cone is bounded; otherwise the affected module is saturated again. Incremental TBox deletion beyond EL is an open research problem, so no generalisation of Kazakov and Klinov (ISWC 2013) is claimed. Every path is checked against a clean rebuild.
   - Parallel across contexts.
4. **Certain answers in the store (PAGOdA's scheme).** *(Revised.)* A reasoning mode `owl2-dl`.
   - **Bounds:**
     - the OWL 2 RL closure is the persistent lower bound L, as today;
     - a candidate upper bound U1 comes from a datalog relaxation of the ontology (disjunctions read as conjunctions, existentials as fresh constants);
     - U1 is materialised in a stack of its own and is **never visible as inferred data**;
     - tighter bounds (PAGOdA's U2/U3, RSA approximation, summarisation) are computed per query, where a query needs them.
   - **Completeness status:** every answer carries `sound`, `complete`, the lower and upper counts, and `unresolved`. A hypertableau is a complete oracle for consistency, entailment and atomic instance queries. It is **not** one for arbitrary conjunctive queries (cyclic ones, or ones whose non-projected variables are witnessed by anonymous individuals). So:
     - an exact query service names the class of queries it is complete for, as part of its public contract;
     - answers in the gap outside that class stay `unresolved`;
     - non-monotone operators (`MINUS`, `NOT EXISTS`, aggregates) get exact answers only when the gap is closed.
   - **Consistency of commits** is checked the same way, the upper bound first.
5. **Explanations.** *(Revised.)*
   - **One proof format** for RL, EL and DL steps, whose every step maps back through the normalisation to the *source OWL axioms*, not to clauses or bare triples.
   - **Justifications** through one API: `one`, `top-k`, `core`, `union` and lazy `all`.
   - **Methods:**
     - PULi-style enumeration (Kazakov and Skočovský) over the derivations NRESE already records, for RL and EL now, before any DL code;
     - pinpointing over consequence-based derivations;
     - black-box minimisation (Kalyanpur et al., ISWC 2007) as the anchor for the hypertableau.
   - **Internals:** the backjumping dependency sets are kept apart from the proof ids.
6. **Evidence.** *(Revised.)*
   - **The protocol comes before the first number:**
     - **Reference runners:** KoncludeCLI, not through OWLLink (the out-of-memory failures in Lam et al. 2023 came from that adapter); HermiT, Openllet and ELK through an OWL API runner of our own for timing, not ROBOT.
     - **Correctness:** checked through a canonical taxonomy and realisation format; disagreements recorded as `DISPUTED` and settled, never by a majority vote.
     - **Runs:** cold and warm reported apart, single-thread and whole-host tables; timeouts and out-of-memory failures counted as censored runs.
     - **Corpora:** fetched by hash and never committed.
   - **Gates, staged:**
     1. the W3C OWL 2 conformance suite (direct semantics);
     2. parity with HermiT on ORE 2015 (1,751 classifications, 1,881 consistency checks in Lam et al. 2023);
     3. Konclude's 1,862 of 1,920 classifications, as the final bar.
   - **Query answering:** instance queries on LUBM and UOBM against PAGOdA; OWL2Bench DL.
   - **The full corpus** runs on the Draco cluster.
   - **Testing the engine:** grammar-based SROIQ(D) fuzzing, with metamorphic tests.
   - Other systems' numbers are published only where their licences allow.

The order is the list's: the model, then the complete engine (correctness first), then
classification speed, then certain answers in the store, then justifications. Each step
ships behind its own reasoning mode or endpoint (`/classification` gains `profile=dl`;
`owl2-dl` as a reasoning mode once step 4 lands); OWL 2 RL stays the default.

## Consequences

- The existing explanation and diagnostics surfaces carry over: the same proof format for
  RL, EL and DL steps.
- Reasoning becomes two-tiered by design: materialised rules for what must be fast on
  every commit, a complete engine for what must be right. The configuration chooses per
  repository.
- The hypertableau engine is a long-lived component (HermiT took years): the first
  milestone is correctness on the ORE corpus, not speed. Speed comes from the
  consequence-based layer and from never asking the complete engine what the bounds
  settle.
- Benchmarks of reasoning (the [reasoning benchmark design](../design/reasoning-benchmark.md))
  gain RT tasks for DL classification and certain answers.

## Decided

- **`owl2-dl` answers (the owner, 2 October):** completeness is always reported; certain answers are the default only where a complete path exists.

## Open for the owner

- Whether SWRL (DL-safe rules) belongs with step 2 or later. The plan's default: later.
