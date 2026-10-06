# Reasoner v2: design and plan to state-of-the-art reasoning

Status: **plan** (2026-09-25). Decision record: [ADR-0003](../adr/0003-materialised-reasoning.md). Roadmap: Milestone 3 in [ROADMAP.md](../ROADMAP.md). The v1 reasoner this replaces is described in [spec/03](../spec/reasoning-semantics.md).

This document answers three questions:
- what "industry-leading reasoning" means in measurable terms
- which techniques get us there, and why each one was chosen
- in which order we build it, and what evidence moves each step to done

---

## 1. Targets

### 1.1 Who we measure against

| System | What it's best at | Published evidence we use |
|---|---|---|
| **GraphDB** (Ontotext) | The semantic reference. Its rulesets (`rdfs`, `rdfs-plus`, `owl-horst`, `owl2-rl`, `owl2-ql`), `.pie` rules, `isSupported`-style retraction, explicit/implicit graphs, proof plugin. | GraphDB 11.5 benchmark page, LDBC SPB-256 (256 M explicit statements) on r6id instances. Load plus materialisation takes 206 min for RDFS-Plus-optimized at 16 cores (402 M total statements; ≈ 21 k explicit st/s) and 470 min for OWL2-RL at 8 cores (775 M total; ≈ 9 k explicit st/s). Going from 8 to 16 cores gives no speedup. Under the SPB production load it serves 23.8 writes/s. |
| **RDFox** (Oxford Semantic) | The performance reference for materialisation. Parallel, lock-free materialisation; `owl:sameAs` by rewriting; B/F, FBF and counting maintenance. | Motik et al., AAAI 2014 and JAIR 2015 (parallel materialisation, near-linear speedup to 16 cores); AAAI 2015 (sameAs rewriting; B/F); IJCAI 2015 (maintenance with equality); AIJ 2019 (DRed vs FBF vs counting) |
| **Inferray** | Throughput on RDFS-class rulesets: vertically partitioned sorted arrays, sort-merge joins, cache efficiency | Subercaze et al., PVLDB 9(6), 2016 |
| **VLog / Nemo** | Column-oriented and trie-based datalog. Nemo is Rust, with worst-case-optimal joins. | Urbani, Jacobs, Krötzsch, AAAI 2016; Carral et al., ISWC 2019; Ivliev et al., ICLP 2023 and KR 2024 |
| **Modular maintenance** | Dedicated algorithms for transitivity, symmetry and equality inside a datalog engine, with orders-of-magnitude gains on recursive rules | Hu, Motik, Horrocks, AAAI 2018; Hu, Motik, Horrocks, AIJ 2022 |

**Outside the target** (tracked, not planned):
- **GPU datalog** (column-oriented datalog on the GPU, AAAI 2025): a research direction with large speedups over Nemo. Our deployment target is a CPU server.
- **Tableau-based OWL 2 DL:** GraphDB doesn't do it either.

### 1.2 Targets

The reference machine is documented in `benches/baselines/README.md`. Every comparison runs the competitor on the same machine: GraphDB Free in Docker; RDFox and Nemo where licences allow.

| # | Target | Metric | Goal |
|---|---|---|---|
| T1 | Semantic parity | Inferred-triple sets on LUBM, UOBM and SPB versus GraphDB, per ruleset | **Identical sets**, apart from documented, deliberate differences (§6.3) |
| T2 | Conformance | W3C OWL 2 test cases tagged for the RL profile, run as entailment and consistency tests | Pass all *approved* RL tests that the `owl2-rl` ruleset can decide |
| T3 | Bulk materialisation | Load plus materialise, `owl2-rl` and `rdfs-plus`, at LUBM-100 and at the largest SPB size that fits in memory | **≥ 10× GraphDB** end to end on the same machine; materialisation alone within 2× of RDFox if RDFox is available |
| T4 | Parallel scaling | Materialisation speedup with 1 → 8 → 16 threads | ≥ 6× on 8 cores; results bit-identical for every thread count |
| T5 | Commit-path reasoning | Reasoning-stage latency of a 1-triple ABox insert or delete at 100 M asserted, `owl2-rl` | p50 ≤ 1 ms, p99 ≤ 10 ms. 1 k-triple batch ≤ 50 ms. GraphDB's per-write cost measured alongside. |
| T6 | Maintenance correctness | Random insert/delete sequences | Incremental result equals rematerialisation in **100 %** of runs (property tests in CI) |
| T7 | TBox changes | Adding or removing a `subClassOf`, `subPropertyOf` or `inverseOf` axiom at 100 M | Cost proportional to the affected extension, reported. Never a silent full rematerialisation. |
| T8 | Memory | Bytes per inferred fact | ≤ 96 B/fact before Pf1 (half the asserted cost, §4.3); ≤ asserted bytes/fact after Pf1 |
| T9 | Explanations | One derivation tree of depth ≤ 10 | ≤ 10 ms, in every reject response and on demand |

**Where we go beyond GraphDB:**
- write-path reasoning latency (T5)
- parallel scaling (T4)
- proof-carrying rejects
- a verified-equivalent incremental path (T6)
- joint incremental SHACL over the materialised view

---

## 2. Architecture

```
ruleset (data) ─► Rule IR ─► analysis ─► schema compiler ─────► specialised program
 .pie import       (§3.1)    strata,      TBox closure,          ├─ batch executor (§4.1): full / large Δ
 built-ins                   recursion    list axioms, rule      ├─ delta executor (§4.2): commit Δ
                                          specialisation         └─ modules (§4.4): hierarchy, transitive,
                                                                    sameAs, symmetric/inverse
                                                   │
                  maintenance (§5): insert = semi-naive; delete = DRed → B/F / FBF
                                                   │
          engine: asserted stack + inferred stack, committed atomically in one revision (§4.3)
```

**Ownership** (ARCHITECTURE §2.1):
- `nrese-reasoner` owns everything above the engine line: pure functions of `(program, view, Δ)` that return an inferred delta, violations or explanations.
- `nrese-engine` owns the inferred stack, but not its meaning.
- `nrese-store` places the reasoner in the pipeline.
- `nrese-sparql` exposes the read models.

### 2.1 Where reasoning runs (the default: `timing = commit`)

By default, reasoning runs **inside the commit** (pipeline step 3). The inferred delta is committed together with the asserted delta, under the same revision.

- **Read-your-writes holds for inferences**, as in GraphDB.
- **A consistency violation rejects the write** before anything is published.
- **Recovery never re-reasons.** The WAL record carries both deltas.

The exception is large TBox changes over big extensions. When the planned work exceeds a configured budget, they run as a **reasoning job**: a long transaction that holds the writer slot, is visible in operator status and can be cancelled. Its cost is reported (T7), and it never falls back silently.

Other timings are configurable (§2.2).

### 2.2 What is fixed and what is configured

What reasoning is needed, when it runs and what a reader sees depend on the use case. So the reasoner is **configured, not hard-wired**.
- **Defaults** are the tuned fast path, and the §1.2 targets are measured on them.
- **Other modes** are supported and tested, and their extra cost is documented.
- **Configuration is parsed and validated at startup.** Unknown or contradictory settings are errors, never silent fallbacks.

**Fixed invariants.** Correctness depends on these, so they don't vary:
- **Asserted and inferred statements are stored separately and never overlap.** An explicit statement that is also derivable counts as explicit. Without this, the read models can't be correct.
- **Whatever reasoning a commit performs is atomic with it:** both stacks, one revision, one WAL record.
- **Inferences are always derivable from the asserted data.** They can't be written or retracted directly; retracting an asserted statement leaves re-derivation to the reasoner.

**Repository settings** (a *reasoning profile*; defaults first):

| Setting | Options | Notes |
|---|---|---|
| `ruleset` | `none`, `rdfs`, `rdfs-plus`, `owl-horst`, `owl2-rl`, `owl2-ql`, or a custom `.pie`/IR program | `none` costs nothing: no reasoning stage and an empty inferred stack |
| `timing` | **`commit`**, `deferred`, `on-demand` | `deferred`: writes don't wait, and a background reasoner commits inferred-only revisions. Readers can require the inferences to have caught up with a given revision. `on-demand`: materialisation only when triggered (API or operator), for batch pipelines. Changing the timing of a populated repository runs a reasoning job. |
| `consistency` | **`reject`**, `report`, `off` | `reject` requires `timing = commit`. Under `deferred`, violations are reported against the revision that caused them. |
| `placement` | **`default-graph`**, `graph:<IRI>`, `source-graph` | Where inferences live and which graphs rules range over. The first two put inferences in one graph and read rules over the union of graphs (GraphDB); the inferred stack keeps four permutations (three pattern orders plus PSO for joins). `source-graph` evaluates rules per named graph and keeps inferences there (RDFox/Stardog style); the inferred stack then needs six permutations, doubling its memory. Fixed at repository creation, because it sets the stack layout. |
| `same_as` | **`rewrite`**, `off` | `rewrite` stores representatives and expands them at read time (§4.4) |
| `maintenance` | **`auto`**, `dred`, `bf`, `counting` | For experts and benchmarks; `auto` chooses per stratum (§5) |
| `axioms` | **`partial`**, `full` | GraphDB's `partialRDFS` distinction (§6.3) |
| `tbox_job_budget` | a work estimate | Above it, TBox changes run as reasoning jobs (§2.1) |

**Per request:**
- **Read model:** `materialised` (default), `asserted` or `inferred`. Selected by an HTTP parameter (RDF4J's `infer=false` maps to `asserted`) or by the pseudo-graphs `onto:explicit` / `onto:implicit`.
- **Explanations:** proofs for results or rejects.
- **Freshness under `deferred`:** "wait until inferences cover revision r" (default: don't wait).

**Per operation:**
- **Bulk load:** `--reason` materialises through the batch executor. `--validate` runs the gates (SHACL, consistency) over the result. The default is a raw load, which marks the repository as needing reasoning when a ruleset is configured.
- **Backup:** `--include-inferred` exports the inferred stack too, so a restore doesn't re-derive it. The default is asserted only.

Every setting appears in operator diagnostics (the active profile, and the inference freshness under `deferred`). Each non-default mode has its own tests in the §7 evidence suite.

---

## 3. Semantics layer

### 3.1 Rule IR

```text
Rule    { id, name, body: [Atom], guards: [Guard], head: Head }
Atom    = (Term, Term, Term)            // triple pattern; graph is the union (§6.2)
Term    = Var(u16) | Const(TermId)
Guard   = Neq(Term, Term) | Builtin(…)  // .pie "Constraint"; datatype tests
Head    = Facts([Atom]) | Inconsistent(ViolationKind)
Program { rules, strata, recursive_sccs, consistency_rules }
```

- **Rules are data.** Rulesets are values of this type. The built-in ones are defined in Rust as tables, with the W3C OWL 2 RL rule names (`prp-dom`, `cax-sco`, `eq-rep-s`, …), not as parsed text.
- **`.pie` import.** It covers `Prefices`, `Axioms`, `Rules` and `Consistency`, `[Constraint …]` and `[Cut]`. Export exists too, so round-trip tests run against GraphDB's published rulesets.
- **Analysis** is a pure function over the IR. It computes the predicate dependency graph, its SCCs (recursion), stratification, and which atoms are *schema atoms*: atoms whose predicates are only ever derived from TBox vocabulary.

### 3.2 Schema compiler (TBox specialisation)

OWL RL rules mostly join one small schema relation with one large instance relation, for example `cax-sco: (?c1 subClassOf ?c2), (?x type ?c1) → (?x type ?c2)`. Evaluating that as a join per round is the main waste in generic engines. The approach taken by WebPIE, Inferray and GraphDB's compiled rules is to evaluate the schema side once.

1. **TBox closure first.** Compute the closure of the schema stratum (`scm-*`: subclass and subproperty hierarchies, equivalences, domain and range propagation, inverses) with the hierarchy module (§4.4).
2. **Specialise instance rules** by partial evaluation over that closure.
   - A rule with a schema atom becomes a *dispatch table*, for example `type(x, c) → type(x, c')` for every `c' ∈ sup⁺(c)`, stored as `c ↦ [c']`.
   - After specialisation almost every instance atom has a **constant predicate**. Variable-predicate atoms (`rdfs2/3/7`, `prp-spo1`, `prp-inv`) turn into one rule per schema binding.
3. **Compile list axioms.** `owl:propertyChainAxiom`, `owl:intersectionOf`, `owl:unionOf`, `owl:hasKey`, `owl:AllDifferent`, `owl:AllDisjointClasses`/`Properties` and `owl:oneOf` are read from the RDF lists in the TBox and compiled into rules of fixed arity. For example, a chain of length n becomes one n-atom rule.
   - This removes the `rdf:first`/`rdf:rest` helper rules that generic `.pie` rulesets need.
   - It fixes v1's defect of never seeing Turtle list syntax.
   - Malformed lists produce diagnostics, never a silent skip.
4. **Soundness condition.** Specialisation is exact as long as the TBox closure doesn't change during the instance fixpoint. If instance rules derive schema facts (for example `owl:sameAs` between classes, or punning), the analysis flags those rules as *schema-feeding*. The evaluator then runs an outer fixpoint: new schema facts → re-specialise the affected rules → continue. Correct in general, and fast in the common case.

### 3.3 Rulesets

| Ruleset | Content | Notes |
|---|---|---|
| `rdfs` | RDFS entailment rules (without the axiomatic container-membership rules, like GraphDB's `partialRDFS` default) | |
| `rdfs-plus` | RDFS + `inverseOf`, `SymmetricProperty`, `TransitiveProperty`, `FunctionalProperty`/`InverseFunctionalProperty` equality, `equivalentClass`/`Property`, `sameAs` | GraphDB's most-used ruleset |
| `owl-horst` | pD* (ter Horst): RDFS-plus + `hasValue`, `someValuesFrom`/`allValuesFrom` (the pD* forms), `differentFrom` consistency | |
| `owl2-rl` | The W3C OWL 2 RL/RDF rules, all tables: eq, prp, cls, cax, dt (partial, §6.3), scm | |
| `owl2-ql` | GraphDB's materialisable OWL 2 QL variant | Query rewriting is an optional stretch (R8) |
| `custom` | Any `.pie` or IR program | Validated by the analysis (safety, arity) |

Every ruleset has two variants, like GraphDB's "optimized" rulesets:
- the **full** variant is the specification
- the **optimised** variant drops rules whose consequences are rarely queried (for example `rdf:type rdfs:Resource`)

Both are covered by the parity tests.

---

## 4. Execution layer

### 4.1 Batch executor: full materialisation and large deltas

Used for initial loads, restores, ruleset changes and deltas above a size threshold.

What the code does (`batch.rs`, `eval.rs`; checked against the code on 6 October 2026):

- **Working set: vertical partitioning.** It is built from one snapshot scan (the store streams its POSG order, already grouped) as per-predicate binary relations of `(u64, u64)` pairs, sorted by subject. The **OS order** is kept only for relations a ground rule looks up by object alone, or grounding reads (P1-F11), so a fact costs 16–32 B. `rdf:type` is one relation like the others (no per-class sets).
  - This is Inferray's layout, and it is also what QLever's permutations amount to per predicate.
  - Each relation holds its input in a run of its own and what was added in a base run and a small recent run, which takes each round's delta and is folded into the base once it reaches a quarter of it; runs that are merged are held in chunks of 1 Mi pairs (P1-F4, F10).
  - Sorting is comparison-based (rayon's parallel unstable sort; a sample sort into chunks for large unions), not radix (kernel #9 of the 6 October investigation is open).
- **Semi-naive evaluation.** A rule with n atoms runs n delta variants (atom i on Δ, atoms before i on old, atoms after i on old ∪ Δ), so no derivation is repeated across rounds.
- **Joins: index nested loops.** A variant's driver atom's matches are read in place, by position, from the runs (P1-F1); each further atom, ordered by bound positions and then estimated matches, is a lookup by binary search in each run. There is no sort-merge join, galloping or Leapfrog Triejoin yet (a seekable cursor kernel, #3 of the investigation, would make the lookups of sorted probes cheap).
  - **Dispatch** (§3.2): ground instances are indexed by the predicate (and class) of each atom, so a round runs only the variants whose atom can match its delta. Instances whose head is one of their body atoms (`(?x type C) → (?x type C)` from `C ⊑ C`) are dropped at grounding (R1).
- **Deduplication without locks.** Each morsel sorts its candidate facts by (predicate, subject, object), drops duplicates and checks them against the working set in that order (P1-F8). The round's new facts stay in their morsels' per-predicate lists until each predicate's are merged into chunked runs by a sample sort (P1-F9). There's no shared hash set, so there's no contention, and the output is deterministic regardless of thread count (T4).
- **Parallelism:** morsel-driven (Leis et al., SIGMOD 2014) through rayon.
  - Work units are (rule variant × morsel of 4,096 driver matches), so skewed predicates split instead of serialising.
  - RDFox's lock-free design is the benchmark to meet (T3). We deliberately take the sort-based route instead: it is simpler to make deterministic, and it matches our immutable-run storage.
- **Equality** by representatives runs within the rounds (§4.4).
- **Output:** the new inferred facts are sorted once more, into the inferred stack's permutations, and installed as base runs through the bulk run builder shared with E5. They never go through the per-commit path.

### 4.2 Delta executor: the commit path

Used for typical interactive writes, where Δ is between 1 and ~10⁴ facts.

- **Same compiled program, different access method.** Index-nested-loop joins over the engine view (asserted ∪ inferred ∪ pending), using the six permutations: `(s p ?)` → SPOG prefix, `(? p o)` → POSG, `(s ? o)` → OSPG.
  - Rules match over the union of graphs, so a triple asserted in several graphs is deduplicated per rule instance.
- **Dispatch tables** (§3.2) make the hot rules O(1) plus output size.
- **Adaptive switch.** When the planner's estimate (Δ size × rule fan-out from predicate statistics) crosses a threshold, it switches to the batch executor.
- **The oracle.** Both executors are differential-tested against each other and against the naive reference evaluator (§7.1).

### 4.3 The inferred stack in the engine (decision D2, refined)

- **Disjoint from asserted data.** `inferred = Mat(P, asserted) \ asserted`, and this invariant is checked by the tests.
  - A union scan merges two disjoint sorted streams and never needs to deduplicate.
  - An explicit statement that is also derivable counts as explicit, as in GraphDB.
  - Deleting an asserted fact that is still derivable moves it into the inferred stack. The maintenance step does this (§5).
- **The layout follows `placement` (§2.2).**
  - `default-graph` and `graph:<IRI>` keep inferences in one constant graph, so the stack needs only **three permutations** (SPO, POS, OSP) instead of six. That halves its memory (T8) at the same key width; key-width specialisation and Pf1 compression reduce it further. E6 implements the default-graph case; generalising it to any single graph is part of R4.
  - `source-graph` uses the six-permutation quad layout, which the engine already supports for the asserted stack.
- **One `Version` holds both stacks.**
  - A commit publishes the asserted and inferred deltas atomically under one revision.
  - The WAL record and checkpoint carry both, and recovery never needs the reasoner.
  - Only the reasoner writes the inferred stack. The engine exposes inferred writes on the transaction as a separate API; `nrese-sparql` and the request paths never call it, and only the pipeline's reasoning stage in `nrese-store` does.
- **Read models** (R4):
  - `Materialised` = asserted ∪ inferred, the default.
  - `Asserted`: explicit statements only.
  - `Inferred`: implicit statements only.
  - They are selectable per request and through GraphDB's pseudo-graphs `FROM onto:explicit` / `FROM onto:implicit`.
  - A `ReadView` is a (snapshot, model) pair, so SPARQL, the Graph Store Protocol, export and SHACL all share one mechanism.

### 4.4 Modules: dedicated algorithms for recursive shapes

Generic semi-naive evaluation of transitive or equality rules makes O(n·closure) redundant derivations. Following Hu, Motik and Horrocks (AAAI 2018, AIJ 2022), recursive components with known shapes are handed to modules. Each module implements `materialise(Δ⁺)`, `maintain(Δ⁺, Δ⁻)` and `explain(fact)` behind one trait.

| Module | Rules covered | Algorithm |
|---|---|---|
| Hierarchy | `scm-sco`, `scm-spo`, `scm-eqc*`, `scm-eqp*`, `cax-sco` fan-out | As built: `scm-sco` and `scm-spo` are transitivity rules, so the transitive module below closes `subClassOf` and `subPropertyOf` (no bitset reachability), and `cax-sco` is grounded into one ground rule per edge of the closed hierarchy (`c ↦ sup⁺(c)` as dispatch entries). Since R13 (6 October 2026), what `cax-sco` (and `prp-spo1` over the property hierarchy) derived is a part of the delta of its own, which their instances don't read: a derived type already has all its ancestors, so a deep hierarchy costs one binding per inherited type, not one per ancestor of each. Deletes go through DRed/B/F like other rules. |
| Transitive property | `prp-trp` per transitive property | SCC condensation (`nrese-exec::graph`); the closure is materialised (GraphDB semantics) but computed without redundant joins. As built, the batch executor recomputes a predicate's closure in each round other rules added edges to it; the delta executor closes per new edge. Deletes: the affected components only. |
| Equality | `eq-sym`, `eq-trans`, `eq-rep-*`, and `prp-fp`, `prp-ifp` and `prp-key` as producers | Union-find over `TermId`s with **rewriting** (Motik et al., AAAI 2015), the smallest id representing its class. As built (6 October 2026), the batch executor merges classes within its semi-naive rounds: a round's new `sameAs` between two representatives merges their classes, and only the facts mentioning the representative that lost its place are rewritten into the next round's delta (egglog's rebuild; the rules read only facts over representatives). Stored over representatives with the read view expanding members at scan time (`equality = "compact"`), or expanded. Commits: the delta executor reasons over the expanded view; in compact mode a commit that merges classes rewrites the stored facts that mention a former representative in the same transaction (G5, 6 October 2026), while one that deletes an equality (a possible split) commits without its inferences and re-materialises after it (B3 of the investigation; Motik et al., IJCAI 2015 is the plan). |
| Symmetric / inverse | `prp-symp`, `prp-inv1/2` | As built: ground one-atom rules (`(?x p ?y) → (?y q ?x)`), no module |
| Equivalence property | `prp-trp` + `prp-symp` on the same property (plus reflexivity if declared): an equivalence relation | Union-find over the property's edges gives the components in O(n + m). The closure is every pair within a component. It's either materialised in one pass without joins, or stored as components and expanded at read time like sameAs. That choice is a profile setting (D7): `materialise` by default for GraphDB parity, `compact` for large components. |

**Evidence (reasoning benchmark, 2026-09-27).** In OWL2Bench RL(1) and DL(1), 55 k asserted triples, `hasSameHomeTownWith` is symmetric and transitive. Its closure is 1.31 M triples: hometown groups of about a thousand people, each closed into a clique. With generic rule evaluation, which is O(k³) per group, every baseline takes 15 to 31 minutes:

| System | Time | Peak memory |
|---|---|---|
| NRESE v1 | 900 s | 0.9 GB |
| Jena OWL-micro | 1,439 s | 5.7 GB |
| Nemo | 1,859 s | 25 GB |

The owlrl oracle doesn't finish within an hour. The equivalence and transitive modules should do this in well under a second: union-find, then writing out the pairs. That makes it the clearest demonstration of what modules are for.

**Equality semantics.** Queries under the `Materialised` model see the fully expanded sameAs semantics, exactly as if every rewritten fact were materialised for every member. This matches GraphDB with sameAs enabled, without the O(k²) blow-up for cliques of size k. There is a per-repository switch to disable sameAs, like GraphDB's `disable-sameAs`.

---

## 5. Maintenance (truth maintenance)

- **Insert:** semi-naive evaluation seeded with Δ⁺ (plus the facts Δ⁺ implies through modules).
- **Delete**, in two stages. Both are required to agree with rematerialisation (T6).
  1. **DRed** (Gupta, Mumick, Subrahmanian, SIGMOD 1993) comes first, as the simple, robust baseline. It overdeletes everything derivable from Δ⁻, then rederives what still has support.
  2. **B/F and FBF** (Motik et al., AAAI 2015; AIJ 2019) are the default for non-module strata. Each deleted fact first gets a backward check for an alternative proof, GraphDB's `isSupported`, and only unsupported facts propagate forward. This avoids DRed's overdeletion blow-up when facts have many derivations, which is typical for type hierarchies.
- **Counting** (per-fact derivation counts) is the AIJ 2019 winner for non-recursive strata, but it needs persistent per-fact state. It is **evaluated in R5** against FBF on the benchmark mix, and adopted only if FBF misses T5 on non-recursive strata.
- **TBox deltas** change the specialised program itself:
  - An added axiom evaluates the *new rule instances* against the full current state. For example, a new edge `C ⊑ D` gives every member of `C` and its subclasses the types `D` and `sup⁺(D)`.
  - A removed axiom runs DRed or FBF seeded by the removed rule instances.
  - Both run through the batch executor when the extension is large (§2.1).
- **Moving facts between stacks:**
  - Deleting an asserted fact that is still supported moves it to the inferred stack.
  - Asserting a previously inferred fact moves it from inferred to asserted.
  - Both follow from the disjointness invariant and are covered by the property tests.
- **Atomicity.** Every maintenance step writes into the same transaction as the asserted delta. An error or cancellation discards both.

---

## 6. Consistency, explanations, semantics details

### 6.1 Consistency and explanations

- **Consistency rules** are `Inconsistent(kind)` heads, evaluated incrementally like any other rule, and imported from `.pie` `Consistency` sections. For `owl2-rl` they cover:
  - `cls-nothing2`
  - `cax-dw`, `cax-adc`
  - `eq-diff1/2/3`
  - `prp-irp`, `prp-asyp`, `prp-pdw`, `prp-adp`, `prp-npa1/2`
  - `cls-com`
  - `cls-maxc1`, `cls-maxqc1/2`
  - `dt-not-type` (partial)
- **Explanations:** backward proof search over the program, reusing the B/F backward step. It returns one shortest derivation tree down to asserted facts, bounded by depth and time. Explanations are recomputed rather than stored, as RDFox does, so they cost no memory.
  - **Reject responses** carry the violation's derivation tree, which replaces v1's heuristic blame.
  - An **on-demand API** returns the proof for any inferred fact, with a GraphDB proof-plugin-compatible SPARQL form as an extension function.

### 6.2 Graph semantics

- **Default (`placement = default-graph`):** rules match over the **union of all graphs**, and inferences go to the default graph. This is GraphDB's behaviour.
- **`graph:<IRI>`** is the same, with inferences in a dedicated named graph.
- **`source-graph`:** rules are evaluated per named graph, and inferences stay in the graph whose statements derived them. Schema statements can be shared from a configured set of graphs. The IR's atoms carry a graph term for this (R9).

### 6.3 Deliberate differences, each pinned by a test

- **Datatype rules (`dt-*`).**
  - Equality and difference are decided on values for inline canonical types (E1).
  - For other datatypes they are lexical, and `dt-type2`/`dt-not-type` are applied only for datatypes we validate.
  - This is documented per datatype; GraphDB is partial here too.
- **Literal identity.** Literals keep their lexical form (spec 02). GraphDB may canonicalise them, and parity comparisons normalise literals before comparing.
- **Axiomatic triples.** Axiomatic triples that are never queried (container membership `rdfs:member` axioms) follow GraphDB's `partialRDFS` default. The switch to full is per ruleset.

---

## 7. Evidence

### 7.1 Correctness

| Test | What it proves | Runs |
|---|---|---|
| **Naive reference evaluator** | A 200-line naive fixpoint over a `BTreeSet`: obviously correct and slow. It is the oracle for everything below. | CI |
| Executor differential | Batch = delta = naive, on random programs × random datasets (proptest) | CI |
| Module differential | Each module equals the generic rules it replaces, including after deletes | CI |
| Maintenance differential | Random insert/delete sequences: incremental equals rematerialisation (T6); stack disjointness invariant | CI |
| Specialisation soundness | Schema-feeding programs (punning, class `sameAs`) against the unspecialised evaluation | CI |
| W3C OWL 2 RL conformance | T2 | CI |
| v1 fixture suite | The `rules-mvp` fixtures (FOAF, Time, ORG, SKOS, PROV-O, DCAT, vCard, DCTerms, SOSA, SSN, ODRL) pass on v2, then v1 is deleted | CI |
| GraphDB parity | LUBM, UOBM and SPB inferred sets, diffed against GraphDB Free running in Docker (T1) | recorded |

### 7.2 Performance

- **Harness:** a `reason` command in `nrese-bench-harness` with generators built in, so runs are hermetic:
  - a Rust port of the LUBM UBA generator
  - UOBM
  - OWL2Bench (ISWC 2020; covers the EL, QL and RL profiles)
  - an SPB sample
  - our RG dataset
- **Metrics:**
  - full materialisation time and facts/s
  - thread scaling
  - peak memory and bytes per inferred fact
  - commit-path latency distributions for insert and delete at Δ = 1, 10, 1 k and 100 k, against rematerialisation
  - TBox-change cost
  - explanation latency
- **Recording:** results go to `benches/baselines/reasoning-*.json` together with the commit, machine and competitor versions.
- **Memory ceiling:** until Pf1 (compressed runs), in-memory capacity is about 190 B per asserted quad. Full-scale SPB-256 with `owl2-rl` (775 M statements) therefore needs Pf1. The ladder runs LUBM-1/10/100 and SPB at sizes that fit, and the full-scale comparison follows Pf1.

---

## 8. Work packages (Milestone 3)

| WP | Scope | Size | Done when |
|---|---|---|---|
| **R0 Engine: inferred stack** | Second index stack in `Version` (three permutations), atomic dual-stack commits, WAL and checkpoint format carrying both stacks, separate inferred-write API on the transaction, bulk run install. **Pulled forward into M1 as E6**, while the v2 on-disk format has no deployments to migrate. | M | Engine model tests cover both stacks; crash tests recover both; the stack disjointness invariant is enforced in debug builds |
| **R1 Rule IR, analysis, rulesets, profile** | IR (atoms with a graph term), analysis (SCCs, strata, schema atoms), built-in rulesets as data, `.pie` import/export, **naive reference evaluator**; the reasoning-profile schema of §2.2, validated at startup | M | `.pie` round-trip on GraphDB's published rulesets; the reference evaluator passes hand-written RDFS/RL fixtures |
| **R2 Schema compiler + batch executor** | TBox closure, specialisation, list-axiom compilation, the vertically partitioned working set, semi-naive evaluation, sort-merge and LFTJ joins, morsel parallelism, bulk install of the inferred stack | L | Batch equals naive (proptest); LUBM-1/10/100 counts equal GraphDB for all rulesets (T1); T3 and T4 recorded |
| **R3 Modules** | Hierarchy, transitive, equality (rewriting plus read-time expansion), symmetric/inverse, equivalence property | M | Each module equals its generic rules; sameAs semantics tests; UOBM parity; OWL2Bench RL(1) materialised in under 1 s (baselines: 15–31 min) |
| **R4 Read models + commit-path reasoning** | Per-request read model over HTTP (`infer`, `onto:explicit`/`onto:implicit`); delta executor in the mutation pipeline with insert maintenance (`timing = commit`); single-graph layout for any `graph:<IRI>` placement; `ruleset = none` as a zero-cost path; bulk load `--reason`; stats and diagnostics for the active profile | M | Query tests for all three models (audit F4 closed); T5 for inserts; `none` adds no measurable write cost |
| **R5 Truth maintenance + timings** | DRed, then B/F / FBF (`maintenance` setting); TBox deltas; sameAs maintenance; stack moves; reasoning jobs for large TBox changes; `deferred` and `on-demand` timings with inference-freshness tracking; the counting evaluation | L | T6 in CI for every timing; T5 for deletes; T7 recorded; `deferred` reaches the same state as `commit` |
| **R6 Consistency + explanations** | Consistency rules with the `consistency` setting (`reject`/`report`/`off`), proof search, proof-carrying rejects, proof API; the v1 fixture suite on v2; **v1 `rules-mvp` deleted** | M | T2 and T9; v1 fixtures green on v2; `rules_mvp*` modules gone |
| **R7 RDF 1.2 triple terms** (D4) | `TermKind::Triple` (the dictionary key is three component ids), parsers/serialisers, SPARQL-star functions; no inference inside quoted triples (GraphDB semantics) | M | RDF 1.2 syntax and SPARQL tests |
| **R8 Beyond GraphDB** (stretch, each an independent decision) | EL classification module (consequence-based, ELK-style: Kazakov, Krötzsch, Simančík, JAR 2014) for large terminologies like SNOMED; OWL 2 QL query rewriting (PerfectRef/Ontop style) as a zero-materialisation mode | L each | Per item |
| **R9 Graph-scoped reasoning** | `placement = source-graph`: per-graph rule evaluation, shared schema graphs, quad layout for the inferred stack, maintenance per graph | M | Per-graph results equal evaluating each graph separately; the memory cost is recorded |

**Order:** R0 (as E6, now) → R1 → R2 → R3 → R4 → R5 → R6, with R7 anywhere in M3. R9 follows R6, unless a ResearchSpace or DMW need moves it earlier.
- R1 and the reference evaluator come before any optimised code, because they are the oracle (roadmap rule 2).
- R4 comes before R5 so the product gets queryable inferences early. Until R5, deletes under reasoning use DRed through the batch executor: correct, but not yet fast.

---

## 9. Sequencing within the whole roadmap

Decision D1 stands: **governance (M2) comes before reasoning (M3).** Two adjustments follow from this plan:

1. **R0 is pulled into M1 as E6.** The inferred stack changes the WAL and checkpoint formats. Doing it now, while no v2 data exists anywhere, avoids a format migration. It also lets SHACL (S2) be built against the final `ReadView`/read-model shape.
2. **E5 (bulk load) builds the shared bulk-run installer** that R2 also uses.

Resulting order: **E6 → E5 → E1 rest → Q1 rest → M2 (S1, S2, T1, X1, …) → M3 (R1 …)**.

M3 depends on M2 only through the shared `ReadView`. If reasoning becomes more urgent than governance, M3 can start straight after M1 without redesign.

---

## 10. Risks

| Risk | Mitigation |
|---|---|
| Specialisation is unsound for exotic TBoxes (punning, schema derived from data) | The analysis flags schema-feeding rules and an outer fixpoint covers them (§3.2). A dedicated differential test runs against unspecialised evaluation. |
| sameAs rewriting makes maintenance hard (IJCAI 2015 is intricate) | The equality module is isolated behind the module trait. Fallback: DRed over the rewritten program, which is correct and slower, with the cost reported. |
| Sort-based parallelism is slower than RDFox's lock-free hash approach on some workloads | Measured in T3. The dedup step is isolated in one function, so a concurrent hash-set variant can be added if the numbers demand it. |
| Memory before Pf1 | Full-scale benchmarks wait for Pf1. The inferred stack uses three permutations (T8). |
| GraphDB ruleset details are only partly documented | Parity is established by diffing inferred sets against a running GraphDB, not by reading documentation. Every difference is either fixed or documented in §6.3. |
| TBox changes are expensive at scale | Reasoning jobs with visible cost and cancellation (§2.1). T7 is reported, never hidden. |
