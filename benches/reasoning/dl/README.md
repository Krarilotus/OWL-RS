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
| `nrese.py` | NRESE's classifiers in the same comparison (`el`: the EL classifier on the EL track, through the runner's `ntriples` conversion and `canonical.py`; `dl`: the DL driver, `dl_classify`, on the DL tracks' classification and realisation tasks, `--only` for a few); the detail column holds the hash, then the classifier's profile: its phase times in ms and its counters (EL: concepts, contexts, subsumers, links, conclusions; DL: tests, candidates, branch points, clashes, the Horn bound's share), or for an incomplete DL run the reasons it printed; a DL row's status is the driver's own outcome (`inconsistent`, `unsupported`, `timeout`, `undecided`), as the references' rows give theirs; `consistency`: the hypertableau (`tableau_consistency`) on the DL tracks' consistency tasks, an input it refuses as outside OWL 2 DL decided by `nrese_dl::beyond` (`dl_beyond --decide`) in the rest of the time, the detail naming the path |
| `cargo run -p nrese-owl --example fuzz` | random OWL 2 DL ontologies (`nrese_owl::fuzz`, within the global restrictions) as functional-syntax files with a manifest: differential fuzz campaigns on the references, `ore.py compare` lists disagreements |
| `canonical.py` | the canonical taxonomy from any subsumption closure (NRESE's EL classifier, later `nrese-dl`), with the reference run's signature |
| `private/disputed.tsv` | differences settled by hand, kept locally and not in the repository (`private/` is git-ignored): `test/task<TAB>reasoner<TAB>result<TAB>why`; `w3c.py compare` reads it and stops without it |
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

- **W3C, direct semantics, species DL** (411 tasks of 311 tests; 20 s per task, and a
  rerun at 120 s of the tasks the 2009 results list as passes that ran out of time;
  the per-task results are kept locally in `results/`, which is git-ignored).
  - **HermiT** was run on every task; its result on each is in the results file.
  - **Openllet** was run on every task; its result on each is in the results file.
  - **Konclude 0.7.0-1138** was run on every task; its result on each is in the results
    file.
  - **ELK:** 93 pass of the 114 EL-profile tasks; the rest use constructs outside the
    fragment it supports (keys, `Self`, negative assertions, datatypes).
  - Results that differ from a test's type are settled by hand as under Adjudication
    below.
- **ORE 2015 development subset** (80 tasks: 40 DL and 20 EL classifications, 20 DL
  consistency checks up to 5 MB; 300 s per task; the per-task results kept locally in `results/`).
  - NRESE's EL classifier gives the same taxonomy as the references on all 20 EL
    ontologies, and solves 20 of 20.
  - HermiT, Konclude, Openllet and ELK (on the EL ones) were run on every task; their
    results are in the results file.
  - The comparison found a gap in NRESE's EL output: classes equivalent to `owl:Thing`
    were not reported as such. Fixed (`Classification::top`).
- **A differential fuzz campaign** (60 random SROIQ(D) ontologies from
  `cargo run -p nrese-owl --example fuzz`), run on HermiT, Konclude and Openllet as
  reference tools.

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
| scope `4,4,4` (every ontology of 1 axiom up to size 4, of 2 and 3 up to total size 4) | 2,761,335 | 2,760,771 | 421 | 139 | 4 (2 with every claim certified) |
| scope `3,3` (the gate's) | 35,505 | 35,454 | 3 | 48 | 0 |
| covering array of triples (`cover:3`, 3 to 6 axioms of up to size 4; every one of the 47,905 triples of the 67 features) | 3,354 | 3,312 | 3 | 38 | 0; 1 NRESE gave up at the campaign's 20 s |

**Every case with differing answers is settled by hand from the semantics with a minimised
witness (Adjudication below); the gate's 51 are in `scope-adjudicated.tsv`, one line each.**

**What the campaign found in NRESE** (each fixed with a guard, 12 October): operands of a
disjointness the U rewriting made equal were dropped (cases 623, 677, 732, 786: answered
consistent, inconsistent); keys over a transitive property read asserted edges only
(found by the review the campaign prompted); the context core took keys to need role
assertions, and the classifier called an ontology with a key over U alone consistent (1219488); realisation took an individual no
assertion mentions for a fresh element, past a key (1267640).

**References run** (scope `4,4,4`): HermiT and Openllet, each with the campaign's time
limit; the cases neither answered are the table's last column.

## Inputs outside OWL 2 DL (13 October 2026, queue jobs)

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
| `beyond/probes.py` | minimal probes of how a reasoner reads SWRL's corner cases (empty heads, `x ≠ x`, declared individuals, equality-only variables, inverse property atoms) |
| `beyond/abba-run.sh` | one run of the A-B-B-A over the DL and QL cases (a queue chunk) |

**The ORE 2015 corpus** (1,920 ontologies; `dl_beyond --survey`): 22 have reader
diagnostics, every one a DL-safe rule (SWRL); the engines leave nothing else out of any
ontology but datatype definitions in one (which they decide). Of the 22:

| | Consistency | Classification |
|---|---|---|
| 714 (consistency under 8 DL-safe rules) | consistent, a certified model of 5 elements satisfying all 8 rules (the references were run on it) | exact (the TBox names no individual) |
| 237 and 7 like it (15516, 8280, 5539, 2669, 10906, 5206, 13880) | inconsistent by the DL part alone (the references were run on them); each has a four-axiom justification: `DBNonKeyAttribute(x)`, `DBNonPrimaryKeyAttribute ≡ DBNonKeyAttribute ⊔ …`, `DBNonPrimaryKeyAttribute ⊑ DBKeyAttribute`, `DBKeyAttribute` disjoint from `DBNonKeyAttribute` | exact (inconsistent) |
| 3726, 7278, 15753 (SQWRL queries among the rules) | consistent, certified (the queries assert nothing) | exact |
| 15993, 16630, 2526, 6615, 8250, 15063, 7345 | consistent, certified | exact (by the disjoint union, or refined by certified models where the TBox names individuals) |
| 12451 | consistent, certified | sound, not exact: 27,223 subsumptions; the refinement's 25 minutes leave most of its 2,000 classes' pairs open |
| 10860 | inconsistent: the DL part with 7 instances of its SWRL rules; one rule uses SWRLTab's `abox:hasClass` (over individuals and classes, not a SWRL built-in) and is left out, named | exact (inconsistent) |
| 13129 | undecided: an ill-typed literal (`"…"^^rdfs:Literal`, *Structural Specification* §5.7) in its rules' data | the DL part's subsumptions, sound |

10860's justification (10 axioms, 2 rules): a situation with exactly zero
`hasR2RRelation`s, whose requester's working department and patient's supervision clinic
are either different or the same individual; one rule gives the situation such a relation
in each case. A model must choose a case, which is why it took grounding.

So of the 22: consistency decided for 21 (12 consistent, 9 inconsistent), classification
exact for 20. Jobs `dl-beyond-runs-1` to `-3` (release binaries of
`v15/dl-beyond`).

**The rule campaigns** (`rules_campaign`, 1,000 cases each, 20 s per decision; HermiT and
Openllet by `nrese.dl.Scope`):

| Campaign | NRESE decided | agree with every answering reference | disagreements | none answered |
|---|---|---|---|---|
| 1: rules, no data | 988 | 899 | 80 | 9 |
| 2: rules and data | 983 | 906 | 73 | 4 |
| 3: rules, data, built-ins (`swrlb:add`, `lessThan`) | 981 | 916 | 57 | 8 |
| 4: clean rules (no `=`, `≠`, empty heads), data | 983 | 970 | 8 | 5 |
| 5: clean rules, no data | 985 | 972 | 12 | 1 |

Every disagreement with a reference was checked against SWRL's semantics, with a minimal
probe for each kind (`beyond/probes.py`) and samples checked by hand; NRESE answers every
probe as the semantics does. NRESE's undecided cases (80 of 5,000): 66 models
the certificate builder can't fold into a model (counting cases, §10), 14 time budgets.

## Adjudication

No majority votes (§11). A result that differs from a test's type, or a reference
reasoner that differs from another, is settled by hand with a minimised witness and a
proof or a model, and recorded in `private/disputed.tsv` (kept locally) with the reason. `compare` fails on
any difference not recorded there; rejected tests are reported but never fail a run.
