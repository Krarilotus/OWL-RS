# OWL 2 QL answers through existentials: tree-witness rewriting over the RL closure

**What this is.** NRESE materialises. An axiom with an existential on the right
(`Employee ⊑ ∃worksFor.Organization`, `∃R⁻ ⊑ ∃S`, a domain that is a restriction) invents
individuals, so it isn't materialised, and the certain answers that go through those
anonymous individuals were missing: `SELECT ?x { ?x :worksFor [] }` didn't find an
`Employee` without a stated employer. This note designs the query rewriting that adds
them (merge checklist §1; coverage plan §3; research tasks item 4; reasoner-v2 R8).

**Sources** (evidence: *read* = the primary source read in `literature/corpus`; *cited* =
known through a source read; *ours* = measured or tested here).
- Rodríguez-Muro, Kontchakov, Zakharyaschev, *Ontology-Based Data Access: Ontop of
  Databases*, ISWC 2013 (*read*, §2.1 and §3.1): the tree-witness rewriting over
  H-complete ABoxes; on LUBM∃20, Adolena and StockExchange 67 % of the queries have no
  tree witness, 29 % one, and every rewriting is at most two CQs no larger than the query.
- Kikot, Kontchakov, Zakharyaschev, *Conjunctive query answering with OWL 2 QL*, KR 2012
  (*cited* through the above): the formal definition; exponentially many tree witnesses
  are possible, polynomially many without the "mimicking" fragments.
- Calvanese et al., DL-Lite, JAR 2007 (*cited*): canonical models; a CQ's matches need the
  anonymous part only to the depth of the query.
- Research round 3, `literature/wiki/research/R3-G4 OWL 2 QL and TBox changes.md`: rewrite
  only the existential part, since the RL closure is already H-complete.

## 1. Where it sits

- **On the algebra, before planning.** `nrese-sparql` rewrites each basic graph pattern
  of the query (`native/ql.rs`) before the optimiser's passes run, also under
  `as_written` (it changes the answers, not just the plan). EXPLAIN lists it as the rewrite
  `ql-tree-witness`, and the plan shows the union it made.
- **The schema at the snapshot.** The TBox is read from the snapshot the query reads
  (asserted and inferred statements, every graph, as the rules read them), mapped by
  `nrese-owl`'s reverse RDF mapping, and compiled (`nrese-owl::ql`). It is cached per store
  and rebuilt only when the schema statements change (a hash of them, checked once per
  snapshot revision); data commits never rebuild it.
- **Only where the closure applies:** the default graph of a query without a dataset of
  its own and without access restrictions, read with the inferred statements; not inside
  `GRAPH`, `SERVICE`, `MINUS`'s right side or `EXISTS` (negation and graph scopes keep
  their materialised meaning).

## 2. What it reads, and how it combines with the closure

**The QL part of the TBox.** From every axiom, what OWL 2 QL can say (with three harmless
generalisations):
- concept inclusions `B ⊑ A` and generating axioms `B ⊑ ∃ρ.X` between basic concepts
  `B ∈ {A, ∃P, ∃P⁻}`: subclass, equivalence, domain and range (also data properties),
  unions on the left split, intersections on the right split;
- nested fillers (`∃ρ.(A ⊓ ∃σ.C)`, beyond QL) normalised with fresh concepts, and
  `minCardinality`/`cardinality` ≥ 1 read as `∃`;
- role inclusions with inverses: subproperties, equivalence, `inverseOf`, symmetry;
  reflexive properties (anonymous individuals are reflexive too).
- Not read: transitivity and chains, `hasValue` and `allValuesFrom` on the right, and the
  RL constructs on the left (`∃R.C ⊑ A`, intersections). The rewriting is complete for
  OWL 2 QL ontologies; for ontologies that also use these, it is sound (the QL part is a
  subset of the ontology, and the closure holds only entailed facts) but may miss answers
  that need an RL rule to fire on an anonymous individual.

**Tree witnesses** (Kikot et al.), per basic graph pattern: a connected set `tᵢ` of the
pattern's existential variables (blank nodes, and variables nothing outside the pattern
uses: not projected, not in another pattern, filter, `BIND`, grouping or ordering) and
the other terms `tᵣ` of the atoms touching it, such that those atoms map into the tree a
generating axiom grows below one individual, `tᵣ` onto the individual and `tᵢ` onto the
anonymous part. A homomorphism search over the tree's types decides it. The rewriting is
the union, over every set of tree witnesses with disjoint atoms, of the pattern with each
witness's atoms replaced by "the root is a `B`", for every `B` that generates it; the
roots of a witness are made one term (`BIND` for an equated answer variable).

**Nothing twice.** The RL closure is H-complete (every named class membership and role
fact the hierarchy, domains, ranges and inverses give is stored), so the rewriting adds
only:
- the tree witnesses (the topology), and
- the class memberships the closure lacks: those only an existential gives (`A ⊑ ∃R`,
  `∃R ⊑ B`: `A ⊑ B`, which no RL rule derives), and those of QL inclusions the ruleset
  doesn't apply (intersections on the right and unions on the left under `owl2-ql`, whose
  ruleset has no list rules; inclusions through inverse expressions); each class atom
  gets them as alternatives.

Alternatives the closure already implies are dropped: `B` is left out when another
alternative (or the atom itself) follows from it by what the ruleset materialises
(subclasses, domains, ranges, sub- and inverse properties, `∃P.⊤ ⊑ A`; intersections on
the right only under `owl2-rl`, whose list rules apply them).

## 3. Semantics: sets and bags

Certain answers are sets. Inside `DISTINCT`, `REDUCED` and `ASK` the rewritten pattern is
the plain union. Elsewhere (bags, `COUNT`), a pattern gives its materialised rows with
their multiplicities, plus **one row per answer that only the rewriting finds**:
`P ∪ (DISTINCT π_V(rewriting) FILTER NOT EXISTS P)`, `V` its non-existential variables.
So queries without witnesses keep their bags exactly, and no answer is counted twice.
`COUNT(*)` reads every variable of the solutions it counts, so none of them is existential
for it; `COUNT(?x)` reads only `?x`.

## 4. Switching it on

`reasoner.ql_rewriting` (`NRESE_REASONING_QL_REWRITING`): `auto` (the default) rewrites
when the inferred stack is current under `owl2-ql` or `owl2-rl`, the rulesets whose closure
is H-complete; `off` never. Other rulesets don't close inverses or domains of restrictions,
so they are never rewritten. A request reading only asserted statements (`infer=false`) or
only inferred ones isn't rewritten either.

**No cost without existentials.** With the switch off, or a schema without generating
axioms or existential-only memberships, the pattern isn't touched: the same plan and the
same EXPLAIN (guard `queries_without_tree_witnesses_plan_as_before`). With them, a pattern
without existential variables and without affected class atoms is unchanged too.

## 5. Limits

Tree witnesses can be exponential in the query: k independent ones give 2^k branches.
- **Implied witnesses go first.** A witness with one root, where another atom states the
  root to be a class that generates it, holds in every model: its atoms are dropped
  before branching (Ontop's CQ subsumption, ISWC 2013 §2.1). On NPD a star of 47
  existential arms on `ExplorationWellbore` becomes its class atom (0.6 ms).
- **Bounds.** Per basic graph pattern: at most 16 existential variables, 64 tree
  witnesses, 256 branches, and 4,096 triple patterns in the rewriting (guard
  `rewritings_stay_within_their_bounds`). Over a bound the pattern runs as written, and
  the answer says it may be incomplete (§7). On NPD the same arms without the class atom
  hit the size bound at k = 8, the branch bound at k = 9 and 12, the variable bound at
  k = 17 and 47, each within 0.7 ms.
- **Reported.** EXPLAIN (`explain=true` and `explain=plan`) gives `ql`: the patterns
  rewritten, their witnesses, branches and triple patterns, the bounds reached, and the
  completeness.

## 6. Evidence

- Unit tests of the witness computation (`nrese-owl` `ql/tests.rs`, 11; Ontop's example
  gives its four queries) and of the SPARQL side (`nrese-sparql` `tests/it/ql_tests.rs`, 7,
  with the two guards).
- A differential test: random small QL ontologies, data and conjunctive queries; the
  store's answers (`owl2-ql` or `owl2-rl` closure plus rewriting, sets and bags) against
  certain answers computed by a separate bounded chase
  (`nrese-store/tests/it/ql_rewriting_tests.rs`; 600 queries in the gate, 24,000 on two
  more seeds on 5 October with 571 answers found only through the rewriting: no
  difference).
- The W3C OWL 2 test cases of the QL profile under the Direct Semantics, query-shaped
  conclusions as `ASK` (`nrese-store/tests/w3c_owl2_ql`): 51 pass, 9 fail as listed with
  their causes (difference without equality rules, class expressions without an axiom,
  two inconsistencies the `owl2-ql` ruleset doesn't detect), 2 import other ontologies. No
  conclusion of the suite goes through an anonymous individual, so the suite checks that
  the rewriting breaks nothing; the differential test checks what it adds.
- NPD (Lanti et al., EDBT 2015; ontology and queries at a pinned commit,
  `benches/reasoning/prepare-npd.sh`): 10 of its 31 queries are rewritten, each with one
  tree witness, 2 branches and 4–17 triple patterns; none comes near a bound. Ontop's
  5,000-subquery rewritings of NPD come from unfolding through mappings and hierarchies,
  which the closure makes unnecessary here; its 73 rewriting branches for q6 don't arise,
  because each of q6's blank nodes touches a projected variable. NPD's data is relational
  (a MySQL dump and R2RML mappings) and no longer published as RDF: query times on it are
  the next measurement.
- Measured cost and answers gained: performance.md §6.
