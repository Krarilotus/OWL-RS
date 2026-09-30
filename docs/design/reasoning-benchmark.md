# Reasoning benchmark (Pf0-R): the headline evidence

Status: **plan**, 2026-09-26; §7 to §9 (a real integration workload, cluster runs, the evaluation for a paper) added 2026-09-30.

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
| **RG × GS × GND integration** (HisQu; §7) | Real: three sources linked by `owl:sameAs`, aligned on the GND ontology, ten competency questions | example (21 k statements), cohort, full (83 k GS persons and the complete GND person and place files) | Identity reasoning over real links, where the closure and the answers grow with the identity groups. The project needed a large heap on Fuseki for it |

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

## 7. The integration workload (RG × GS × GND)

Added on 30 September 2026. Kit and instructions: [benches/integration](../../benches/integration/README.md).

**What it is.** The HisQu project links persons of the Repertorium Germanicum, the Germania Sacra person database and the GND by `owl:sameAs`, aligns the three vocabularies on the GND ontology, and asks ten competency questions (persons over time and place, name variants, regesta evidence, co-mention networks, identity groups). Its repository has no licence and its RG source is private, so the kit reads both from a checkout and copies nothing.

**Why it belongs here.** It exercises what the synthetic benchmarks don't:
- equality reasoning over real identity groups (up to 24 identities of one person on the example data), where the OWL 2 RL rules copy every statement to every identity;
- a large real ontology whose domains and ranges are unnamed `owl:unionOf` classes;
- analytical questions with property paths over `owl:sameAs`, several OPTIONALs and aggregates, written by their users and not by a benchmark's authors.

**Tasks.** RT1 (materialisation: time, inferred count, peak memory) and RT3 (the questions: answers, then latency). RT2 and RT5 follow when the matching pipeline writes its links into a running store.

**Entailment regimes.** The answers depend on how much of `owl:sameAs` is derived: everything (OWL 2 RL), the identity closure only (the project's lightweight rules), or nothing. Systems are compared within one regime, and on the row counts of their answers before any time.

**First measurement.** Example tier: 21,261 statements with the project's alignment axioms. One run on a shared workstation with 16 cores and 32 GB, so these are development figures and not results to publish. Both systems through the same client, full results fetched as JSON, 1 warm-up and 3 measured runs, p50:

| | NRESE, `owl2-rl` | Fuseki 6.0.0, Jena's OWL rule reasoner as the project configures it (8 GB heap) | NRESE before the day's fixes |
|---|---|---|---|
| Load | 0.09 s, reasoned and durable | 2.1 s, not yet reasoned | |
| Reasoning | 0.04 s (within the load) | 106 s (at the first query) | |
| Statements answered | 65,928 | 73,639 (Jena's rule set derives more than OWL 2 RL) | |
| Peak memory | 154 MiB loading; 6.1 GB serving, from CQ05's attempt | 1.7 GB | |
| CQ02 (2,430 rows) | 18 ms | 4,897 ms | 23 ms |
| CQ03 (90) | 0.8 ms | 18.5 ms | 3.1 ms |
| CQ04 (19) | 1.2 ms | 11.9 ms | 3.8 ms |
| CQ05 (9,009) | 0.36 s in the process, after the fixes below (not yet over HTTP) | no answer in 120 s | stopped at the 4 GiB query budget |
| CQ06 (648) | 2.9 ms | 148 ms | 11 s |
| CQ07 (576) | 4.1 ms | 75 ms | 236 ms |
| CQ08 (159,040) | 1,068 ms | 4,959 ms | out of memory |
| CQ09 (13) | 1.3 ms | 22 ms | 280 ms |
| CQ10 (4) | 0.5 ms | 18.6 ms | no rows (a wrong answer) |

The eight questions both answer have the same row counts. CQ01 is an empty file. Fuseki kept working on the abandoned CQ05 while the later questions ran, so its later times are upper bounds. Without reasoning (`nrese-plain`) the nine questions take 6 ms together, and answer less. The last column is where NRESE started: CQ02 to CQ04 from the first run on this machine, the others measured in the process on the development PC.

**What it showed about NRESE** (work packages W1 to W7 in the [plan](../plan/2026-09-30-graphdb-parity-plan.md), §5b). Fixed the same day:
- `HAVING` on a `SELECT` alias returned no rows (CQ10).
- A group's filters ran after all its joins, also inside a basic graph pattern (CQ06, CQ07, CQ08, CQ09).
- A property path with two variable ends was computed for every node of the graph before it was joined (CQ03, CQ04, CQ10).
- The server didn't set `TCP_NODELAY`: a result sent in several pieces waited 40 ms for the client's delayed acknowledgement, whatever the query took.

Fixed later that day:
- CQ05 is a product of about 10¹⁰ rows under full equality, with several OPTIONALs that only feed `COUNT(DISTINCT …)` and `SAMPLE`. `SELECT DISTINCT` and groups whose aggregates ignore duplicates now work on sets, and such an OPTIONAL is joined to the groups after they are formed (W5): 9,009 rows in 0.36 s.
- The query memory budget is a bound: operators charge their working memory, and a budget for all queries together caps the server (1 GiB budget, 815 MiB peak, measured on Linux).

Open:
- CQ08's second is mostly its 159,040 rows written as JSON and read by the client; evaluating them takes 0.3 s.
- With the GND ontology added, 55 % of the inferred statements are memberships in unnamed union classes (W7).

**Still to measure:** the cohort tier (being built with the project's pipeline) and the full tier, the other systems, and the GND ontology variant on Fuseki.

## 8. Runs on a cluster

Instructions: [benches/cluster](../../benches/cluster/README.md). The numbers in a paper come from there, not from workstations.

- **Machine:** Draco at the University of Jena (SLURM; standard nodes with 48 cores and 256 GB, five nodes with 2.3 to 4 TB). The large-memory nodes are what lets the in-memory reasoners run on the full tier at all, so that "needs N GB" is a measured result and not a failure to run.
- **One system per job on a whole node** (`--exclusive`), so no other job shares cores, memory or memory bandwidth. The job records the node, CPU, kernel, toolchain and the commits of NRESE and of the workload.
- **No Docker on the cluster.** NRESE, Fuseki and the client run as processes. The other systems run from their published images through Apptainer; those adapters are still to be written.
- **Repetition:** three jobs per system and tier on the same node type; the median and the spread are reported.
- **State:** the job script ran on a Linux workstation without SLURM. There is no cluster account yet.

## 9. The evaluation for a paper

What the paper claims, and the experiment that supports each claim. Every experiment names its correctness check: a time is reported only for a result that was checked.

| Claim | Experiment | Data | Compared with | Check |
|---|---|---|---|---|
| Materialisation is fast and compact | RT1: load and reason; time, inferred statements, peak memory, bytes per statement, thread scaling | LUBM 1 to 1000, OWL2Bench per profile, DBpedia and YAGO with their ontologies, the integration workload | Jena, Nemo (published freely); GraphDB, RDFox (with permission) | Inferred set equal to the oracle's (owlrl where it finishes, else Nemo) |
| Reasoning stays on while data changes | RT2 and RT5: single-statement and batch commits with inference and the consistency gate on; latency p50/p99, with concurrent readers | LUBM 10 to 1000; the integration workload's links written one by one | GraphDB, RDFox; Jena (rematerialises) | The state after the changes equals rematerialisation |
| Queries under entailment are answered fast and completely | RT3: the query sets after materialisation; latency, answers, throughput with 8 clients | LUBM (14 queries), OWL2Bench (22), the integration workload (10) | As RT1, and Stardog, Virtuoso (query-time reasoning) | Answers against published or oracle answers; row counts across systems |
| A real integration workload runs on a workstation | RT1 and RT3 on the tiers of §7; the memory each system needs to finish | The integration workload | Fuseki as its project runs it; GraphDB, RDFox | Row counts across systems within one entailment regime |
| Inconsistencies are found and explained | RT4 and RT7: injected contradictions; detection time, explanation size | LUBM and OWL2Bench with injected contradictions; the project's own ontologies | GraphDB, RDFox, Stardog | Every injected contradiction found, none on clean data; each proof step is a rule application |
| The basics are not what it costs | The [basics scorecard](../../benches/competitors/SCORECARD.md): load, store size, restart, query mix, throughput, writes under read load | olympics, YAGO tiny, DBpedia core, Wikidata lexemes | QLever, Oxigraph, Virtuoso, Jena TDB2 (none of them reasons) | Row counts across systems |

**Rules for every experiment:**
- Versions pinned, each vendor's own loader and tuning guidance, configurations published with the results.
- The same input file, the same client, the same timeout for all systems. A timeout or an out-of-memory is reported as that, with the limit.
- One warm-up and at least five measured runs per query; p50 and the range. Three jobs per configuration.
- A disagreement between systems is investigated and reported (a bug, reported upstream, or a documented semantic difference), never averaged away.
- Licensed systems appear only with the vendor's written permission. If a vendor refuses, the paper says so and reports the open systems.

**What the paper needs that doesn't exist yet:**
1. A cluster account, and the adapters for the other systems under Apptainer.
2. The cohort and full tiers of the integration workload, and its authors' agreement to be named.
3. Licence files for GraphDB, RDFox, Stardog and AnzoGraph, and the vendors' answers on publishing.
4. The engine work the first measurement asks for (W3 to W6), so that the larger tiers are answered and not only loaded.
5. RT6 (classification) stays out of the paper unless R8 is built.
