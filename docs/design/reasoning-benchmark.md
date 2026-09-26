# Reasoning benchmark (Pf0-R): the headline evidence

Status: **plan**, 2026-09-26.

Ontology reasoning is what NRESE is built to be best at. Load, storage and plain SPARQL speed are the basics; the [basics scorecard](../../benches/competitors/SCORECARD.md) guards them against regressions. This benchmark measures the claims of [reasoner-v2.md](reasoner-v2.md) (targets T1–T9) against the systems that reason today, and it tracks NRESE through M3.

## 1. Tasks

| # | Task | What is measured | Correctness check |
|---|---|---|---|
| **RT1** | **Materialisation.** Compute all inferences of instance data under a ruleset: `rdfs`, `rdfs-plus`, `owl-horst`, `owl2-rl` | Load plus materialise time; inferred-triple count; peak memory; bytes per stored fact; thread scaling | The inferred **set** (not just its count) diffed against an oracle (§4) |
| **RT2** | **Incremental maintenance.** After RT1, insert and delete ABox batches (1, 100, 10 k triples) and TBox axioms | Latency per batch against full rematerialisation | Incremental result = rematerialised result |
| **RT3** | **Query answering under entailment.** The benchmark query sets, after materialisation (or by rewriting, for systems that reason at query time) | Latency per query, completeness (answers against the reference answers), throughput with 8 clients | Answer sets against published or oracle answers |
| **RT4** | **Consistency and explanations.** Data with injected contradictions: disjoint classes, functional/inverse-functional properties, `owl:Nothing`, `differentFrom`, irreflexive/asymmetric properties | Detection time; whether an explanation is produced, and its size | Every injected contradiction found, and no false positives on clean data |
| **RT5** | **Commit-path reasoning.** Interactive editing on a reasoned store: 1-triple writes with inference and the consistency gate on | Write latency p50/p99 at 1 M / 10 M / 100 M, with and without concurrent readers | The committed state equals rematerialisation |
| **RT6** | **Classification.** TBox subsumption hierarchy (the OWL 2 EL and DL tasks) | Classification time; hierarchy size | Hierarchy against HermiT/Konclude; against ELK for EL |
| **RT7** | **Explanations on demand.** Proof of one inferred fact, or one violation | Latency; proof depth and size | The proof checks out: every step is a rule application over existing facts |

## 2. Datasets

| Dataset | Kind | Sizes | Why |
|---|---|---|---|
| **LUBM** (UBA generator, univ-bench ontology, 14 queries) | Synthetic, OWL Lite-ish | 1 / 10 / 100 / 1000 universities (≈ 0.1 / 1.3 / 13 / 130 M triples) | The standard materialisation benchmark; GraphDB and RDFox publish numbers on it |
| **OWL2Bench** (ISWC 2020; extends UOBM) | Synthetic, one TBox per OWL 2 profile (EL, QL, RL, DL), 22 queries | 1…N universities per profile | Tests language coverage per profile, not just scale |
| **DBpedia 2022-12 core + DBpedia ontology** | Real | 67 M facts, ~800 classes | Real RDFS/RL materialisation over a real class and property hierarchy |
| **YAGO 4.5 (tiny, later full) with its taxonomy** | Real | 23 M facts, deep `subClassOf` hierarchy (schema.org plus Wikidata classes) | Deep hierarchies stress transitive closure and the hierarchy module (R3) |
| **Wikidata classes** (P31/P279 slice) | Real | server-scale | The largest real class hierarchy; needs the big machine |
| **ORE 2015 corpus** (1,920 ontologies; EL and DL tracks) | Real ontologies | subsets by size | Classification and consistency across what people actually build (BioPortal, the Oxford library, web crawl) |
| **Large biomedical TBoxes** (GO, ChEBI, UBERON, NCIt) | Real | 10⁴–10⁵ classes | EL classification at scale (the ELK use case); SNOMED CT only if a licence is available |
| **Our own ontologies** (the RG ontology, DMW models) | Real | small, rich | The editing workflows NRESE serves; used for RT4, RT5 and RT7 |

## 3. Systems

| System | Reasoning model | Tasks | Licence and status |
|---|---|---|---|
| **NRESE v1** (`rules-mvp`) | Bounded RDFS and OWL rules; commit gate only, inferences not queryable | RT4, RT5 (partial) | Today's baseline, honest about its limits |
| **NRESE v2** | Materialised, incremental, per [reasoner-v2.md](reasoner-v2.md) | all | Arrives through M3 (R1–R6) |
| **GraphDB 11.5** | Forward chaining with rulesets; `isSupported` retraction | RT1–RT5, RT7 | Needs a licence file (Free); results need Ontotext's permission to publish |
| **RDFox 7.6** | Parallel datalog materialisation; B/F/FBF maintenance | RT1–RT5, RT7 | Evaluation licence requested; results need OST's approval |
| **AnzoGraph 3.5** (Altair Graph Lakehouse) | RDFS-plus and an OWL 2 RL subset | RT1, RT3 | Free up to 8 GB RAM; results need CSI's consent |
| **Stardog** | Query-time reasoning (RL/QL/EL/SL) | RT3, RT4 | Free developer licence; licence terms still to be checked |
| **Apache Jena** | Rule reasoners (RDFS, OWL micro/mini/full) in memory | RT1, RT3 (small) | Apache 2.0, publishable |
| **Virtuoso 7** | Query-time subclass, subproperty and sameAs inference only | RT3 (partial) | GPL, publishable |
| **Nemo** (Rust datalog), **VLog** | Datalog materialisation (rulesets translated) | RT1 | Open source, publishable; academic state of the art |
| **owlrl** (Python) | Reference OWL 2 RL implementation | RT1 oracle (small only) | Open source |
| **ELK, HermiT, Konclude, Openllet** | DL/EL classification | RT6, RT4 (TBox) | Open source; ELK and HermiT via ROBOT (the `obolibrary/odkfull` image) |

## 4. Correctness oracles

- **OWL 2 RL / RDFS materialisation:** the inferred set from RDFox or GraphDB once licensed. Before that:
  - owlrl, on small inputs
  - agreement between Jena's rule reasoner and Nemo on the rules they share
- **Queries:** the published LUBM reference answers for LUBM(1). Beyond that, agreement across the systems that claim completeness for the profile.
- **Classification:** HermiT (DL) and ELK (EL) hierarchies.
- **Disagreements are investigated, never averaged away.** Each difference is either a bug (ours or theirs, reported upstream) or a documented semantic difference, as in reasoner-v2 §6.3.

## 5. Harness

- **Location:** `benches/reasoning/`, alongside the basics kit.
  - `prepare-*.sh` runs the official generators (LUBM UBA, OWL2Bench, both Java, in Docker) and fetches ontologies.
  - One adapter per system and task: materialise, maintain, query, check, classify.
  - `reasoning-scorecard.sh` runs everything and writes to the git-ignored `results/`.
  - `REASONING-SCORECARD.md` holds the publishable systems' results.
- **Same fairness rules as the basics kit.** Each vendor's own loader and reasoner settings, the same input, the same host, repeated runs on the reference machine. The licence rules in `benches/competitors/README.md` apply unchanged.

## 6. Staging

1. **Now:**
   - datasets and generators: LUBM, OWL2Bench, the DBpedia ontology, the YAGO taxonomy, an ORE subset, large biomedical TBoxes
   - the oracles: owlrl, HermiT/ELK via ROBOT
   - the publishable baselines: Jena, Nemo, Virtuoso
   - AnzoGraph, locally
   - NRESE v1 on RT4/RT5
2. **With licences:** GraphDB, RDFox and Stardog, locally, published only with permission.
3. **Through M3:**
   - NRESE v2 enters task by task: R2 → RT1, R4 → RT3, R5 → RT2/RT5, R6 → RT4/RT7, R8 → RT6
   - every M3 work package's "done when" cites this benchmark
