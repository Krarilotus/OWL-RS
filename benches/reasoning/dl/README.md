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
| `nrese.py` | NRESE's classifiers in the same comparison (`el`: the EL classifier on the EL track, through the runner's `ntriples` conversion and `canonical.py`); the detail column holds the taxonomy's hash, then the classifier's profile: read, normalise, prepare, saturate, assemble and write times in ms, and the counters (concepts, contexts, subsumers, links, conclusions, duplicates, threads) |
| `cargo run -p nrese-owl --example fuzz` | random OWL 2 DL ontologies (`nrese_owl::fuzz`, within the global restrictions) as functional-syntax files with a manifest: differential fuzz campaigns on the references, `ore.py compare` lists disagreements |
| `canonical.py` | the canonical taxonomy from any subsumption closure (NRESE's EL classifier, later `nrese-dl`), with the reference run's signature |
| `disputed.tsv` | differences settled by hand: `test/task<TAB>reasoner<TAB>result<TAB>why` |

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

## Adjudication

No majority votes (§11). A result that differs from a test's type, or a reference
reasoner that differs from another, is settled by hand with a minimised witness and a
proof or a model, and recorded in `disputed.tsv` with the reason. `compare` fails on
any difference not recorded there; rejected tests are reported but never fail a run.
