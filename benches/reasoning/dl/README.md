# OWL 2 DL: reference reasoners, the W3C suite, canonical taxonomies

The correctness infrastructure of [docs/design/owl2-dl.md](../../../docs/design/owl2-dl.md)
§11, work package 2.3. Everything NRESE's DL engines are judged by goes through here.

| File | What |
|---|---|
| `runner/` | NRESE's own runner for HermiT 1.4.5.519, ELK 0.6.0 and Openllet 2.6.5, all on OWL API 5.1.20, and, as processes of their own, KoncludeCLI 0.7.0-1138 and ELK 0.4.3's command line (`elk-0.4.3`, classification only; the detail column adds its own stage times, since the task includes starting its JVM; `ELK043_WORKERS` sets its threads): consistency, entailment and classification with a timeout per task (a fresh JVM after a timeout), a batch per JVM, the canonical taxonomy, conversions to N-Triples and functional syntax. Not ROBOT, not OWLLink (§11). |
| `Dockerfile` | builds the runner into `nrese-bench/dl-reference` |
| `reference.py` | runs a manifest on the image with a watchdog (a reasoner that ignores interrupts is stopped and recorded as `timeout`); resumable |
| `w3c.py` | the W3C OWL 2 test cases of species DL under the direct semantics: `prepare`, `manifest`, `compare` against the test types, `published` and `against` for the working group's results of 14 December 2009 |
| `published-2009.tsv` | those results, extracted from the archived `Test_results.html` (FaCT++, HermiT, Pellet) |
| `ore.py` | the ORE 2015 development subset (`subset` by track and size, `manifest`, `compare` by canonical hashes); the corpus is fetched by hash into `.cache/ore2015/`, never committed (CC BY-NC-ND) |
| `nrese.py` | NRESE's classifiers in the same comparison (`el`: the EL classifier on the EL track, through the runner's `ntriples` conversion and `canonical.py`; `dl`: the DL driver, `dl_classify`, on the DL tracks' classification and realisation tasks, `--only` for a few); the detail column holds the hash, then the classifier's profile: its phase times in ms and its counters (EL: concepts, contexts, subsumers, links, conclusions; DL: tests, candidates, branch points, clashes, the Horn bound's share), or for an incomplete DL run the reasons it printed |
| `cargo run -p nrese-owl --example fuzz` | random OWL 2 DL ontologies (`nrese_owl::fuzz`, within the global restrictions) as functional-syntax files with a manifest: differential fuzz campaigns on the references, `ore.py compare` lists disagreements |
| `canonical.py` | the canonical taxonomy from any subsumption closure (NRESE's EL classifier, later `nrese-dl`), with the reference run's signature |
| `disputed.tsv` | differences settled by hand: `test/task<TAB>reasoner<TAB>result<TAB>why` |
| `scope.py` | the small-scope campaign (docs/design/owl2-dl.md §11): every OWL 2 DL ontology up to a bound (`nrese_owl::scope`, `cargo run -p nrese-owl --example scope`) or a covering array (`cover:3`), answered by NRESE (`cargo run -p nrese-dl --example scope_check`) and the references (`runner`'s `nrese.dl.Scope`, many ontologies per JVM, one functional-syntax document a line); `run`, `second` (Openllet where HermiT doesn't decide or differs from NRESE), `compare`, `record` (the gate's answers, `crates/nrese-dl/tests/scope`) |
| `scope-adjudicated.tsv` | the small-scope cases of the gate's scope the references don't settle, each with the answer the semantics gives and why |

## The canonical taxonomy

One line per class, `= rep member`, the representative being the smallest IRI of its
equivalence class (`owl:Nothing` for unsatisfiable classes, `owl:Thing` for classes
equivalent to it), and one line per direct subsumption between representatives,
`< sub super`; sorted. Equal taxonomies have equal SHA-256 hashes, so a corpus run
compares by hash and opens files only where hashes differ.

## Running

```bash
docker build -t nrese-bench/dl-reference benches/reasoning/dl
cd benches/reasoning/dl
python w3c.py prepare ../../../.cache/owl-test/all.rdf ../../../tmp/dlw3c
python w3c.py manifest ../../../tmp/dlw3c hermit openllet konclude elk   # ELK: EL tests only
python reference.py run ../../../tmp/dlw3c --timeout 20
python w3c.py compare ../../../tmp/dlw3c --out results/w3c-$(date +%F).tsv
python w3c.py against ../../../tmp/dlw3c published-2009.tsv hermit
rm -rf ../../../tmp/dlw3c
```

## Results so far (3 October 2026, main PC)

- **W3C, direct semantics, species DL** (411 tasks of 311 tests; 20 s per task, the
  published passes HermiT missed rerun at 120 s; `results/w3c-2026-10-03.tsv`).
  - **HermiT:** 382 pass, no wrong result. Against the working group's published HermiT
    results of 2009 it reproduces 351 of the 362 published passes. Of the rest, ten time
    out at 120 s, and HermiT 1.4.5 on OWL API 5 crashes on `WebOnt-Thing-003`.
  - **Openllet:** reproduces 366 of Pellet's 369 published passes.
  - **Konclude 0.7.0-1138:** answers wrongly on 30 tests. These are its own answers, not
    the harness's: on a minimal ontology an irreflexive property with a self-loop
    assertion comes out consistent, while functional-property and domain clashes come out
    inconsistent.
  - **ELK:** 93 pass of the 114 EL-profile tasks; the rest use constructs outside the
    fragment it supports (keys, `Self`, negative assertions, datatypes).
  - Every difference is recorded in `disputed.tsv` with what settles it; one is open
    (`WebOnt-description-logic-909`: Konclude says consistent, HermiT and Openllet time
    out).
- **ORE 2015 development subset** (80 tasks: 40 DL and 20 EL classifications, 20 DL
  consistency checks up to 5 MB; 300 s per task; `results/ore-dev-2026-10-03.tsv`).
  - 79 agree across HermiT, Konclude and Openllet, ELK on the EL ones, and NRESE's EL
    classifier on all 20 EL ontologies.
  - One disagreement among the references: `ore_ont_1340`, where Openllet differs from
    HermiT and Konclude; to settle.
  - Solved: HermiT 80, Konclude 79, Openllet 77, ELK and NRESE 20 of 20.
  - The comparison found a gap in NRESE's EL output: classes equivalent to `owl:Thing`
    were not reported as such. Fixed (`Classification::top`).
- **A differential fuzz campaign** (60 random SROIQ(D) ontologies from
  `cargo run -p nrese-owl --example fuzz`): 57 agree; in 3, Konclude differs from HermiT
  and Openllet.

## The small scope (12 October 2026, main PC)

```bash
cargo build --release -p nrese-owl --example scope -p nrese-dl --example scope_check
python scope.py run 4,4,4 DIR --reasoners hermit --jobs 6   # NRESE and HermiT, in slices
python scope.py second 4,4,4 DIR                             # Openllet on the rest
python scope.py compare DIR
python scope.py record 3,3 DIR ../../../crates/nrese-dl/tests/scope/reference-3-3.txt \
  --adjudicated scope-adjudicated.tsv                        # the gate's answers
```

The runner's jar from the image (`docker cp`) and `nrese.dl.Scope` compiled against it run
on any JDK 21+ (`DL_RUNNER_JAR`, `DL_RUNNER_CLASSES`). Each case is answered with a code:
`i` (inconsistent), or `c` and ten bits (A, B satisfiable, `A ⊑ B`, `B ⊑ A`, `⊤ ⊑ A`,
`⊤ ⊑ B`, `a : A`, `a : B`, `b : A`, `b : B`).

| Campaign | Cases | NRESE and every answering reference agree | NRESE differs from references that agree | references differ | none answered |
|---|---|---|---|---|---|
| scope `4,4,4` (every ontology of 1 axiom up to size 4, of 2 and 3 up to total size 4) | 2,761,335 | 2,760,771 | 421 | 139 (NRESE with Openllet 82, HermiT 57) | 4 (2 with every claim certified) |
| scope `3,3` (the gate's) | 35,505 | 35,454 | 3 | 48 | 0 |
| covering array of triples (`cover:3`, 3 to 6 axioms of up to size 4; every one of the 47,905 triples of the 67 features) | 3,354 | 3,312 | 3 | 38 (NRESE with HermiT 37, Openllet 1) | 0; 1 NRESE gave up at the campaign's 20 s |

**Every disagreement is settled by the semantics, in NRESE's favour:**
- *NRESE differs from both references* (421 + 3 + 3): every one involves
  `owl:topObjectProperty` or a property it makes universal, the forms NRESE answered
  `unsupported` before 12 October: 348 a key over U (any two named members of the key's
  class share a named U-value, each other, so the key makes them one; both references apply
  keys to property assertions only), 73 U as a subproperty, in a chain, an equivalence or
  an inverse (`U ⊑ r⁻` with `¬r(a, b)`, `U ∘ U ⊑ r` with `r ⊑ ⊥`: inconsistent; both
  answer consistent).
- *The references differ:* Openllet reads U as an ordinary property (`∃U.{a}` is `⊤`),
  misses `owl:bottomDataProperty` (`d ⊑ ⊥D` with a value of `d`), answers `≥ 0 r.⊥`
  inconsistent and `≥ 2 r.{a}` consistent, and doesn't see that `Self(r)` and `Self(r⁻)`
  are one class; HermiT errs on a disjoint union with a nominal (`DisjointUnion(A, ∀U.X,
  {a})` makes `A ≡ {a}`, HermiT entails `b : A`; for 13 such cases NRESE's checked model has
  `b ∉ A`) and with a complement (`DisjointUnion(A, B, ¬B)`: HermiT entails `⊤ ⊑ A` but not
  `a : A`). NRESE agrees with one reference in every one of them.
- *None answered* (HermiT's time-outs, Openllet's errors): `⊤ ⊑ =2 r⁻.(=2 r.∃r.{a})` has the
  model of two elements with `r` the whole relation; NRESE answers consistent.
- The gate's 51 are in `scope-adjudicated.tsv`, one line each.

**What the campaign found in NRESE** (each fixed with a guard, 12 October): operands of a
disjointness the U rewriting made equal were dropped (cases 623, 677, 732, 786: answered
consistent, inconsistent); keys over a transitive property read asserted edges only
(found by the review the campaign prompted; HermiT reads them so too, Openllet doesn't);
the context core took keys to need role assertions, and the classifier called an
ontology with a key over U alone consistent (1219488); realisation took an individual no
assertion mentions for a fresh element, past a key (1267640).

**Reference failures** (scope `4,4,4`): HermiT throws on 344 (333 "operands cannot be null or
empty", on inconsistent inputs such as `⊤ ⊑ ⊥`; 11 stack overflows) and times out on 4;
Openllet answers unsupported on 414 (reflexive properties, U in domains and ranges),
throws on 57 (56 "Nominal FunValue … is not found in the KB") and times out on 46.

## Inputs outside OWL 2 DL (13 October 2026, Office-PC, queue jobs)

[docs/design/owl2-dl.md](../../../docs/design/owl2-dl.md) §16: DL-safe rules decided by
grounding with certified models, repairs, monotonicity. The tools: `cargo run -p nrese-dl
--example dl_beyond` (`--survey` the reader's diagnostics and the engines' left-out axioms;
`--decide`, `--classify`, `--justify`), `--example rules_campaign` (random SROIQ(D)
ontologies with random DL-safe rules over the small-scope names, NRESE's answers by
`nrese_dl::scope::answer_beyond`), and `beyond/`:

| File | What |
|---|---|
| `beyond/run.sh` | the campaigns and the ORE runs as one queue job (its results: cases, NRESE's and the references' answers, decisions, taxonomies, justifications) |
| `beyond/compare.py` | NRESE against the references on the Scope runner's codes |
| `beyond/triage.py` | each disagreement against the references' probed deviations from SWRL's semantics |
| `beyond/probes.py` | the minimal probes of those deviations (empty heads, `x ≠ x`, declared individuals, equality-only variables, inverse property atoms) |
| `beyond/abba-run.sh` | one run of the A-B-B-A over the DL and QL cases (a queue chunk) |

**The ORE 2015 corpus** (1,920 ontologies; `dl_beyond --survey`): 22 have reader
diagnostics, every one a DL-safe rule (SWRL); the engines leave nothing else out of any
ontology but datatype definitions in one (which they decide). Of the 22:

| | Consistency | Classification |
|---|---|---|
| 714 (the owner's question) | consistent, a certified model of 5 elements satisfying all 8 rules (Openllet: consistent; HermiT, Konclude: no answer) | exact (the TBox names no individual) |
| 237 and 7 like it (15516, 8280, 5539, 2669, 10906, 5206, 13880) | inconsistent by the DL part alone (Openllet agrees; HermiT rejects a literal typed `rdfs:Literal`); each has a four-axiom justification: `DBNonKeyAttribute(x)`, `DBNonPrimaryKeyAttribute ≡ DBNonKeyAttribute ⊔ …`, `DBNonPrimaryKeyAttribute ⊑ DBKeyAttribute`, `DBKeyAttribute` disjoint from `DBNonKeyAttribute` | exact (inconsistent) |
| 3726, 7278, 15753 (SQWRL queries among the rules) | consistent, certified (the queries assert nothing) | exact |
| 15993, 16630, 2526, 6615, 8250, 12451, 15063, 7345 | consistent, certified (12451: HermiT times out, Openllet overflows its stack) | see `classify.tsv` of the job |
| 13129 | undecided: an ill-typed literal (`"…"^^rdfs:Literal`, *Structural Specification* §5.7) in a rule's data | the DL part's subsumptions |
| 10860 | undecided: SWRLTab's `abox:hasClass` (a built-in over individuals and classes, not SWRL's) | the DL part's subsumptions |

**The rule campaigns** (`rules_campaign`, 1,000 cases each, 20 s per decision; HermiT and
Openllet by `nrese.dl.Scope`):

| Campaign | NRESE decided | agree with every answering reference | disagreements | none answered |
|---|---|---|---|---|
| 1: rules, no data | 988 | 899 | 80 | 9 |
| 2: rules and data | 983 | 906 | 73 | 4 |
| 3: rules, data, built-ins (`swrlb:add`, `lessThan`) | 981 | 916 | 57 | 8 |
| 4: clean rules (no `=`, `≠`, empty heads), data | 983 | 970 | 8 | 5 |
| 5: clean rules, no data | 985 | 972 | 12 | 1 |

Every disagreement is a construct a reference reads against SWRL's semantics, shown by a
probe each (`beyond/probes.py`, both references on each probe; NRESE answers all as the
semantics does): HermiT ignores a rule with an empty head (SWRL §3: it is false); neither
binds a variable only `=` and `≠` mention; Openllet binds no declared individual the
assertions don't mention, no variable only `=` mentions, and no inverse property in an
atom. Three were settled by hand: campaign 1's case 852 (HermiT's disjoint-union error of
the small scope: it entails `⊤ ⊑ B` but not `a : B`), campaign 4's 753 (HermiT misses
`b : A`, which the rule `¬A(x) → B(x)` and `B ⊑ A` entail for the named `b`), and the
hand-checked samples of each deviation. NRESE's undecided cases (80 of 5,000): 66 models
the certificate builder can't fold into a model (counting cases, §10), 14 time budgets.

## Adjudication

No majority votes (§11). A result that differs from a test's type, or a reference
reasoner that differs from another, is settled by hand with a minimised witness and a
proof or a model, and recorded in `disputed.tsv` with the reason. `compare` fails on
any difference not recorded there; rejected tests are reported but never fail a run.
