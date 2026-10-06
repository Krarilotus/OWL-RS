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
- violations of the global restrictions: non-simple roles in cardinalities, `Self`, `owl:disjointWith` of properties, irreflexive and asymmetric; and an RBox that isn't regular.

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
  - An algebraic method for cardinalities (ILP, CARON's arithmetic module) comes only if profiling on UOBM, OWL2Bench and ORE shows merge combinatorics as the hotspot (B, D).
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

### Caching

In order, each only once the one before is proven correct:
1. **Satisfiability cache** of root labels.
   - **Key:** `(ontology fingerprint, normalised root signature, nominal dependency fingerprint, datatype signature, ABox fingerprint)`.
   - Each entry also keeps its **axiom footprint**, so that a commit invalidates only entries whose footprint it touches (D).
   - Nominals, merges, cardinalities and ABox dependencies must be in the key or the footprint.
2. **Completion-graph caching** (Konclude's)†. Measured on Wine: 49.5 s without it, 0.8 s with it; UOBM-1 went from 240.6 s to 1.3 s.

### Gates

The W3C OWL 2 test suite, direct semantics, all of it green:
- consistency;
- inconsistency;
- positive entailment;
- negative entailment.

Then the development subset (§11) against HermiT, KoncludeCLI and Openllet, with no
disagreement that isn't adjudicated.

## 7. Classification and realisation

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

## 13. Configuration

| Setting | Values | Default |
|---|---|---|
| `reasoner.mode` | … `owl2-dl` | — |
| `dl.answers` | `certain-where-complete`, `sound`, `exact` | `certain-where-complete` |
| `dl.consistency` | `inline`, `async`, `off` | `inline` |
| `dl.timeout`, `dl.memory` | per task | 30 min, 75% of RAM |
| `dl.fallback.clauses`, `dl.fallback.equalities`, `dl.fallback.contexts` | budgets of §4 | from the ORE and Oxford runs |
| `dl.race` | `on`, `off` | `on` above a module size |
| `dl.threads` | count | every core |
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
| 3.4 | Classification and realisation driver (known/possible subsumers, parallel tests, model merging) | taxonomies equal to the references'; within 1.5× of Konclude's sum |
| 3.5 | The datatype theory | the W3C DL suite green |
| 3.6 | Context core, SRIQ and SROIQ stages; dynamic fallback, racing, telemetry | §5's gates; the Oxford sample at Konclude's solved count |
| 3.7 | Caches with footprints, completion-graph reuse, saturation coupling; profiled SIMD | metamorphic on/off tests; the full ORE run at Konclude's level (Draco) |
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
