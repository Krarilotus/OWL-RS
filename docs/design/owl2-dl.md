# OWL 2 DL reasoning: the engineering design (3 October 2026)

**What this is.** The engineering design behind [ADR-0009](../adr/0009-owl2-dl-reasoning.md):
for each component, its data structures, its algorithms in order, how it is tested, and
which optimisations it gets at which level. It is the plan for phases 2 to 4 of the
[roadmap](../ROADMAP.md).

**Sources.**
- Seven research reports, A to G (2 October; outside the repository in `output/dl-research-A…G.md`):

  | Report | Subject |
  |---|---|
  | A | the benchmark bar |
  | B | consequence-based against hypertableau |
  | C | certain answers |
  | D | tableau engineering |
  | E | justifications and proofs |
  | F | the benchmark protocol |
  | G | the field |

- Their check against primary sources (`output/OWL-RS-dl-research-review-2026-10-02.md`), whose corrections are applied here: C's ORE figures, A's Konclude out-of-memory target, the Rust field.

Where a figure below is a measurement, its source says so. Figures from before 2018 are
marked † (historical: right for design choices, not for what is fastest today).

**The contract in one paragraph.**
- **Tasks:** NRESE answers consistency, class satisfiability, classification, realisation and axiom entailment for OWL 2 DL (SROIQ(D), the direct semantics), soundly and completely, or reports why it didn't.
- **Result classes:** `complete`, `incomplete`, `unsupported`, `timeout` and `memory` are distinct, never folded into one another.
- **Queries:** a SPARQL query under `owl2-dl` reports its completeness: certain answers where the plan has a complete path, and otherwise sound answers with the lower and upper counts and the unresolved candidates (the owner's decision of 2 October).
- **Explanations:** every entailment can be explained back to the source OWL axioms, as justifications (minimal axiom sets) and as readable proofs.

## 1. Architecture

```text
 store triples (any graphs)
        │  reverse OWL 2 RDF mapping, diagnostics          ── nrese-owl
        ▼
 OWL structural model (hash-consed, axiom ids → source)    ── nrese-owl
        │  NNF, structural transformation, RBox automata,
        │  absorption, clausification, profile flags        ── nrese-owl
        ▼
 DL-clauses with provenance (one IR for every engine)
        │
        ├─ module extraction (⊥-locality, atomic decomposition) and dispatch
        │
        ├─► EL saturation (exists, v2/classify.rs; becomes a client of the model)
        ├─► context-saturation core: Horn → +disjunction/cardinality/equality
        │                             → +nominals          ── nrese-dl
        └─► hypertableau with datatype theory (the anchor) ── nrese-dl
                │
 classification and realisation driver (known/possible subsumers)
        │
 in the store (owl2-dl mode): lower bound L as inferred statements,
 candidate upper bound U1 in a stack of its own, gap → module →
 tighter bounds → exact services, completeness on every answer   ── nrese-store
        │
 proof IR → justifications (one, top-k, core, union, all), proofs ── nrese-owl/nrese-dl
```

**Rule one (reports A, B and D agree; the strongest signal of the seven):** the leaders
win by avoiding the general search. ELK, MORe, Konclude's saturation front-end and
PAGOdA's bounds all send only the hard residue to the complete engine. In Konclude's
ablation, coupling saturation with its tableau took the accumulated time over its test
repositories from 44,910 s to 9,589 s† (D). So the complete engine is the correctness
anchor and the hedge against the cases saturation can't do. The fast paths carry the load.

**Rule two:** every engine works on the same clauses with the same provenance. A proof can
then combine EL, context-saturation and hypertableau steps, and a justification is always
a set of source OWL axioms (B, E).

**Rule three:** correctness infrastructure before engine code (review §3, gaps 2 and 3).
Nothing is optimised that can't be checked: every optimisation has a switch, and "on" and
"off" must give the same answers (D).

### Crates

| Crate | Holds | Depends on |
|---|---|---|
| `nrese-owl` (new) | The structural model, the reverse RDF mapping and its diagnostics, normalisation to DL-clauses, profile analysis, module extraction, the proof IR | `nrese-rdf` only: no store, no engine; reusable and testable alone, as `nrese-vector` is |
| `nrese-dl` (new) | The context-saturation core, the hypertableau, the datatype theory, classification and realisation, justification search | `nrese-owl`, `rayon` |
| `nrese-reasoner` | RL and rule reasoning as today; its EL classifier moves onto `nrese-owl`'s model | `nrese-owl` |
| `nrese-store` | The `owl2-dl` mode: bounds, the candidate stack, exact services, the completeness status, the commit gate | all of the above |

Everything is NRESE's own code, MIT OR Apache-2.0. No code comes from Horned-OWL (LGPL), the
OWL Explanation library (LGPL) or HermiT (LGPL). Papers are the specification; permissive
references (PULi, Apache-2.0) may be read.

## 2. The OWL structural model (`nrese-owl`)

### Types

The OWL 2 structural specification, interned, so that equal expressions share one id
(hash-consing; A's rank-four lever: one canonical IR in which subexpressions, role automata
and clausified restrictions are shared).

```text
ClassId(u32)        named class, owl:Thing, owl:Nothing
ObjPropId(u32)      named object property; ObjProp = (ObjPropId, inverse: bool)
DataPropId(u32)
IndividualId(u32)   named or anonymous
DatatypeId(u32), LiteralId(u32)

ClassExpr (interned as ExprId(u32)):
  Class | Not(E) | And([E]) | Or([E]) | OneOf([Ind])
  Some(R, E) | All(R, E) | HasValue(R, Ind) | HasSelf(R)
  Min(n, R, E) | Max(n, R, E) | Exact(n, R, E)
  DataSome(D, DR) | DataAll(D, DR) | DataHasValue(D, Lit) | DataMin/Max/Exact(n, D, DR)

DataRange: Datatype | DataAnd | DataOr | DataNot | DataOneOf | Restriction(Datatype, [(facet, Lit)])

Axiom (AxiomId(u32)):
  SubClassOf, EquivalentClasses, DisjointClasses, DisjointUnion
  SubObjectPropertyOf (with chains), EquivalentObjectProperties, DisjointObjectProperties,
  InverseObjectProperties, ObjectPropertyDomain/Range, Functional, InverseFunctional,
  Reflexive, Irreflexive, Symmetric, Asymmetric, Transitive
  the data property axioms, DatatypeDefinition, HasKey
  ClassAssertion, ObjectPropertyAssertion, NegativeObjectPropertyAssertion, the data ones,
  SameIndividual, DifferentIndividuals
```

- **Provenance:** each axiom keeps where it came from: its graph, the triples (by the store's term ids) of its RDF form, and the revision. Explanations and module extraction read it.
- **Store ids:** IRIs and literals are the store's `TermId`s through a side table, so results map back to the store without decoding strings. The model is built without decoding the dictionary, except for literals in data ranges.

### The reverse RDF mapping

The mapping from RDF graphs to the structural specification (W3C *OWL 2 Mapping to RDF
Graphs*, §3), as a pass over the store's triples by predicate:
1. Declarations and typing (`owl:Class`, `owl:ObjectProperty`, …, punning allowed as OWL 2 allows it).
2. Class and property expressions from blank-node structures (`owl:Restriction`, `owl:unionOf` lists, `owl:onProperties`, …). Each blank node is read once; one used twice is ill-formed.
3. Axioms, including n-ary ones from `owl:AllDisjointClasses`, `owl:AllDifferent`, `owl:AllDisjointProperties`, `owl:NegativePropertyAssertion`, and axiom annotations through reification (kept as annotations, no logical meaning).

**Diagnostics**, as typed values with the triples involved, never a silent drop:
- ill-formed structures (missing `owl:onProperty`, broken lists, a blank node shared between expressions);
- untyped entities;
- violations of the global restrictions: non-simple roles in cardinalities, `Self`, `owl:disjointWith` of properties, irreflexive and asymmetric; and an RBox that isn't regular. `owl:topObjectProperty` is composite (*OWL 2 Structural Specification* §11.1), so a property above it is non-simple (12 October); `owl:bottomObjectProperty`, composite too, adds no pair to what is above it and isn't counted.

An ontology with violations is still loaded for the parts that are well-formed, and its
answers are `unsupported` for the tasks the violations touch (A: `unsupported` is a result
class of its own).

**The EL classifier** (`v2/classify.rs`) moves onto this model: its own normaliser (ELK's)
becomes a client of `nrese-owl`, so its skipped-axiom count comes from the shared profile
analysis.

### Tests

- **Round trip:** model → RDF (the forward mapping, written as a test helper) → model is the identity, on the W3C OWL 2 test suite's ontologies and on generated ones.
- **Every RDF document of the W3C suite** parses to the expected axiom count, with no diagnostic where the suite says the input is valid, and the expected diagnostics where it isn't.
- The ORE 2015 corpus and the development subsets read without a panic, under a time and memory budget (§11's corpora).

## 3. Normalisation into DL-clauses (`nrese-owl`)

In the order of Motik, Shearer and Horrocks (JAIR 2009)†, with every step recorded:

1. **NNF and simplification:** negation pushed inwards; `Exact(n)` split into `Min` and `Max`; trivial conjuncts and disjuncts removed; `Not(OneOf)` kept as it is.
2. **RBox:**
   - Property hierarchies are closed.
   - Transitivity and chains become non-deterministic automata per role (Horrocks and Sattler's construction for regular RBoxes). `All(R, C)` over a non-simple `R` then propagates along the automaton's states instead of materialising transitive closures.
   - Inverse roles are normalised to `(id, inverse)`.
   - Report A ranks complex RBoxes among the hard cases, and recommends role automata and propagation along the hierarchy over micro-optimising the search.
3. **Structural transformation:** a fresh name for each nested subexpression that needs one, with polarity (Plaisted–Greenbaum style), so that the clausification is linear and Horn structure survives. In JAIR 2009† this is what keeps hypertableau deterministic on Horn parts.
4. **Absorption**, before clausification. It decides which body atom triggers a clause, so it acts as the query planner of the later hyperresolution joins (D):
   - **Role absorption:** domains, ranges and `Some(R, ⊤) ⊑ C` bind to role events, not to every node. This is the strongest isolated measurement in D: FaCT++ on NCI took 2,447.7 s without it and 63.9 s with it, and tableau operations fell from 344,142,434 to 1,580,206†.
   - **Binary absorption:** `A ⊓ B ⊑ C` with a conjunction of atoms as the trigger.
   - **Nominal absorption:** at the named individual.
   - The trigger atom is chosen by cost: a static default (the rarest predicate by the model's counts), later by the store's statistics.
5. **Clausification** into DL-clauses: `U₁ ∧ … ∧ Uₘ → V₁ ∨ … ∨ Vₙ`.
   - Body atoms are `A(x)`, `R(x, yᵢ)`.
   - Head atoms are `A(x)`, `R(x, y)`, `≥ n R.C(x)`, `y ≈ z`, `⊥`, data atoms, and `x ≈ a` for nominals.
6. **Clause metadata,** computed once and kept on each clause (B, C):

   | Field | Meaning |
   |---|---|
   | `id`, `source_axioms[]`, `steps[]` | provenance back to the OWL axioms |
   | `horn` | one head atom at most |
   | `el` | in the EL fragment the EL path handles |
   | `rl` | covered by the OWL 2 RL rules (the lower bound's) |
   | `rsa` | in RSA (RSAComb's tractable class; for later) |
   | `existential`, `equality`, `number`, `nominal`, `datatype` | what it generates or needs |
   | `trigger` | the absorbed trigger atom |

7. **Interning:** clauses are flat slices of atom ids in an arena, with no pointers into the model. Equal clauses are one, with merged provenance.

### The built-in properties and keys (as built, 12 October)

- **`owl:topObjectProperty` (U)** goes through a hub (`universal.rs`): a fresh individual
  `h` every element reaches by a fresh role `u`, so `∃U.C` is `∃u.∃u⁻.C` and `∀U.C` is
  `∀u.∀u⁻.C`. U in a chain, as a subproperty, in an equivalence or an inverse pair is
  `u ∘ u⁻` in the chain: the property above becomes universal through its automaton. U in
  a key is a condition every two named individuals meet (each is U-related to the
  other) and is dropped from the key. A domain or range of U, `R ⊑ U`, and the
  characteristics U has hold. What stays `unsupported` is what OWL 2 DL rules out: U is
  composite (§11.1), so it can't be in a cardinality or be functional, inverse-functional,
  irreflexive, asymmetric or disjoint with another property (§11.2, simple roles).
- **`owl:topDataProperty`** occurs only above another data property in OWL 2 DL (§11.2,
  "Restriction on owl:topDataProperty"); that axiom holds and is left out, any other use
  is `unsupported`.
- **Disjointness after a rewriting:** two operands of `DisjointClasses` or `DisjointUnion`
  that a rewriting makes equal (`∃U.{a}` and `⊤` are both `⊤`) say that the class is
  empty; the operands keep their repeats, and the normaliser reads the pair as `X ⊑ ⊥`.
- **Keys over non-simple properties:** a key's pairs are the property's whole extension
  (Direct Semantics: `(x, z) ∈ (OPE)^OP`), which for a non-simple property includes what its
  chains and transitivity compose. For such a property the key's rule reads, per named
  individual `z`, the fresh name of `∃P.{z} ⊑ Q`, derived deterministically through `P`'s
  automaton (`{z} ⊑ ∀P⁻.Q`); a rule per combination of individuals. A key applies to
  every named individual, also one no assertion mentions: where an ontology has keys, the
  hypertableau's program holds all of them.

### Tests

- **Semantic equivalence,** checked by a model enumerator on small signatures. For random ontologies from the fuzzer (§11), every model of the clauses restricted to the original signature is a model of the ontology, and back: an exhaustive check over domains of up to four elements.
- **Provenance:** every clause has a non-empty `source_axioms`, and removing a source axiom removes the clauses that only it produced.
- **Absorption on and off** give the same entailments (metamorphic, through the engines).

## 4. Modules and dispatch

**Modules.** ⊥-locality modules (Cuenca Grau et al., JAIR 2008)† are computed per task,
for example the signature of a class to classify, or of a query's gap. Over them, an atomic
decomposition (Chainsaw, ReAD) serves as a reusable index of which axioms always travel
together. MORe's numbers show why: on NCI, 94.9% of the signature was EL and the hard
module 15.4% of the axioms (author figures, B). The part needing complete DL reasoning can
be much smaller than the ontology's expressive part.

**Dispatch** decides per module, and per sub-task within it:

| Module | Goes to |
|---|---|
| inside EL (the clause flags say so) | the EL path, always (B: ELK about 2 s on SNOMED against Sequoia's 131 s) |
| Horn, without datatypes | the context core, Horn rules |
| non-Horn, without nominals or datatypes | the context core with disjunction, cardinality and equality |
| with nominals, without datatypes | the context core with nominal rules |
| with datatypes, or when the context core gives up | the hypertableau |

**Dynamic fallback.** The context core gives up a module when its own growth says it should,
not by syntax alone (B, review §3 gap 4). The budgets are configurable and recorded:
- clauses generated;
- equality literals generated;
- the widest head;
- contexts created.

Telemetry is there from the first day (B's list), so that the thresholds come from ORE and
Oxford runs, not guesses:
- `contexts_created`, `contexts_saturated`, `clauses_generated`, `max_clause_head_width`;
- `equality_literals_generated`, `nominal_contexts`, `successor_edges`, `peak_agenda_size`;
- `max_rss`, `cpu_time`, `wall_time`;
- `tableau_fallback_modules`, `fallback_module_axioms`, `proof_nodes`.

**Racing:** for a module both engines could do and neither has a clear signal for, both may
run on separate cores, and the first complete answer wins. B's data supports this: HermiT
solved 35 Oxford ontologies where Sequoia timed out, and Sequoia 13 where HermiT did.

## 5. The context-saturation core (`nrese-dl`)

The calculus of Bate, Motik, Cuenca Grau, Simančík and Horrocks (JAIR 2018) for ALCHIQ+,
which extends through preprocessing to SRIQ. Then Tena Cucala, Cuenca Grau and Horrocks
(AI 2021) for nominals, which takes it to SROIQ without datatypes. **One engine whose rule
families switch on step by step**, not a separate engine per fragment (B, review §3 gap 4).

### Data structures

```text
ContextId(u32)
Context {
    core: ClauseSetId,              // the conjunction the context stands for
    clauses: per-literal indexes    // by max literal under the term order:
                                    // body literal → clauses, head literal → clauses
    succ: [(RoleSig, ContextId)],   // successor links with the roles that led there
    pred: [(RoleSig, ContextId)],
    agenda: Vec<ClauseId>,          // new clauses to process
    root: Option<NominalId>,        // root contexts for nominals (Sequoia)
}
Clause { body: [Lit], head: [Lit], derivation: DerivationId }   // in an arena
```

- **Term order and redundancy:**
  - Clauses are kept modulo subsumption, and tautologies are dropped.
  - The order on literals selects which literal of a clause may resolve (ordered resolution), as Bate et al. do. Without that, the clause sets explode (A rank two: "indexing, ordered inference and redundancy elimination are central").
- **Indexes:** each clause is indexed by its maximal literal, so a new clause meets only the clauses that can resolve with it. There is no global scan (A rank two).
- **Context strategy:** contexts are shared per core (cautious) by default, with eager per-successor contexts as a setting. Bate et al. describe both.

### Rule families, in the order they ship

| Stage | Rules | Covers |
|---|---|---|
| Horn | Core, Hyper, Succ, Pred, ⊥ | Horn-SRIQ: 612 of 779 preprocessed Oxford ontologies were Horn (78.6%; B, Sequoia 2021), and the plain one-pass saturation beat the special Horn algorithm by 1–10× there |
| SRIQ | + disjunctive heads, Eq, Ineq, Factor, Elim, the encoding of `≥ n` as n successors with inequalities and of `≤ n` as equality disjunctions | all of SRIQ; 671 of 703 preprocessed Oxford ontologies in 10 minutes (Bate 2018) |
| SROIQ | + root contexts, Nom, r-Pred | SROIQ without datatypes (Sequoia 2021); behind Konclude on nominal-heavy ontologies, so racing and fallback matter |

**The known weak spot:** `≤ n` with many successors, combined with non-Horn heads, makes
equality disjunctions grow quadratically. Bate et al.'s Pizza example applies one clause
154 times. That is what the fallback budget watches (§4).

### Proofs

Every inference is recorded as `Derivation { rule, premises[], clause }`, deduplicated.
Proofs aren't rebuilt afterwards. E's condition is a tested property, not an assumption:
**for every subset M of the ontology with M ⊨ α, the recorded inferences contain a proof of
α from M.** Otherwise justification search misses justifications.

### Parallelism

**Ownership through activation, messages between contexts, a pool of workers** (ELK's
scheme, *The Incredible ELK*; B; Sequoia 2025 found thread pools beat dedicated
message-passing threads):
- **Activation.** Each context has an inbox and an activation flag. Whoever sends a message to an inactive context sets the flag (compare-and-swap) and schedules it on the pool. Rayon's work stealing shares the active contexts among the workers. A context's state is only ever touched by the one worker holding its activation, so it needs no lock inside.
- **Messages.** Clauses for other contexts go to their inboxes as immutable messages. Messages a context sends itself stay in its own batch.
- **Links.** A link `C →r D` is two messages, a backward link to `D` and a forward link to `C`. Every rule then finds its premises in one context: rules over the link and D's subsumers run at D; chains `B → C → D` meet at their middle context.
- **Termination.** The scope ends when no context is active or has mail.
- **Shared state is immutable:** the clauses, interned ids and the term order.
- **First implemented** for the EL classifier (`classify_parallel`, package 3.1 step 1), checked against the sequential saturation on random ontologies.

Expectations are modest. Konclude went from 1 to 4 workers for 1.20× over its whole corpus,
and 2.94× on the hard FMA case (B, 2014†). Parallelism pays on hard contexts, and cheap
saturations don't amortise coordination.

### Gates

- **Horn stage:**
  - On the EL fragment, the same taxonomy as the EL path and ELK.
  - On Horn ontologies of the W3C suite and the development subset, the same as HermiT and KoncludeCLI.
- **SRIQ stage:** the same against HermiT and KoncludeCLI on the SRIQ part of the development subset; no wrong taxonomy on the Oxford corpus.
- **SROIQ stage:** the same on the nominal ontologies, with the root-context proofs checked by the proof checker (§10).

## 6. The hypertableau anchor (`nrese-dl`)

The complete engine for SROIQ(D): satisfiability and consistency with DL-clauses, by the
hypertableau calculus of Motik, Shearer and Horrocks (JAIR 2009)† with the extensions of
Glimm, Horrocks, Motik, Stoilos and Wang (JAR 2014)†.

**Report D's design rule:** not "a tableau tree of Rust objects", but a transactional,
append-oriented inference machine. Compact ids, arenas, interned syntax, shared dependency
sets, a branch-local trail and event-driven rule queues.

### Data layout

```text
#[repr(C, align(64))]
struct HotNode {                     // expansion and blocking data only: one cache line
    parent: u32, representative: u32, label: u32, first_edge: u32,
    last_edge: u32, todo: u32, existentials: u32, blocker: u32,
    depth: u32, nominal_level: u32, flags: u32, cold: u32,
    blocking_hash: u128,
}
```

- **Facts:** the semantic truth lives in append-only extension tables, not in nodes:

  ```text
  UnaryFact  { concept, node, dep, proof }
  BinaryFact { role, from, to, dep, proof }
  Equality   { from, representative, dep, proof }
  Inequality { a, b, dep, proof }
  ```

  Nodes hold indexes into them (HermiT's split between the extension manager and lazily built labels†).
- **Labels** are three views of the same facts:
  - small sorted inline id lists;
  - dense `u64` bitsets for frequent atomic concepts;
  - typed lists for `∀`, `∃`, `≤`, `≥`, nominals and data restrictions.

  This follows FaCT++'s separation of simple and complex concepts†.
- **Cold data** is in side arenas behind `cold`: nominal introduction, distinctness, cardinality state, datatype components, cache metadata. This is Konclude's lazy side structures, in a cache-friendly form.
- **Undo** is a trail, never a copy:

  ```text
  BranchFrame { node_len, edge_len, fact_len, trail_len, queue_cursors, dep_level }
  Undo { NodeFlags{node, old} | Representative{node, old} | Blocker{node, old}
       | Label{node, old} | QueueCursor{queue, old} | … }
  ```

  Append-only arenas are cut back to their saved lengths; only mutated fields go on the trail (FaCT++'s save and restore†). The union-find of `representative` uses no irreversible path compression inside a branch.
- **Dependency sets** are interned persistent chains of branch points, as HermiT's are, hash-consed by `(rest, point)`. `DepSetId` answers "where to jump back to"; `ProofId` answers "which axioms explain this". **The two are never mixed (D, E).**
- **Two memory measures** are reported, since the literature has no per-node figure (D): bytes per live hot node, and amortised bytes per node including labels, edges, dependency sets and the trail.

### Rules and scheduling

- **Compiled clause triggers:**
  - Each DL-clause is compiled once into a join program per selectable body atom: the hyperresolution join, as HermiT pre-compiles its clauses†.
  - A new fact `A(x)` runs only the programs triggered by `A`, joining the rest over the node and edge indexes.
  - These are the same plans the store's query engine and the RL reasoner use for rule bodies (§12).
- **Queues by kind, deterministic first,** so saturation stays deterministic as long as it can before a branch point exists (the hypertableau's point):

  ```text
  deterministic hyperresolution → ∀ propagation → datatypes → ∃ expansion
  → blocking re-checks → nominal introduction → ≤ merges → disjunctive choices
  ```

  The order is set by profiling.
- **Blocking:**
  - Pairwise blocking, which is required for inverses and number restrictions.
  - **Anywhere blocking:** a blocker is any earlier node under a strict order that respects ancestry, not only an ancestor.
  - Candidate blockers are found through the 128-bit blocking signature (node label, parent label, edge roles), and the exact check follows the hash hit.
  - **Individual reuse** comes later (D: weaker measurements, error-prone with nominals).
- **Nominals and numbers:**
  - The NI rule and at-most merging in the order the calculus prescribes. Motik et al. discuss the "yo-yo" effects that otherwise arise.
  - Counting before merging where the clauses fix class sizes: the number module, [§ Number reasoning layers](#number-reasoning-layers).
- **Search:**
  - **Semantic branching:** after `C` fails, `¬C` is added.
  - **Dependency-directed backjumping:** on a clash, jump to the latest branch point the clash depends on.
  - Both have the largest robust historical effects:
    - Tableaux'98 solved problems: 967 with everything, 849 without semantic branching, 880 without backjumping†;
    - modified GALEN: 70 s with everything, over 10,000 s without backjumping†.

### The datatype theory

A component with its own interface, called by the tableau (D):

```text
DatatypeTheory
    add_literal(var, canonical_literal, dep)
    add_range(var, range, polarity, dep)
    add_not_equal(a, b, dep)
    merge(a, b, dep)
    min_cardinality(component)
    check(component) -> Sat | Clash(DepSetId)
```

- **Per connected component:** constraints are solved per component of data variables, not over the whole state at each literal.
- **Value spaces** per the OWL 2 datatype map (W3C *OWL 2 Structural Specification*, §4):
  - numerics as canonical disjoint intervals over the overlapping value spaces of decimal, integer, float, double and `owl:real`/`owl:rational`, with negation as interval difference;
  - strings with patterns and lengths;
  - date-times, booleans, binary data.
- **Equality:** value equality and lexical equality are kept apart.
- **Clashes carry dependency sets,** as FaCT++'s intervals do†. Without that, backjumping over a choice whose impossibility only a facet combination shows would be wrong.
- **Shared code:** the numeric value spaces reuse `nrese-xsd`'s parsing and comparison (one numeric semantics in the store, roadmap step 4). They don't reuse SPARQL's operator semantics, which differs for out-of-range and invalid literals (C).
- **As built (12 October): where a range is only bounded, two kinds of reason.** A model
  found with a bound is never an answer; the reason decides which:
  - *outside OWL 2 DL, or what the specification lets an implementation leave out*
    (`Eval::approximate`, the answer `unsupported`): a datatype neither in the map nor
    defined, a facet outside its datatype's facet space or a pattern that isn't a regular
    expression (§11.2, §7.5); a cyclic datatype definition (§11.2); a literal whose value
    isn't represented (a number past 128 bits, more than 18 fractional digits of a
    second: §4 asks only for XML Schema's minimal lexical forms; an XML literal not
    decided);
  - *a resource limit* (`Eval::limited`, the answer `gave-up`): two sets combining more
    than `MAX_PATTERNS` (12) patterns and language ranges, each kind of text apart (the
    limit is per combination; before, more than 12 in the whole ontology left every
    pattern undecided); a component of more pairwise-unequal values than are listed (over
    a million).
- **The search for distinct values** is exact: it runs until it decides or the
  operation's deadline or cancellation stops it (`gave-up`), no longer to a fixed step
  budget that answered `unsupported`.

<a id="number-reasoning-layers"></a>

### Number reasoning layers

**What it is for.** Some class sizes are fixed globally, and no merge search sees it. In
W3C DL-906, 907 and 910 (WebOnt 2003, Extracredit, issue `dl-900-arith`), with `p`, `q`
and `r` functional:
- `N` is exactly the `p`-predecessors of the nominal `d`, and `d` has `n` of them;
- each `N`-element has `m` `q`-predecessors, so `|NM| = m·|N|`;
- `NM` is also exactly the `r`-predecessors of `d`, and `d` has `k` of them.

So `k = m·n`: a degree sum over a singleton anchor (the nominal), inverse cardinalities and
functional forward roles. 906 is (20, 30, 600), 907 (200, 300, 60,000), consistent; 910 is
(20, 30, 601), inconsistent. The merge search is resolution-like, and pigeonhole formulas
need exponential resolution proofs and have polynomial cutting-plane ones (paper says,
Buresh-Oppenheim et al. 2003 citing Haken 1985 and Cook et al. 1987). The search gives up
on 906 from `n = 3, m = 4`, and 907's 60,000 pairwise-unequal successors exceed memory.
(Wiki: *Cardinality reasoning and pigeonhole cases*, with the review of 7 October.)

**Four layers, the trust boundary in the types:**
1. **Extraction** to a `NumberProblem`, within a syntactic fragment: singleton nominals,
   domains and ranges, inverses, functional properties, existentials, the sub- and
   equivalent-class axioms over the counted classes, and exact, min and max cardinalities
   that translate fully. Anything else (chains, transitivity, `Self`, keys, property
   disjointness, negative assertions, an unrelated ABox) and the module declines: the
   search decides.
2. **Count closure and refutation:** an integer variable per type (the classes split into
   types where they overlap), equations from each pattern (`|X| = n` at a nominal,
   `|Y| = m·|X|` through a functional role), bounds from at-least and at-most. Infeasible
   means inconsistent, and that is sound: the clauses used are a subset of the axioms.
   `nrese_dl::numbers` is this layer today, with sizes per class compared pairwise
   (DL-910: 9.5 ms, no branch point).
3. **Construction of a `CompressedModelCandidate`,** untrusted: one proxy per type with its
   multiplicity, **role blocks** (the edges of each role between proxy types) and **degree
   constraints** (907: the out-degree of `q` on `NM` is 1, the in-degree on `N` is 300). A
   role is a relation, not a multiset, so the counts must be realisable as a simple
   relation: trivial for these shapes, an integer-flow or b-matching problem in general.
   A construction that fails means "fall back to the search", never "inconsistent".
4. **An independent validator** checks every axiom of the problem on the compressed model,
   multiplicity-aware, without knowing how the candidate was built. Only a validated
   candidate answers "consistent", and only where the whole ontology lies inside the
   fragment; anything else falls back.

Only layer 2's refutation and layer 4's validated model are types that become answers; a
`NumberProblem` or a `CompressedModelCandidate` cannot. No count solution is "consistent".

For 907 the candidate has three proxies: `d`; `N` × 200; `NM` × 60,000 with 300
`q`-predecessors per `N`-element. That compresses one chosen model of 60,201 elements; it
isn't a minimal model size (the ontology doesn't make the classes disjoint).

**Guards:**
- a parametric family: `(n, m, n·m)` consistent and `(n, m, n·m ± 1)` inconsistent, with the
  answers, no branch point, and the number of types and proxies (counts before times);
- at small sizes, the candidate expanded to an explicit model and checked by the fuzz suite's
  model checker (`tests/tableau_fuzz/semantics.rs`). Expansion is a test oracle only:
  "consistent" in production comes from the validator;
- 907's acceptance test asserts that no 60,000 witnesses are built;
- mutants with one axiom outside the fragment, which the module must decline.

**Later:** inside the search (not only before it), a refutation carries the dependency sets
of the facts its constraints came from, so that backjumping works; today it runs before the
search, where 910's facts are unconditional.

**Separate lever:** pool-based merging of at-most candidates (Steigmiller et al. 2012, §4) is
an optimisation of the fallback search, measured on its own. It may bring 906 (621 elements)
into budget; it can't help 907.

**As built (7 October):** layers 1 to 4 in `nrese_dl::numbers` (`problem`, `closure`,
`candidate`, `validate`). Layer 2 replaced the clause-level size comparison and reads the
`NumberProblem`, with the degree sums of properties that have a domain and a range of exact
degrees. `Validated` has a private field: only `validate` makes one. The hypertableau asks
`numbers::model` before it compiles a consistency test (not a class probe, and not where the
caller keeps the model, since the compressed model is no completion graph). DL-906 and 907
are consistent and DL-910 inconsistent in about 1 ms each, with no node and no branch point.

### Caching

In order, each only once the one before is proven correct:
1. **Satisfiability cache** of root labels.
   - **Key:** `(ontology fingerprint, normalised root signature, nominal dependency fingerprint, datatype signature, ABox fingerprint)`.
   - Each entry also keeps its **axiom footprint**, so that a commit invalidates only entries whose footprint it touches (D).
   - Nominals, merges, cardinalities and ABox dependencies must be in the key or the footprint.
2. **Completion-graph caching** (Konclude's)†. Measured on Wine: 49.5 s without it, 0.8 s with it; UOBM-1 went from 240.6 s to 1.3 s.

**Open (W6, caching across a classification's tests; ORE 5303, 3262, 14551, 6485, 10019):**
what is a cacheable node label? With inverse roles a node's satisfiability depends on its
predecessor (a `∀R⁻` sends conclusions back up), and with nominals on the nominal nodes
and the NI rule's merges it shares with other subtrees, so the label alone is no key.
Settled in the performance phase, by a count first: per class test, the nodes whose whole
label (fresh names included) repeats one from an earlier test's complete model (a
satisfiable hit) or from a clash (an unsatisfiable one), split by whether the node's
subtree reached an inverse edge or a nominal; that share, under each candidate key, is
what such a cache can save. What is known (7 October): on 5303 each test's search
dominates, not the number of tests (136 tests, 786 k branch points, 3,391 clashes, 15.2 s
on the lab build; Konclude 0.1 s). Lazily unfolded definitions, which classification
can't use since it reads subsumers off model labels, cut the search to 104 k branch
points over 1,433 tests (12.4 s), with the same taxonomy.

### Gates

The W3C OWL 2 test suite, direct semantics, all of it green:
- consistency;
- inconsistency;
- positive entailment;
- negative entailment.

Then the development subset (§11) against HermiT, KoncludeCLI and Openllet, with no
disagreement that isn't adjudicated.

## 7. Classification and realisation

**Explicit-result caching.** The store caches only complete results of explicit
classification and realisation requests, by asserted revision. A later request retries
an incomplete attempt under the configured budget (an unsupported ontology repeats its
work); nothing retries in the background. Publication keeps the first complete result for a revision and prevents
an older in-flight request from displacing a newer cached revision. Each request still
receives the result for its own snapshot. Query lower bounds retain their separate
schema-lifetime taxonomy cache, including sound partial knowledge.

**Completion-state ownership.** Each stage owns its compiled `Prepared`.
`consistency_with_base` runs the search once and can retain its deterministic checkpoint
and exact model in `Base`. Where the existing feature analysis rules
out choices, the completed graph becomes the deterministic base without a copy.
Classification and realisation keep their
different individual maps; no cross-stage cache is introduced. Realisation releases
the base when the initial labels leave no candidates. Otherwise its candidates negate
unproven classes in that initial model: it retains the deterministic start and releases
the model that those probes would contradict, avoiding a redundant model attempt.

Every retained probe receives its current configuration. Only the deadline,
cancellation and workers may differ from the base; another strategy or capacity
configuration declines reuse before execution and uses the existing fresh search. `None`
means incompatibility, never timeout or unknown. An initially unknown search can retain
its deterministic checkpoint without acquiring model status. An operation's `timeout`
becomes one absolute deadline at its start, which its parts inherit unchanged: model
rollback and its deterministic fallback, detached terminology and full-ontology
attempts, queued island batches and portfolio variants. Each solver checks it before it
starts and while it runs; preparation, state copies, labels and cleanup consume it;
checks remain cooperative, not preemptive.
Probe telemetry includes both fallback attempts; the profile now adds initial and
realisation work. Existing top/subsumption aggregate-counter gaps remain; historical
counters with narrower coverage are not directly comparable work savings.
Per-engine capacity checks do not become an aggregate Base-plus-pool memory ceiling.

Never one test per pair of classes:
- `known_subsumers(C)`: told subsumers, the EL path's, the context core's, and those read from deterministic hypertableau labels. On deterministic ontologies the hierarchy can be read off the labels in a linear number of tests† (HermiT on GALEN).
- `possible_subsumers(C)`: the candidates the model of `C` leaves; only these are tested, in hierarchy order, by the algorithm of Glimm, Horrocks, Motik, Shearer and Stoilos (*A Novel Approach to Ontology Classification*, JWS 2012)†, which improved some ontologies by one to two orders of magnitude over enhanced traversal†.
- **Parallel:** the tests are independent and run on every core; only the hierarchy insertions are serialised.
- **Realisation:** the same over individuals, with the store's RL closure as the known types (the lower bound, §8).

**Output:** a canonical taxonomy:
- each class with its representative among its equivalents;
- its direct superclasses;
- the unsatisfiable classes.

The output is sorted and hashed, so taxonomies are compared byte for byte with the
reference reasoners' (F).

## 8. In the store: the `owl2-dl` mode (`nrese-store`)

**C's central point, and the most important finding of the seven:** a hypertableau is a
complete oracle for consistency, entailment and atomic instance queries. It is **not** one
for arbitrary conjunctive queries, such as cyclic ones or ones whose existential variables
are witnessed by anonymous individuals. The store therefore answers through bounds, and
says what it knows.

### Bounds

- **Lower bound L:** materialised in the inferred stack as today, with provenance. It is the union of everything NRESE derives soundly and cheaply:
  - the OWL 2 RL closure, maintained by the delta executor;
  - the EL path's class memberships;
  - the context core's Horn consequences.

  L only grows with more sound sources (PAGOdA: LUBM's query count closed by the bounds went from 26 to 33 with the stronger lower bound, C).
- **Upper bound U1:** a static, TBox-compiled datalog over-approximation:
  - disjunctions split so that every disjunct is derived (PAGOdA's strengthening, Zhou et al. JAIR 2015, Definition 5.1; choosing one disjunct would not bound: where `⊥` blocks it, the answers of the other are lost);
  - existentials to representative Skolem constants;
  - `⊥` neutralised.

  It is maintained per commit by the same delta executor, as an ordinary incremental datalog materialisation. **It lives in a stack of its own**, a third engine stack beside the asserted and inferred ones, never visible as inferred data (C: `candidate::` against `certain::`).
- **Tighter bounds** are computed per query on the gap's module, never in the commit path: PAGOdA's c-chase variants U2 and U2|3, RSA-based bounds (ACQuA/RSAComb), and summarisation. PAGOdA closed 4,033 of 4,052 queries (99.53%) by bounds alone; UOBM went from 4 queries closed with `L₂+U₁` to 16 with `U₂|₃` (C, from PAGOdA's Table 4).

### The query path

1. Evaluate the basic graph patterns over L (`A_L`) and over L ∪ U1 (`A_U`). If they are equal, the answer is exact.
2. Otherwise, take the gap `G = A_U \ A_L`. Build a query-relevant module from the store's provenance; PAGOdA's modules held 0.5–16% of the facts.
3. On that module, in order:
   1. tighter upper bounds;
   2. RSA recognition;
   3. summarisation;
   4. candidate dependencies;
   5. then the exact services.
4. **The exact services, and the query class each is complete for:**

   | Service | Complete for |
   |---|---|
   | `ExactGroundEntailment` | ground atoms `C(a)`, `R(a, b)`: the hypertableau |
   | `ExactInternalisableCQ` | tree-shaped queries rolled up into class expressions |
   | `ExactFragmentCQ` | queries over modules in RSA or Horn-ALCHOIQ (the combined approach of Carral, Dragoste and Krötzsch 2018) |
   | `ExactGeneralCQ` | not offered until it exists and is proven |

5. **Every answer carries its status** (owner's decision, 2 October):

   ```json
   {"answers": […], "sound": true, "complete": false,
    "lower_count": 10241, "upper_count": 10247, "unresolved": 6}
   ```

   Certain answers are the default only where the plan has a complete path. A query can ask for the lower bound alone (`nrd:mode "sound"`) or for exactness, which may fail with `incomplete`.
6. **SPARQL algebra over the bounds:**
   - Joins, `UNION`, projection and filters that only remove are monotone and are planned over both bounds.
   - `MINUS`, `NOT EXISTS`, negative filters and exact aggregates first close the gaps of the patterns they depend on, or report `incomplete` (C).
   - The literature has no blueprint for SPARQL 1.1 or 1.2 under OWL 2 DL entailment (C); this is NRESE's own contribution.

### Consistency on commit

Under classical semantics, an inconsistent ontology entails everything, so certain answers
need consistency. The commit gate checks consistency with the DL engine:
- **Additions:** monotone. Only the modules of the changed signature are checked, from the cached completion graph where one exists.
- **Deletions:** can't make a consistent ontology inconsistent, so no check is needed for consistency itself; the caches are invalidated by footprint.
- **Settings:**
  - the check runs inline, rejecting the commit as the RL gate does today;
  - or asynchronously, marking the store's DL status `unknown` until it is done, as quarantine does now.

## 9. Incrementality

| Change | Engine | Approach |
|---|---|---|
| ABox addition | context core, tableau caches | monotone: affected contexts queued; caches stay valid |
| ABox deletion | L and U1 | the delta executor (B/F, DRed, the one-step check), as for RL today |
| TBox addition | context core | new clauses into the affected contexts' agendas |
| TBox deletion, bounded cone | context core | over-delete the dependent derivations from the provenance, re-saturate (Kazakov–Klinov for EL, generalised only within a bounded cone) |
| TBox deletion, large cone; nominal or equality changes with wide fan-out | any | re-saturate the affected module from scratch |
| datatype or tableau-only axiom | hypertableau | invalidate the caches whose footprint holds it |

Incremental deletion in SROIQ is an open research problem (B, review §3 gap 5). NRESE claims
it only within a bounded cone. **Every incremental path is checked against a clean rebuild
in the same tests** (§11), as the RL delta executor is today.

**As built (12 October), in the store's `owl2-dl` mode.** What a commit maintains, what it
recomputes, and the test that holds each maintained path to a cold rebuild:

| State | On a commit of assertions only (no statement that may change the schema) | On a schema change | Against a cold rebuild |
|---|---|---|---|
| L: the RL closure | maintained by the reasoner's delta executor (B/F, DRed) | the same | the reasoner's maintenance tests against rematerialisation |
| L: the taxonomy's memberships | recomputed per revision from L (one scan per subclass); the taxonomy kept per schema | taxonomy recomputed on its next read | – (no maintained state) |
| U1 | maintained by the delta executor; afresh where it has equality classes | recompiled, evaluated afresh | `the_maintained_upper_bound_is_the_one_evaluated_afresh`, `…_with_equality_classes_…` (store) |
| consistency, base revision consistent | U1 where it proves it; else **only removals: consistent, no check** (monotone); **additions: the ABox islands with an individual the commit mentions** (`nrese_dl::islands::after_assertions`), the whole where the ABox doesn't split | checked whole | `incremental_consistency_on_commit_is_the_cold_check` (store: every commit of random sequences accepted exactly when a fresh store given the same data accepts it; 200 seeds swept, 3,200 commits, 707 decided on islands, 220 by monotonicity), `touched_islands_decide_as_a_cold_check_does` (dl: 2,000 cases swept) |
| consistency, base inconsistent or unknown | checked whole (quarantine) | checked whole | `inconsistent_data_loaded_past_the_gate_is_quarantined_not_locked` |
| classification, realisation | cached per revision, recomputed on a new one | the same | – (no maintained state) |

Not built: the context core's incremental saturation (ABox and TBox additions queued
into the affected contexts), TBox deletion within a bounded cone, and caches invalidated
by footprint. A schema change recomputes, as the table's last two rows allow for large
cones; v1.5 maintains what the store's write path is dominated by, assertions.

## 10. Proofs and explanations

### One proof IR (E)

```text
SourceAxiom    { id, structural form, graph, triples, revision }
NormalizedAxiom{ id, source_axioms[], steps[] }           // the clauses of §3
Inference      { rule, premises[], conclusion }           // RL, EL, context core, tableau
ProofGraph     { goal, inferences_by_conclusion }         // a directed hypergraph
```

Today's RL explanations (one shallowest proof over triples) move into it. Their premises
map to the OWL axioms through the RL rules' source triples, so a justification is a set of
OWL axioms, not of triples or clauses (review §3 gap 6).

### Justifications

- **API:** `one`, `top-k`, `core` (the intersection of all justifications), `union`, and lazy `all`, in that order of cost (E). Core and union answer most "why" questions without enumerating every justification; one entailment in PULi's SNOMED run had 942,658 of them.
- **Glass box where derivations are recorded** (RL, EL, the context core): PULi-style enumeration by resolution over the proof hypergraph (Kazakov and Skočovský, Apache-2.0 reference). Achievable for RL and EL **now, before any DL code**.
- **Black box for the hypertableau** (version 1, E):
  1. take a locality module;
  2. minimise by divide and conquer over it, with the hypertableau as the entailment oracle;
  3. check the result with a fresh entailment test;
  4. for several justifications, use hitting-set trees or MARCO-style blocking.

  Glass-box certificates from the tableau (clausification, deterministic hyperresolution, clash dependencies) come later, and only as an acceleration.
- **A Horn MUS (minimal unsatisfiable subset) backend** as a differential oracle for the glass-box enumerator (EL2MUS-style): for random entailments, both must give the same minimal sets.

### Readable proofs and a proof checker

- **Readable proofs:** EVEE-style choice of a proof by size, depth or weighted cost, with the user choosing between "minimal axioms", "short", "shallow" and "detailed" (E: no single form is best).
- **A proof checker:** a small, separate checker that validates every step of a proof against the rules and the clauses. The proofs then double as:
  - certificates in the differential tests;
  - regression tests for incremental updates;
  - the explanation benchmarks' ground truth.

### Certificates (as built, 12 October)

An answer is checkable where a small checker can verify it without trusting the
engines, as the number module's validator verifies its compressed models
(§ Number reasoning layers):

| Answer | Certificate | Checker | Covered |
|---|---|---|---|
| consistent (hypertableau) | the engine's model: folded at the blocks, its 3-fold covering lift, or the graph unravelled one step at each block | `nrese_dl::certificate`: every axiom of the ontology as written, by the direct semantics on the finite interpretation (U the universal relation, keys over named individuals), closed under the role inclusions; independent of the normalisation and the engines | object properties, classes and individuals; on the small scope `4,4,4`, 99.84 % of the claims a model can show without data values (8,990,815 of 9,005,280); data values since 13 October (the model carries the values the datatype theory assigns; 196 of the W3C suite's 199 consistent answers with a model certified, the 3 others listed in `tableau_w3c/uncertified.txt`); DL-safe rules (§16) |
| consistent (number module) | the validated compressed model | `numbers::validate` | the module's fragment |
| a class satisfiable, a non-entailment | a model of the ontology with a witness (a fresh anonymous individual in the class, or `a : ¬C`) | as above | as above |
| inconsistent, an entailment | none yet: a refutation is a search tree, not recorded as a proof; the context core's derivations are in the proof IR, but no checker validates their steps against the calculus | – | references decide (§11) |

Next for "inconsistent": the context core's derivations as a checked proof (each step an
instance of its rule over the clauses, the clauses traced to the axioms), then the
tableau's closed branches as a refutation trace.

### Measured

For each explanation request (E's list):
- time to the first justification, to the first readable proof, to the first ten, and to all;
- the justification count and sizes (p50, p90, p99);
- the proof graph's nodes and edges;
- peak memory;
- reasoner calls (black box);
- the module's share of the ontology;
- the incremental latency;
- the time to check the proof.

## 11. Correctness infrastructure (phase 2, before engine code)

### The W3C OWL 2 conformance suite

- Direct semantics, every test type: consistency, inconsistency, positive entailment, negative entailment.
- It is the first gate for every engine. Today only the RL tests run.
- The test cases are fetched by hash, with their licence noted.

### Reference reasoners

Each runs through a runner of NRESE's own:

| Reasoner | How it runs | Role |
|---|---|---|
| KoncludeCLI | directly, **not through OWLLink** (review: the OWLLink adapter, not Konclude, caused the 2023 out-of-memory failures) | reference |
| HermiT, Openllet, ELK | an OWL API runner of NRESE's own; ROBOT only for checking artefacts, not for timing (F) | reference |
| rustdl | directly | a comparison, not an oracle |

### Comparing and adjudicating

- **One output format:** a canonical taxonomy and realisation format, `canonical_rep<TAB>member` and the direct-superclass relation, sorted and hashed (F).
- **No majority votes:** disagreements are recorded as `DISPUTED` and settled by hand, with a minimised witness and a proof or model, never by vote (ORE voted by majority, and its winners had wrong answers too).

### The fuzzer and metamorphic tests

- **A grammar-based generator** of valid SROIQ(D) ontologies that respects the global restrictions (simple roles, regular RBoxes). Small signatures, so the model enumerator of §3 can check them exhaustively.
- **Metamorphic tests**, each of which must leave the answers unchanged:
  - renaming IRIs, reordering axioms, adding redundant axioms;
  - defining fresh names;
  - switching optimisations on or off: absorption, caches, anywhere against ancestor blocking, parallel against single-threaded, SIMD against scalar, the saturation front-end, the context core against the hypertableau.

  Run times may differ.
- **Delta debugging:** every difference is minimised automatically and checked against two complete engines and, where small, by model enumeration.

### The small scope: every ontology up to a bound (as built, 12 October)

Random tests sample; this enumerates. By the small-scope hypothesis, errors show on small
inputs, so every OWL 2 DL ontology up to a bound is checked, not a sample of them:

- **The universe** (`nrese_owl::scope`): classes A, B; properties r, s and their inverses,
  `owl:topObjectProperty`, `owl:bottomObjectProperty`; a data property d with
  `owl:topDataProperty`, `owl:bottomDataProperty`; individuals a, b; `xsd:integer`,
  `xsd:string`, `rdfs:Literal` and a defined datatype; the literals 0 and 1; numbers 0 to
  2. Every class expression constructor, data range constructor and axiom type of the
  structural specification but annotations. A size per axiom: one per constructor and
  per class or data range atom, a chain one more.
- **The bound** `4,4,4`: every one-axiom ontology up to size 4, every two- and three-axiom
  ontology up to total size 4: 2,761,335 ontologies after the reductions. Within OWL 2 DL
  only (§11.2 decided exactly: simple roles; a regular hierarchy, by the least strict
  order each choice of chain forms needs, read in the inverse-normalised form of Horrocks,
  Kutz and Sattler that HermiT checks; `owl:topDataProperty`; defined datatypes); of each
  class of ontologies equal up to renaming (A↔B, r↔s, r↔r⁻, s↔s⁻, a↔b: 32 renamings, a
  group), only the least.
- **The questions:** consistency, A and B satisfiable, `A ⊑ B`, `B ⊑ A`, `⊤ ⊑ A`, `⊤ ⊑ B`,
  `a : A`, `a : B`, `b : A`, `b : B`, through NRESE's consistency, classification and
  realisation (`nrese_dl::scope`), read from the functional syntax as the references read
  it. The three entry points must agree; each claim a model can show must be shown by a
  certificate where data values don't enter (§10).
- **The independent decision:** HermiT on every case, Openllet where HermiT doesn't decide
  or differs from NRESE (`benches/reasoning/dl/scope.py`, the runner's `nrese.dl.Scope`).
  No vote: where they differ from each other or both from NRESE, the case is settled by
  the semantics, and NRESE's model certificate refutes a reference outright.
- **Beyond the bound:** covering arrays (`scope::covering`): ontologies of 3 to 6 axioms of
  up to size 4, chosen greedily until every pair (349 cases) and every triple of the 67
  features (3,354 cases, all 47,905 triples) occurs; a tuple the batches leave gets many
  aimed tries of its own.
- **In the gate:** the scope `3,3` (35,505 cases) against the references' recorded answers
  (`crates/nrese-dl/tests/scope`, the push tier, 10 s), its one-axiom part (8,578 cases) in
  the commit tier (`tests/scope_one`, 5 s).

The campaigns and their adjudications are in `benches/reasoning/dl/README.md`.

### Corpora

| Corpus | Use |
|---|---|
| the W3C suite | the first gate |
| ORE 2015 (Zenodo, by hash, never committed) | the second gate; development subsets on the main PC, the full 1,920 × 3 tasks on the Draco cluster |
| the Oxford corpus (Bate and Sequoia evaluations) | the context core |
| UOBM 1/5/10, OWL2Bench, LUBM | ABox and query bounds |
| BioPortal snapshots (SNOMED only with a licence) | large ontologies |
| the incremental track: deterministic update streams per base ontology (add and delete ABox facts, GCIs, chains, facets, nominal equalities), each checked against a cold rebuild | incremental reasoning |

ORE had no incremental track, so NRESE defines one (D, review §2).

### Results

Results are recorded as `success`, `timeout`, `memory`, `unsupported`, `incomplete`,
`parse-error` and `wrong-result`, never averaged away. Each run records:
- the hardware and software versions;
- per-phase times: parse, preprocess, saturate, search, classify;
- peak memory;
- search counters: nodes, merges, branches, clashes, backjumps;
- rule and blocking counters;
- cache hits;
- the taxonomy hash.

## 12. Optimisations at every level

The roadmap's demand ("state of the art at least, then NRESE's own full-stack
optimisations"), per level:

| Level | State of the art taken | NRESE's own, through the full stack |
|---|---|---|
| Method | EL fast path; one context core with families; hypertableau anchor; modules; PAGOdA/ACQuA bounds; PULi; black-box minimisation | The store's incremental datalog engine *is* PAGOdA's lower and upper bound: maintained per commit, not rebuilt per query (A: "the part of the architecture stand-alone reasoners have to bolt on"). Support graph sets give provenance for modules and explanations for free. |
| Plan | absorption with cost-chosen triggers; ordered resolution; known/possible subsumers; module-first query gaps | Trigger atoms and clause join orders chosen from the store's exact counts and characteristic sets; query gaps planned by the same planner as the query. |
| Operators | compiled hyperresolution joins; indexed clause matching; semantic branching and backjumping; anywhere blocking with signatures | The query engine's joins (merge, hash, leapfrog over sorted runs) reused as clause-trigger programs; the RL reasoner's semi-naive evaluation for L and U1. |
| Layout | interning; arenas; trails; persistent dependency sets; split labels | Store `TermId`s throughout (no string decoding on any path); 64-byte hot nodes; bitset labels; clause arenas; mapped checkpointed caches with axiom footprints (the derived-index mechanism of 3 October). |
| Machine | coarse parallelism over tests, modules and contexts | Morsel parallelism and work stealing as in the query engine; SIMD for label subset and clash tests, blocking signatures and the candidate-subsumer matrix, **only where profiles show it** (D: no published evidence either way); GPU off the critical path (all reports). |

### Bound results and exact-test reuse

The store's `dl/query` modules separate query analysis, ID-level gaps and exact-test
adaptation. SPARQL owns each retained solution table's snapshot, computed terms and
reservation. Lower and upper results must share a dictionary; query-local computed IDs
are remapped before raw IDs are compared. Lower bags and delivery order survive, and a
true lower ASK skips the upper evaluation. The upper ID table is materialised; decoded
exact-candidate rows are batched. Under a finite `dl.max_candidates` the capped
candidate prefix is admitted in term-id order (deterministic, not by meaning); the rest
are reported unresolved.

Eligible exact tests use the existing `tableau::Prepared` probe engine. One immutable
program per candidate batch holds a fresh selector per test that implies its assumption;
only the selected probe asserts its selector, and the unselected definitions are
conservative extensions of the premise. Each probe owns fresh search state; temporary
selector axioms are removed from the operation's scratch ontology afterwards. Test forms
the batch can't express (where no assumption exists) use the scalar reduction, which also
serves as the batch's differential oracle. There is no cross-query compiled cache: the
batch owns its program.

Candidates run as a batch of the operation's `Workers` width, each with a one-worker
child allowance; classification and realisation reuse the same handle across stages and
context rounds. Finite search memory is divided among the probes actually running.
Portfolio siblings share a child cancellation token that cannot cancel their request; the
returned outcome is the first decided one in input order, so its telemetry does not name
the earliest wall-clock winner.

## 13. Configuration

This section owns what the DL settings mean; [config-reference.md](../ops/config-reference.md)
lists the keys and their defaults.

**Time.** A DL operation (a commit's consistency check with its evidence, a query's exact
services, a classification or realisation) takes one absolute deadline from `dl.timeout`
when it starts. Every phase measures what remains from that deadline instead of starting
its own timeout, and checks the request's cancellation flag at the same work boundaries.
A cancelled query publishes no consistency status for its revision. Exhausted gate
evidence keeps the sound rejection evidence found so far without claiming it minimal.
Normalisation and compilation don't poll; schema reading may overrun the deadline by
their duration.

**Memory (`dl.memory`).** Two checks, both at work boundaries, both cooperative:

- *Context saturation* samples an estimate of the capacity it owns: the compiled program,
  the context arena and registry, clauses and their indexes, queued payloads, assertion
  state and worker buffers. Each context publishes its local capacity at its work
  checkpoints and inbox writers publish queued capacity under the inbox lock; a sampler
  reads these without taking context locks, on the process watch's cadence and once
  after each round. Hash-table capacity and B-tree nodes are estimates, not allocator
  telemetry. Observations can lag growth and overlap during transfers, so an operation
  near its envelope may stop earlier or later than exact accounting would, and in-flight
  work can exceed the envelope.
- *The tableau* checks the capacity it owns; a batch divides the allowance among the
  probes actually running.

**The request's envelope.** Within a request (a query's exact services and its
consistency check, a commit's gate, an entailment check, an explanation) both engines
also reserve what they observe from the request's one memory envelope
([execution-core.md](execution-core.md#execution-and-resource-lifetime)): context
saturation through its observer (`context::Budget::memory`), the tableau at its memory
checks (`tableau::Config::memory`), shared by a batch's probes, a portfolio's variants and
the islands. Compiling reserves its scratch there too ([`nrese_dl::memory`](../../crates/nrese-dl/src/memory.rs)):
a premise's copy before it is made, the normalised clauses, the compiled program and the
island batches as each exists, estimated by capacity (within 0.1 % of the allocator on the
guard's ABox, `tests/scratch_memory.rs`). Where the envelope has no room the run stops as
past `dl.memory`, its candidates unresolved because "the request's memory envelope ran
out"; a verdict so limited isn't recorded for the revision. `dl.memory` bounds each run
inside the envelope. Classification and realisation, cached per revision and shared by
the lower bound, aren't a request's: they keep `dl.memory` and the process limit.

Exhaustion yields an incomplete outcome, never a classification inferred from truncated
work. Retained classification buffers are charged; result and proof assembly,
short-lived recursive temporaries, the tableau's checkpoints, allocator metadata and
stacks are not. `0` disables the `dl.memory` checks; the standalone context API defaults to
none. The profile's `task_memory_peak` is the highest sampled estimate (zero when nothing
is observed), neither RSS nor an exact peak.

`Budget::max_memory` in the context core measures the **whole process** and is a separate
opt-in guard; who installs the process limit and which watches reach DL work is in
[config-reference.md](../ops/config-reference.md#budgets).

**No built-in budget.** Every budget of the engines and drivers is a setting
([`nrese_dl::limits`](../../crates/nrese-dl/src/limits.rs)), unlimited where nobody set
it: the engines' own defaults are none, the store's are the table's, each with its reason
in [config-reference.md](../ops/config-reference.md). A run that reaches one says which
(`dl.max_nodes reached (…)`). A comparison gives NRESE the limits every system gets, the
task's time and the envelope's memory (`dl_classify --timeout --memory-mib`), nothing
tighter. The guard is `tests/tableau/limits.rs`: the defaults set none, each budget names
its setting when reached, and no literal budget is in the sources.

The settings; `dl.race`, `dl.cache`, `dl.fallback.*`, `dl.explanations` and `async`
consistency are designed, not yet settings.

| Setting | Values | Default |
|---|---|---|
| `reasoner.mode` | … `owl2-dl` | — |
| `dl.answers` | `certain-where-complete`, `sound`, `exact` | `certain-where-complete` |
| `dl.consistency` | `inline`, `async`, `off` | `inline` |
| `dl.timeout`, `dl.memory` | per operation; 0 or `unlimited`: none | 30 min, none (the request's envelope) |
| `dl.max_candidates`, `dl.max_nodes`, `dl.max_branch_points`, `dl.max_merges`, `dl.explanation_tests` | counts; 0 or `unlimited`: none | 1000, then none |
| `dl.lower_bound_timeout`, `dl.max_join`, `dl.max_join_steps` | route thresholds (past them the hypertableau decides) | 60 s, 2^20, 2^16 |
| `dl.fallback.clauses`, `dl.fallback.equalities`, `dl.fallback.contexts` | budgets of §4 | from the ORE and Oxford runs |
| `dl.race` | `on`, `off` | `on` above a module size |
| `dl.threads` | batch width; 0: every core | 0 |
| `dl.cache` | `sat`, `completion-graph`, `off` | `sat` |
| `dl.explanations` | `proofs` (record derivations), `off` | `proofs` |

**Metrics:** the per-phase times and counters of §11, with the fallback telemetry of §4,
under `nrese_dl_*`.

## 14. Work packages and gates

**Phase 2: foundations (the roadmap's phase 2)**

| Package | Delivers | Gate |
|---|---|---|
| 2.1 | `nrese-owl`: structural model, reverse mapping, diagnostics, provenance | the round trip and the W3C suite's documents read (§2) |
| 2.2 | Normalisation into DL-clauses with metadata and provenance | exhaustive model equivalence on fuzzed ontologies (§3) |
| 2.3 | The W3C direct-semantics runner, reference reasoner runners, canonical taxonomies, `DISPUTED` records | the runners reproduce published HermiT and ELK results on the development subset |
| 2.4 | The proof IR; RL derivations moved into it (EL's with the EL classifier in 3.4); PULi-style justifications (`one`, `core`, `union`, `top-k`, `all`); the proof checker | justifications equal to a Horn MUS enumerator on random entailments |
| 2.5 | The SROIQ(D) fuzzer and metamorphic harness | runs in CI |

**Phase 3: the engines** (reordered on 3 October; the reasons, the measured bar and a
performance gate for each package are in [owl2-dl-performance.md](owl2-dl-performance.md) §6)

| Package | Delivers | Gate |
|---|---|---|
| 3.0 | The DL lab completed: a stratified ORE development set (≥ 300 tasks), OWL2Bench DL, UOBM; reference results on all; NRESE per-phase timing and counters; committed baselines | the references agree or are adjudicated |
| 3.1 | Context core, Horn stage, parallel from the start (lock-free context activation, work stealing); dispatch and modules; the EL classifier becomes its EL mode (chains kept explicit for EL: the automata of §3 make `∃(r∘s).B ⊑ C` non-Horn) | §5's Horn gate; EL taxonomies unchanged; ≥ 2× faster than ELK at equal threads on the EL track |
| 3.2 | Bounds for large ABoxes: U1 compiled and maintained per commit, the gap, consistency through L, U1 and the Horn core | the incremental track against cold rebuilds; LUBM(800) and UOBM(500) ≥ 10× faster than Konclude |
| 3.3 | Hypertableau, ALC → SHIQ → SROIQ: absorption, lazy unfolding, blocking, NI, merges (rollback union-find with reasons), dependency sets over stable decision literals, backjumping, semantic branching, (un)satisfiability caching, watch lists, a native cardinality propagator with explanations | the W3C DL suite green without datatypes; within 3× of Konclude on the development set |
| 3.4 | Classification and realisation driver (known/possible subsumers, parallel tests, model merging) | taxonomies equal to the references'; total time at most Konclude's ([performance plan](owl2-dl-performance.md) row 5) |
| 3.5 | The datatype theory | the W3C DL suite green |
| 3.6 | Context core, SRIQ and SROIQ stages; dynamic fallback, racing, telemetry | §5's gates; the Oxford sample: every task Konclude solves and more |
| 3.7 | Caches with footprints, completion-graph reuse, saturation coupling; profiled SIMD | metamorphic on/off tests; first in every category of the full ORE run (Draco) |
| 3.8 | Black-box justifications over the hypertableau; glass-box certificates | equal to the glass-box results where both apply |
| 3.9 | Conflict-driven hypertableau (research): learned nogoods, activity order, restarts ([performance plan](owl2-dl-performance.md) §6) | no answer changes on or off; a robust gain on the holdout or the switch stays off |

The final bar is Konclude's 1,862 of 1,920 ORE classifications (and 1,911 consistency
checks, 591 of 624 realisations, Lam et al. 2023), on the same machine, with current
versions run anew (A, F). It is not a first milestone.

**Phase 4: in the store**

| Package | Delivers | Gate |
|---|---|---|
| 4.1 | L from every sound source with provenance; U1 in a candidate stack, maintained per commit | U1 ⊇ certain answers ⊇ L on UOBM, LUBM and OWL2Bench (checked against the reference reasoners) |
| 4.2 | The query path with gaps, modules, tighter bounds, exact services and the completeness status | PAGOdA's bound coverage reproduced on its corpora |
| 4.3 | The DL consistency gate on commit | the incremental track against cold rebuilds |
| 4.4 | Explanations of DL answers through the proof IR | the explanation measurements of §10 |

## 15. Risks and open questions

- **Equality and cardinality explosion** in the context core. Hedged by the fallback and by racing; the thresholds come from data.
- **Nominal-heavy ontologies with large ABoxes:** the weakest area of every published system (A, B). Here the store's bounds are the lever, not the search.
- **Incremental SROIQ deletion:** open research. NRESE claims only bounded cones, and module re-saturation otherwise.
- **Certain answers to arbitrary conjunctive queries over SROIQ(D):** open. NRESE reports completeness and offers only the exact services it can prove.
- **Datatype coverage:** reported apart in every benchmark. Unsupported never counts as success (A).
- **Benchmark compute:** ORE's full corpus doesn't fit on one PC. The full runs go to the Draco cluster (review §3 item 7).

## 16. Inputs outside OWL 2 DL (as built, 13 October)

**The question** (the owner, on ORE's `ore_ont_714`, which the reader found not OWL 2 DL
and every reference but Openllet failed on): decide what can be decided soundly, and say
exactly why where it can't. `nrese_dl::beyond` answers consistency
(`beyond::consistency`) and classification (`beyond::classify`); every answer is certified by
a model the independent checker verifies, or follows by monotonicity from part of the
input, never assumed.

**What is outside OWL 2 DL, and what is done with it:**

| Input | Reader | Answer rests on |
|---|---|---|
| a DL-safe rule (SWRL; `DLSafeRule`, not an OWL 2 axiom, *Structural Specification* §9) | read into `Ontology::rules` with a `Diagnostic::Rule`; the engines leave it out and say so | grounding (below) |
| a SQWRL query (a rule with `sqwrl:` built-ins) | read as a rule | reported as a repair: a query, it asserts nothing |
| a disjointness repeating an operand (`x owl:disjointWith x`, `a owl:differentFrom a`) | **repaired**: the operand is empty (an individual: a contradiction), its RDF-Based meaning; the operands were deduplicated and the axiom said nothing before | the repaired axioms |
| a property used without a declaration, a blank node two expressions share | repaired as before (typed by use, one expression) | as OWL 2 DL |
| the reserved vocabulary used as a class, property or individual (`rdfs:Class`, `rdf:type`, `rdf:first`; §2.4) | the axioms left out, each with a `Diagnostic::Reserved`; they were read as fresh names, which decided what OWL 2 Full says otherwise | monotonicity only |
| a non-simple property where a simple one is required, an irregular hierarchy (§11) | kept in the model, left out of the clauses | monotonicity; the certificate checks the axiom itself |
| an ill-typed literal (§5.7), syntax and mapping errors | left out | monotonicity only |

**Monotonicity.** What the reader reads is part of what the input says: "inconsistent"
and every entailment of the DL part hold for the input. For an RDF graph outside OWL 2
DL (whose meaning is the RDF-Based Semantics), the correspondence theorem (*RDF-Based
Semantics* §7.2) carries the DL part's entailments to the graph, and that semantics is
monotone. "Consistent" and non-entailments need a model of the whole input: they are
claimed only where the reader left nothing out but rules and axioms the checker checks.

**Rules by grounding.** An instance of a DL-safe rule over named individuals and data
values is an OWL 2 DL axiom (`beyond::ground`): with a pivot individual `a` of the body,
`{a} ⊓ Bₐ ⊓ ∃U.({b} ⊓ B_b) ⊑ Hₐ ⊓ …` (an empty head is `⊥`, SWRL §3; built-ins and data
ranges on constants are evaluated), or, where the assertions state the body, the head's
assertions. It follows from the rule, so adding it changes no model of the input. The loop:
the chase adds the instances the assertions entail; then the hypertableau runs on the DL
part with the instances so far, the checker evaluates every axiom and rule on its model
(rules over the elements named individuals denote; data variables over the values the body
binds; `swrlb:` arithmetic, comparisons and strings), and the instances the model breaks
are added. It ends with a model that breaks none ("consistent", certified), an
inconsistent DL part with instances ("inconsistent", by monotonicity), or a budget. A model
the checker can't certify still shows instances to ground (sound whatever the model), so
it can lead to "inconsistent", never to "consistent". The certificate checker now covers
data values (`certificate::data`: the datatype map value by value; the tableau's models
carry the values the datatype theory assigns, `DatatypeTheory::assign`).

**Classification.** Inconsistent: every class unsatisfiable. Consistent, and the TBox
names no individual and uses `owl:topObjectProperty` at most as a superproperty: the DL
part's classification is the input's (a disjoint union of a TBox model with a model of
the input is a model of the input, since assertions, keys and rules bind named
individuals only). Otherwise the DL part with the instances gives the entailed
subsumptions, and models of the whole input refute the rest: the certified model, a run
per class with a fresh anonymous witness, a run per open pair; what stays open is said.

**Why not native rules in the hypertableau.** HermiT's way (rules as DL-clauses guarded
by `O(x)`) decides rules without built-ins; it needs value joins and built-ins inside the
datatype theory, which the grounding loop gets from concrete values in certified models.
Grounding reuses the engines unchanged and certifies its answers; its cost is a
hypertableau run per round (the ORE inputs need one or two). Native rules stay the next
step if rule-heavy inputs need many rounds.

**Explanations of "inconsistent".** `beyond::justify` finds a minimal inconsistent part
(axioms and rules) by deletion; on ORE's eight inconsistent inputs with rules, four axioms
each, checkable by hand (§ results in [STATUS](../STATUS.md)).

**Where it stays undecided, and why** (each answer says it): a model the certificate
builder can't certify (counting cases the fold breaks, §10); a built-in NRESE doesn't
evaluate (named); an ill-typed literal (§5.7); what the reader couldn't map (no direct
semantics); a budget.
