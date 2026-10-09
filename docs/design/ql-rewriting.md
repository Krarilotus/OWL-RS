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

- **One preparation per operation.** `prepare_ql_query` in `nrese-sparql` returns the
  rewritten algebra and its report together, borrowing unchanged queries. The store
  retains one snapshot and one options value for reporting, cache lookup and execution.
  Preparation uses the executor's strict or canonical equality view. Whole-output keys
  still use the original query and QL options; a miss runs the prepared query through
  the existing serializers with further QL rewriting disabled. A hit does no planning
  or evaluation. Collected responses retain the first report, including its actual probe
  count, instead of preparing again on a potentially newer snapshot.
  Request cancellation is checked before/after preparation and before cached bytes;
  preparation errors propagate. These boundary checks do not make the schema scan or
  every rewriting loop internally cancellable.
- **On the algebra, before planning.** `nrese-sparql` rewrites each basic graph pattern
  of the query (`native/ql.rs`) before the optimiser's passes run, also under
  `as_written` (it changes the answers, not just the plan). EXPLAIN lists it as the rewrite
  `ql-tree-witness`, and the plan shows the union it made.
- **The schema at the snapshot.** The TBox is read from the snapshot the query reads
  (asserted and inferred statements, every graph, as the rules read them), mapped by
  `nrese-owl`'s reverse RDF mapping, and compiled (`nrese-owl::ql`). It is cached per store
  and rebuilt only when the schema statements change. Its fast key is the engine's
  `SnapshotIdentity`, including pending changes and visibility masks, alongside the
  reader's graph access. On a new identity, a hash of the schema statements permits
  reuse of the compiled TBox when the schema is unchanged. A revision and statement
  counts alone cannot distinguish two pending views or a same-count replacement.
- **Only where the closure applies:** the default graph of a query without a dataset of
  its own, read with the inferred statements, also as `GRAPH <urn:x-arq:DefaultGraph>`
  (another store's name for it, `compat`). Not inside `GRAPH` over a named graph, which
  holds what was asserted in it (the inferences are the default graph's: reasoning
  semantics, "Graph scope"), nor `SERVICE`.
- **Negation reads the entailments too** (6 October). A pattern inside `EXISTS`,
  `NOT EXISTS` or on `MINUS`'s right side is rewritten as a set: its variables the outer
  solutions bind are answer variables (each solution substitutes its values), the others
  existential, so `?x a :Employee FILTER NOT EXISTS { ?x :worksFor [] }` has no answer
  when every employee works for some organisation. Before, negation kept the
  materialised meaning, and its answers contradicted the same pattern's outside it.
  What such a pattern may miss can make an answer of the query wrong: its incompleteness
  makes the answers `unsound` (§7), as does an `EXISTS` whose truth value can be turned
  either way (`BIND`, a comparison, `IF`'s condition). A variable an outer solution may
  leave unbound (from `OPTIONAL`) is free inside the `EXISTS` and could be an anonymous
  individual: `sound-only`, said. Tested against the chase
  (`negation_and_exists_answer_as_the_chase`: for random queries `Q1` and `Q2` sharing a
  variable, `Q1`'s certain answers kept where `Q2` has (or hasn't) a certain answer that
  agrees; pure QL exact and complete, mixed ontologies never silently wrong; on 6 October
  22,586 queries on two seeds, 672 of them answered otherwise than over the closure alone).
- **Graph access, as the RL path has it.** A rewritten query reads data through the
  reader's access like any other. The schema it is rewritten with matches the closure the
  reader sees: with `inferred = "supported"` (inferences only where the reader's graphs
  support them) the existentials of readable graphs only, compiled per reader; with
  `"visible"` (every inference, whatever graphs it came from) the whole schema, so schema
  graphs are exempt exactly as they are for the RL inferences that reader already sees;
  with `"hidden"` no rewriting and no status. Test
  `rewritten_answers_follow_the_readers_graph_access`. The same holds where the rewriting
  goes further (7 October, `restricted_readers_get_nothing_from_graphs_they_cant_read`):
  the data check (§3) is asked only for a reader of every graph, since its answer comes
  from the whole data (a reader who doesn't see an inference keeps the witness, and the
  answer through the existential); a negated pattern is rewritten with the reader's
  schema, so no answer disappears because of an axiom in a graph it can't read. The
  printer reads the whole store: it is the operator's offline tool
  (`nrese-server print-query`), not a request.

## 2. What it reads, and how it combines with the closure

**Current lifecycle (9 October).** Realised-witness results are scoped to the same
snapshot identity. A failed or budget-skipped probe is not cached as a negative answer;
a later request can retry it. Preparation, reporting and execution share the lifecycle
in §1; the old separate status preparation and error-swallowing wrapper are removed.
Cancellation at evaluator boundaries does not yet make every schema scan or rewriting
loop interruptible.

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
  that need an RL rule to fire on an anonymous individual. Such a miss is never silent:
  §7.

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
  ruleset has no list rules); each class atom gets them as alternatives. Inclusions
  through inverse expressions (`[owl:inverseOf p]`) both rulesets apply, their facts
  being generalised triples (`inclusions_through_inverse_expressions_are_materialised`).

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
for it; `COUNT(?x)` reads only `?x`. The union of the bag form leaves out the branch that is
`P` as written: its rows are `P`'s, all filtered out again. A group none of whose
aggregates depends on how often a row comes (`MIN`, `MAX`, `SAMPLE`, the `DISTINCT` ones,
none) reads a set: the plain union.

**No cost where the rewriting adds nothing** (7 October). Before a witness is folded, the
data the query reads is asked whether every individual of each concept the witness folds
to already has the witness's tree (`ASK { ?r a C FILTER NOT EXISTS { tree(?r) } }`, per
concept). If so, every branch that folds it gives only what the same branch without it
gives, so the witness goes, as a witness the rest of the query implies does (§2,
Ontop's CQ subsumption, decided here by the data instead of the TBox). Where no witness is
left the pattern runs as written. The answer is cached per snapshot revision and question
in the store's rewriting (the first query after a commit asks again), asked only for a
reader of every graph (a restricted reader keeps the whole rewriting), and EXPLAIN counts
the witnesses left out (`ql.realised`) and the questions asked (`ql.checks`). Bounded like
the rewriting (§5), counted, not timed: at most 16 questions per query and 1,000,000
statements per question (the concept's and the tree's predicates', counted before it
runs); past them a witness is folded unasked. A question runs under the query's own
options (cancellation, memory budget). The printer doesn't ask: its queries hold for any
data. On NPD's data 9 of the 12 rewritten queries have their witness in the data and run
as written (`benches/reasoning/queries/npd-stress/README.md`); the random cases of the
differential test leave out 47 witnesses in 600 queries with every answer still the chase's
(`rewritten_answers_are_the_certain_answers_of_random_ql_cases`; guard
`witnesses_the_data_has_are_not_folded`).

## 4. Switching it on

`reasoner.ql_rewriting` (`NRESE_REASONING_QL_REWRITING`), and per repository its settings'
`ql_rewriting`, changed at once:
- `auto` (the default) rewrites when the inferred stack is current under `owl2-ql`, the
  profile that names these answers;
- `on` under `owl2-rl` too. Not the default there (the owner's decision of 6 October): a
  user choosing `owl2-rl` chooses its standard semantics, which also keeps answer counts
  equal to other systems' `owl2-rl` (the benchmark protocol compares rulesets before
  times); `owl2-dl` mode gets these answers through the DL bounds;
- `off` never.

Only `owl2-ql` and `owl2-rl` close the data enough (H-complete); other rulesets don't close
inverses or domains of restrictions and are never rewritten. A request reading only
asserted statements (`infer=false`) or only inferred ones isn't rewritten either.

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
- **A budget for the time.** The bounds above limit the result, not the work: the
  witness search backtracks over the anonymous types, and a query that fails late can
  try exponentially many places within every size bound. So the rewriting also counts
  its steps (candidate interiors, places tried, sets of witnesses): at most 50,000,
  counted rather than timed, so the same query trips the same way everywhere. Past it,
  the pattern runs as written, flagged `sound-only` like any bound (test
  `the_work_bound_stops_a_search_the_size_bounds_dont`). A TBox where every anonymous
  element has ten successor types, queried with a chain of twelve existential arms ending
  in a class none has (about 10^11 places): flagged after 30.5 ms (main PC, release),
  where the search would not have finished. NPD's 31 queries and stress set don't reach
  it.
- **One budget per query** (7 October). The 50,000 steps bound one pattern; a query of
  many patterns (unions, `EXISTS`, `MINUS`, each rewritten on its own) shares 200,000:
  each pattern takes at most what the query has left, and past it the rest run as
  written, flagged (`the query's work`). Planning isn't cancellable, so without it a
  query's rewriting time grew with its number of patterns (tests
  `a_query_has_one_work_budget_for_its_patterns`,
  `the_rewriting_paths_end_in_a_status_at_their_budgets`).
- **Every path ends in a status.** Under a negation a bound reached makes the answers
  `unsound` (§1); the data check (§3) folds a witness unasked past its 16 questions per
  query or 1,000,000 statements per question; the printer (§8) stops after 100,000 class
  expansions (RL rules unfolded into each other can make its query exponential: two rules
  per class over the one below, 18 deep, would be 2^18 copies) and says the query is
  not expressible (`the_printer_stops_at_its_size_bound`). The rewritten query runs
  under the query's memory budget and cancellation like any other; so do the data
  check's questions.
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
  `benches/reasoning/prepare-npd.sh`): 12 of its 31 queries are rewritten (q17,
  q20–q30), each with one tree witness, 2 branches and 4–17 triple patterns; none comes
  near a bound. Ontop's
  5,000-subquery rewritings of NPD come from unfolding through mappings and hierarchies,
  which the closure makes unnecessary here; its 73 rewriting branches for q6 don't arise,
  because each of q6's blank nodes touches a projected variable. On its data (6 October;
  the PostgreSQL dump materialised by Ontop, 2.0 M statements,
  `benches/reasoning/prepare-npd-data.sh`): all 31 queries give the row counts of Ontop
  answering over the database with its existential reasoning, all `complete`; without the
  rewriting q28–q30 miss 62, 113 and 28 answers. Times in
  `benches/reasoning/queries/npd-stress/README.md`: the queries not rewritten unchanged,
  those gaining answers +0.5–0.9 ms, the others rewritten +0.9–6.1 ms (the bag form's
  second evaluation).
- Measured cost and answers gained: performance.md §6.

## 7. Completeness, reported with every answer

The rewriting is complete for OWL 2 QL. Over the RL closure, axioms outside QL can meet
the anonymous individuals it reasons about: a transitive property or a chain links them to
other individuals, a functional property, key or cardinality makes one equal to a named
individual, `∃R.C ⊑ A` or an intersection on the left classifies one. Answers through those
may then be missing. The rewriting doesn't change any answer for it; it says so.

**Detected from the schema at the snapshot** (`nrese-owl::ql`, when the TBox is compiled),
conservatively:
- *hazards*: properties that are transitive, in a chain, functional or inverse functional,
  in a key, or under `allValuesFrom`, `hasValue`, `hasSelf`, a cardinality restriction or a
  qualified `someValuesFrom` on the left; classes in an intersection, a filler or an
  enumeration on the left; generating axioms whose filler the rewriting reads only in part;
  individuals asserted to be in an existential restriction;
- an anonymous individual's type is *affected* when a property connected to its role (by
  inclusions, inverses, equivalences or chains) or one of its classes has a hazard;
- the *affected terms* are the properties and classes of affected types, closed upward
  along every axiom (from the terms of its premise to those of its conclusion); a hazard
  that can make individuals equal (functional, keys, cardinality, enumerations) affects
  every term.

**Per query:** `sound-only` when an atom reads an affected term, when a pattern reached a
bound (§5), when a predicate or class is a variable, or when a pattern the rewriting doesn't
enter (`SERVICE`, a property path, an aggregate's `EXISTS`) reads a term the rewriting
would change; otherwise `complete`. Under a negation each of these is `unsound` instead
(§1). Each reason names the term and the hazard.

**Reported** in EXPLAIN (`ql.completeness`, `ql.sound`, `ql.complete`, `ql.reasons`, both
forms), in the store's results, and with every answer over HTTP: the header
`NRESE-Completeness: complete; regime=owl2-ql` or `sound-only; regime=owl2-ql; reasons="ql: …"`
(`owl2-rl` over the RL closure), computed on the snapshot
the query reads before its first byte.

**One status for every engine.** The status is `nrese_sparql::Completeness`, the shape the
DL design gives every answer (owl2-dl.md, "Every answer carries its status"): `sound`,
`complete`, optional `bounds` (lower, upper and unresolved counts), `reasons`, each with
its `source` (`ql` here, `dl` for the bounds), and the `regime` its `complete` refers to (set
by the producer: `owl2-ql` or `owl2-rl` here, by the closure; `owl2-dl` for the bounds).
QL's `complete` is every answer the closure's rules and the existentials entail, not every
certain answer under OWL 2 DL. Sources add reasons and `merge` combines
statuses; `header()` writes the header (`complete`, `sound-only` or `unsound`, then
`lower=…; upper=…; unresolved=…` where bounds are known, then the first five reasons). The
QL rewriting reports `sound` and, under a hazard, not `complete`, without bounds: it
doesn't know how many answers it misses.

**The research review's case** (existentials feeding a transitive role): `Engine ⊑
∃partOf.Car`, `Car ⊑ ∃partOf.Fleet`, `partOf` transitive, `e1 a Engine`. `SELECT ?x { ?x
partOf ?f . ?f a Fleet }` has the certain answer `e1` through two anonymous individuals and
the transitive shortcut; the closure and the rewriting miss it, and the answer says
`sound-only` with "partOf is transitive" (tests
`existentials_feeding_a_transitive_role_miss_with_the_flag` against the chase,
`existentials_feeding_a_transitive_role_are_flagged`). In `owl2-dl` mode the DL bounds
answer it exactly.

**Tested** with mixed ontologies in the differential test: QL axioms with transitive,
chain, functional and inverse functional properties and `owl:sameAs` in the data, under
`owl2-rl`, against a chase that applies them all (equality by merging elements). Every
answer must be certain; every query that misses one must say `sound-only`. On 6 October,
23,828 queries on two seeds: none wrong, 349 missed answers and all said so, none
silently. The test found two bugs on the way: chains weren't read from the store's
schema, and a transitive property hid that its inverse was inverse functional. The
detection is conservative: 15,953 queries said `sound-only` and missed nothing (98 % of
those flagged), most of them because a functional property makes every term suspect.

## 8. The printer: rewritten queries as standard SPARQL 1.1

For the comparison with stores without reasoning (the protocol's QL line), a query is
printed as standard SPARQL 1.1 that answers, on a store holding the same asserted
statements (the schema included) and nothing inferred, as NRESE answers it under `owl2-rl`
with the rewriting on: the closure's matches plus the tree witnesses, the same rows as bags
and as sets.

**Entries.** `nrese_sparql::ql::print(view, query, form)` returns `Printed::Query { text,
completeness }` or `Printed::NotExpressible(reasons)`; `StoreService::print_query(text,
form)` on a store's snapshot; on the command line `nrese-server print-query [--form
paths|values] [--schema FILE]... (QUERY | --file PATH)` against the configured store, or
with `--schema` against those files alone (exit status 2 and the reasons on standard error
when not expressible). `crates/nrese-store/examples/ql_print_check.rs` prints a query
directory in both forms, runs the printed queries on a store without reasoning and
compares the rows with NRESE's, writing the printed queries with `--out`.

**What is written.** The query is rewritten (§2) and each atom of a basic graph pattern
replaced by what makes it hold in the closure, as a `SELECT DISTINCT` subquery over the
atom's variables (the closure holds each statement once, so its matches stay bags of the
right size). Only the inclusions the materialisation applies (the *base* ones, §2) go into
an atom; what the others give comes from the rewriting's own branch, once, as in NRESE:
- a property: its sub-properties, inverses as `^p`, a transitive one as `(…)+`, a chain as
  `p/q`;
- a class: the classes below it, the existentials below it (`?x p ?fresh`), and the left
  sides of RL rules concluding it, unfolded (intersections as joins, qualified existentials
  and `hasValue` as edges, unions, enumerations as `VALUES`);
- the headline form (`paths`) reaches the hierarchy through the schema statements:
  `rdf:type/(rdfs:subClassOf|owl:equivalentClass|^owl:equivalentClass|owl:intersectionOf/rdf:rest*/rdf:first)*`,
  with the classes the closure puts below that the path doesn't reach listed; the secondary
  form (`values`) lists every class below in `VALUES` and property alternatives
  enumerated, computed from NRESE's hierarchy;
- an empty projection (a pattern without answer variables) as `{ FILTER EXISTS { … } }`,
  since `SELECT DISTINCT *` would keep every variable.

**Not expressible, said and never approximated:** equality (`owl:sameAs` in the data,
functional and inverse functional properties, keys, maximum cardinalities, same
individuals), `allValuesFrom`, `hasValue` and `hasSelf` on the right, reflexive
properties, recursive chains and class definitions (an RL rule through its own conclusion
at another individual), a variable predicate or class, the schema vocabulary in a query,
property paths over inferred properties, complex class assertions. NRESE's own status goes
with a printed query (`# NRESE completeness: …` on the command line): where NRESE says
`sound-only` (§7), the printed query gives the same answers and says the same. DL answers
(the `owl2-dl` bounds) aren't printed.

**Tested** (`printed_queries_answer_on_a_store_without_reasoning_as_nrese_with_it`, in the
gate): on the random pure and mixed cases of §6 and §7, every printed query in both forms,
as bags and with `DISTINCT`, on a store without reasoning, gives NRESE's rows and status.
On 6 October, 150 cases (the gate's): 300 pure queries printed (all of them) and 100 mixed,
58 of them `sound-only` as NRESE's; the rest refused for equality (functional properties,
`owl:sameAs`) or recursive chains. Two seeds of 1,000 cases: 4,000 pure and 711 mixed
queries printed, no difference. The test found three things: the empty projection above;
atoms expanded by every QL inclusion, which counted rows the rewriting adds once; and that
the materialisation applies inclusions through inverse expressions (`[owl:inverseOf p]` in
sub-properties, domains, ranges, restrictions: its facts are generalised triples), which
the TBox had marked as not applied (harmless for the rewriting, which then added answers
the closure had; now marked applied, the QL tests unchanged). LUBM(1)
(`univ-bench.owl`, its 14 queries and the 6 of `lubm-ql`) and OWL2Bench-QL(1) (its 10):
every query printed in both forms, the same rows as NRESE's on every one
(`ql_print_check`).
