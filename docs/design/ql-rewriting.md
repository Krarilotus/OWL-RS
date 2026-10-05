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
- the class memberships that only an existential gives (`A ⊑ ∃R`, `∃R ⊑ B`: `A ⊑ B`, which
  no RL rule derives); each class atom gets them as alternatives.

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

Tree witnesses can be exponential in the query. Per basic graph pattern: at most 16
existential variables, 64 tree witnesses, 256 branches, and 4,096 triple patterns in the
rewriting (guard `rewritings_stay_within_their_bounds`). Over a limit the pattern runs
unrewritten (sound, perhaps incomplete), and EXPLAIN shows `ql-limit`.

## 6. Evidence

- Unit tests of the witness computation (`nrese-owl` `ql/tests.rs`).
- A differential test: random small QL ontologies, data and conjunctive queries; the
  store's answers (owl2-ql closure plus rewriting) against certain answers computed by a
  separate bounded chase (`nrese-store/tests/it/ql_rewriting_tests.rs`).
- The W3C OWL 2 test cases of the QL profile, conclusions as `ASK` queries
  (`nrese-store/tests/w3c_owl2_ql`); none of them goes through an existential.
- Measured cost and answers gained: performance.md §6.
