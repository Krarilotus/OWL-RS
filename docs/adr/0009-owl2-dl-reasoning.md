# ADR-0009: OWL 2 DL reasoning

Status: proposed (2 October 2026), for the owner's review. The owner's direction (2 October):
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
3. **Classification that scales.** A consequence-based calculus (Horn first, then
   disjunctions and number restrictions after Bate et al., then nominals after Sequoia),
   with the EL saturation as its fast path and the hypertableau engine for the classes it
   leaves open (MORe's split, decided per module). Incremental on TBox commits (the
   approach of Kazakov and Klinov, ISWC 2013, for EL, generalised), parallel across
   contexts.
4. **Certain answers in the store (PAGOdA's scheme).** A reasoning mode `owl2-dl`: the
   OWL 2 RL closure as the lower bound (as today); an upper bound from a datalog
   relaxation of the ontology (disjunctions read as conjunctions, existentials as fresh
   constants), materialised in a stack of its own; answers in the upper but not the lower
   bound checked by the hypertableau engine on the fragment of the ABox they depend on.
   Queries in `owl2-dl` give certain answers; the explain endpoint says which were
   settled how. Consistency of commits is checked the same way (the upper bound first).
5. **Explanations.** Proofs from the consequence-based derivations and the hypertableau
   traces (as the RL explanations are today), and justifications: minimal axiom sets,
   found by pinpointing over the recorded derivations and minimised with the engine
   (Kalyanpur et al., ISWC 2007, for the black-box part).
6. **Evidence.** Classification and consistency of the ORE 2015 corpus against HermiT,
   Konclude and ELK (correctness against HermiT's results, run through ROBOT in Docker);
   instance queries on LUBM and UOBM against PAGOdA; OWL2Bench DL. Each layer lands with
   its corpus green and its numbers published (other systems' numbers only where their
   licences allow).

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

## Open for the owner

- Whether `owl2-dl` query answering should be certain answers by default once it exists,
  or opt-in per query (it can be slower on queries whose bounds differ).
- Whether SWRL (DL-safe rules) belongs with step 2 or later.
