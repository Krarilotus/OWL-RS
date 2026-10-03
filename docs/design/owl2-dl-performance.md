# OWL 2 DL reasoning: how NRESE leads on performance (3 October 2026)

**What this is.** The performance half of the [DL design](owl2-dl.md):
- the bar, measured on NRESE's machine and taken from the literature;
- where NRESE can lead, and by how much;
- the techniques with measured effects, mapped to NRESE's engines;
- NRESE's own levers through the full stack;
- the method by which every claim is measured;
- the order of phase 3 with a performance gate on each package.

The [DL design](owl2-dl.md) stays the reference for the components themselves. This
document decides what is built first and how fast it has to be.

**Sources.**
- Research reports A, B, D and G (2 October; `output/dl-research-*.md`).
- Primary sources read for this document:
  - Konclude's system description (Steigmiller, Liebig and Glimm, JWS 2014†);
  - its scaling paper (*Query Answering and Scaling Extensions of Konclude*, SemREC 2021);
  - the parallel ABox paper (Steigmiller and Glimm, DL 2020, technical report);
  - *The Incredible ELK* (Kazakov, Krötzsch and Simančík, JAR 2014†);
  - *Parallel Reasoning in Sequoia* (ISWC 2025).
- NRESE's own runs of 3 October (`benches/reasoning/dl/results/`).
- Research report H (the owner's deep-research report of 3 October, `output/dl-research-H-owner-deep-research.md`):
  - cross-domain algorithm engineering from SAT, SMT, CP, MaxSAT and theorem proving;
  - the benchmark metrics;
  - the failure modes.
  - What it adds is in §3 ("What the solver communities add"), §5 and §7. Its claims about CaDiCaL 3.0 (SAT 2026) and the Baobab preprint (2026) haven't been checked against the papers yet.

† marks figures from before 2018: valid for design choices, not as today's state.

## 1. The bar

### Measured on NRESE's machine (main PC, 3 October)

The reference runner of work package 2.3 (`benches/reasoning/dl`) on the ORE 2015
development subset: 40 DL classifications, 20 DL consistency checks and 20 EL
classifications, each ontology at most 5 MB, 300 s per task. Each time is a whole task
(parse, preprocess, reason, serialise) inside a running JVM or process.

| Task | Konclude 0.7 | HermiT 1.4.5 | Openllet 2.6.5 | ELK 0.6.0 | NRESE |
|---|---|---|---|---|---|
| DL classification, sum (solved) | **9.6 s** (39/40) | 148.7 s (40/40) | 26.1 s (38/40) | — | no DL engine yet |
| DL classification, median / p90 | 146 / 567 ms | 171 / 4,985 ms | 126 / 2,766 ms | — | |
| DL consistency, sum | 2.9 s | 5.9 s | **1.9 s** | — | |
| EL classification, sum | 3.7 s | 5.1 s | 9.8 s | 4.06 s | **0.50 s** (after `c0b5e5c`) |
| EL classification, median | 85 ms | 104 ms | 46 ms | 131 ms | **17.5 ms** |

**What the table says:**
- **Konclude** is the bar for expressive classification: its slowest case is under 0.9 s, while HermiT's tail reaches 43 s.
- **NRESE's EL classifier** already beats ELK 0.6.0 on every one of the 20 EL ontologies. One quadratic step had made it slower on the largest; it was fixed on 3 October.
- **Which ELK.** The Whelk paper (Balhoff et al., TGDK 2024) measured ELK 0.6.0, the version in our runner, far slower than ELK 0.4.3 on large ontologies: NCI Thesaurus 23.2 s against 2.4 s, uberon-go-cl-ro 99.7 s against 2.3 s. ELK 0.4.3 joins the reference runner (package 3.0), and every EL claim is made against the faster of the two.
- **The subset is small.** Before any claim, the full EL track, a stratified DL sample of at least 300 ontologies, and the Oxford sample are run (§5).

### From the literature

The records report A collected stand:
- **Konclude on ORE 2015:**
  - 1,862 of 1,920 classifications, 1,911 consistency checks and 591 of 624 realisations, each with a 30-minute limit and 64 GB (Lam et al. 2023; their Konclude failures came from the OWLLink adapter, which is why our runner calls KoncludeCLI directly);
  - in the competition itself (ORE 2015 journal report, Parsia et al. 2017, Table 5, which counts wrong answers apart; the workshop's table counted them as solved): classification 288 solved and 10 wrong, realisation 247 and 14, consistency 303 and 2;
  - 698 of the 703 Oxford ontologies.
- **ELK on SNOMED CT:** 18.6 s with one worker, 4.85 s with eight on a 2011 quad-core: a speed-up of 3.84 (*The Incredible ELK*, Table 11†).
- **Sequoia (consequence-based SROIQ):**
  - 610 of 703 Oxford ontologies under 10 s, falling behind Konclude where equality and numbers explode;
  - its 2025 parallel version gains up to 2.62× with a thread pool on hard ontologies.
- **Konclude on large ABoxes** (DL 2020, 8 cores, 480 GB):
  - LUBM(800) needs 2,692 s to get ready for query answering with one thread, 828 s with four and 439 s with eight (Table 3; corrected 3 October: the 389 s quoted earlier is UOBM(500) at four threads);
  - UOBM(500) needs 1,228 s, 389 s and 245 s;
  - the time is dominated by consistency checking.
- **Konclude's own gaps** (SemREC 2021, Table 1): it times out on classifying OWL2Bench's OWL 2 DL variants at sizes 1 and 10, while the RL, QL and EL variants take 2–20 s. In OWL2Bench's own paper (Singh et al. 2020, §4.1) only Openllet classified the DL variant, at sizes 1 and 2.
- **The Rust field:** OxidOWL (2026), a Rust tableau reasoner, completes 51% of ORE 2015 at 180 s, and about half of its finished realisations are correct; none of the Rust DL reasoners publishes W3C DL suite results.

## 2. Where NRESE can lead, and the targets

Ranked by the size of the opening and the evidence for it.

| # | Opening | Why NRESE can win there | Target (main PC, same protocol as the references) |
|---|---|---|---|
| 1 | **Expressive reasoning over large ABoxes** | Konclude builds model abstractions individual by individual (LUBM(800): 2,692 s). NRESE already materialises LUBM(100) under OWL 2 RL in 6.3 s inside the store. With the bounds of §8 of the design, the DL engine only sees the gap. | Consistency and query answering on LUBM(800), UOBM(500) and OWL2Bench DL at least **10× faster than Konclude** with the same threads, and complete (the status on every answer says so) |
| 2 | **EL and Horn classification** | Already ahead of ELK on the development subset, single-threaded. ELK's concurrency gives it up to 3.8×. | The full ORE EL track and SNOMED-sized ontologies: **≥ 2× faster than ELK** at equal threads, and faster than Konclude |
| 3 | **Incremental reasoning per commit** | No published system or benchmark for OWL 2 DL updates (A: no record exists). NRESE has incremental datalog with provenance, and the store. | A commit touching k axioms costs time proportional to the affected module, not the ontology: **≥ 100× faster than a rebuild** on the incremental track (§11 of the design) |
| 4 | **OWL2Bench DL classification** | Konclude times out (SemREC 2021). | Classified at sizes 1 and 10 within the 30-minute limit |
| 5 | **Expressive classification in general** | Konclude leads; HermiT is 15× slower on the subset. | Phase 3 gate: **at Konclude's level** on the development subsets (sum within 1.5×, no timeout it doesn't have). Phase 4 claim: Konclude's ORE solved counts, with p50 and peak RSS reported (A's release criterion) |
| 6 | **Explanations** | No published bar (A). NRESE's proof IR and justifications already run on RL (2.4). | Time to the first justification and to all of them reported for every entailment of the benchmark sets |

Openings 1, 2 and 3 are where leadership can be large and shown soon. Opening 5 is where it
is hardest: it is reached through 1–3 (most of a real ontology is EL or Horn, report B) and
the hypertableau's engineering (§3).

## 3. The techniques with measured effects

Each technique is listed with its evidence, its place in NRESE and its switch. Every
switch has an on/off metamorphic test (the design's rule three).

### What makes Konclude fast (system description 2014†, SemREC 2021)

| Technique | Evidence | In NRESE |
|---|---|---|
| **Pay-as-you-go:** specialised saturation for the lighter fragments, the tableau only on what isn't handled, and no optimisation switched off globally because nominals occur somewhere | the architecture of the best ORE system; the coupling took Konclude's accumulated time from 44,910 s to 9,589 s (report D†) | dispatch by module and per sub-task (design §4); the context core's results feed the hypertableau |
| **Saturation coupling:** saturation results block tableau nodes early, and supply known and possible subsumers | same | `known_subsumers` / `possible_subsumers` (design §7); context saturation summaries as blocking candidates |
| **Partial absorption** (generalises binary, role and nominal absorption) | report D: FaCT++ on NCI went from 2,447.7 s to 63.9 s with role absorption† | design §3 step 4; extended to partial absorption for more expressive axioms |
| **Precise (un)satisfiability caching** through dependency tracking, and caching of node expansions so that nodes block earlier | the system description; works especially on mostly deterministic ontologies | the hypertableau's cache (design §6), keyed with dependency fingerprints |
| **Reuse of entire completion graphs** (also with nominals) | Wine 49.5 s → 0.8 s; UOBM-1 240.6 s → 1.3 s (report D†) | design §6, cache stage 2 |
| **Pool-based merging** for number restrictions | reduces merge non-determinism | the `≤`-rule's merge order |
| **Known/possible set classification and realisation**, with model merging | one to two orders of magnitude over enhanced traversal on some ontologies (Glimm et al. 2012†) | design §7 |
| **Three levels of parallelism:** ontologies, sub-queries (tests), and alternative branches | 1→4 workers: 1.20× over the whole corpus, 2.94× on FMA (report B†) | classification tests in parallel first; branches only where profiles show long searches |
| **Lock-free caches** read in parallel; **block allocation** per task | the system description | caches as immutable snapshots with epoch publication; per-worker arenas freed per task |
| **The individual derivations cache** (stepwise ABox consistency in parallel) | 8 threads: 5.5× on ChEMBL and 6.8× for parsing (DL 2020) | not needed for the Horn part (the store does it set at a time); kept for the gap's individuals |
| **Absorption-based query answering** with binder concepts | ISWC 2019; Konclude's CQA | an exact service for the gap (design §8), after the bounds |

### What makes ELK fast (*The Incredible ELK*, JAR 2014†)

| Technique | Evidence | In NRESE |
|---|---|---|
| **Contexts** (one per concept) with their own to-do queues; activation by compare-and-set, no locks on the hot path | 3.84× with 8 workers on SNOMED CT, 3.40× on GALEN8 | the context core's worker model (design §5); the EL classifier adopts it first (opening 2) |
| **Rule optimisations** that avoid re-deriving decomposed conjunctions and redundant links | Table 10: measured on the derived axiom counts | in the EL rules and the context core's redundancy elimination |
| **Simple ontologies** (only `A ⊑ B` and `A ⊑ ∃R.B` with no negative existentials) classify from the told hierarchy alone; ELK still derives the unnecessary links | the paper notes that no reasoner exploited this | NRESE's EL path skips link derivation when no negative existential can use it (EMAP-like ontologies) |

### What consequence-based reasoning adds (Sequoia)

- **Indexing, ordered inference and redundancy elimination** are central (Bate et al. 2018).
- **Thread pools beat message passing** for parallel saturation: up to 2.62× (ISWC 2025).
- **Equality and number restrictions are where it explodes:** the design's dynamic fallback and racing (§4) exist for this.

### What the solver communities add (report H)

Report H's thesis: the next gains in expressive reasoning come from SAT/SMT/ATP-style
algorithm engineering around the DL calculus, not from a new calculus. The transferable
mechanisms, and where each lands:

| Mechanism | From | In NRESE | Package |
|---|---|---|---|
| **Conflict-driven search:** nondeterministic choices (disjuncts, merges, `≤`-choices) as *stable decision literals*; a clash's minimised reason set as a learned nogood; activity-based choice order; restarts. Dependency-directed backjumping is the start, not the end. Nogoods must be over canonical decisions, never over completion-graph addresses that merges and blocking invalidate | CDCL (CaDiCaL, Kissat) | the hypertableau's dependency sets become decision-literal sets from 3.3 on; learning is an experiment with an ablation | 3.3 (literals), 3.9 (learning) |
| **Cardinality as a propagator with explanations:** `≤ n R.C` over the current successors as an at-most constraint; native propagation for small ones, a pseudo-Boolean encoding (totaliser, sorting network) or a PB/SAT backend for large ones; explanations feed the conflict analysis | PB solving, Z3's cardinality encodings, CP-SAT | the `≤`-rule's merge search behind a backend interface (`add_at_most`, `assert_equal`, `explain_conflict`, push/pop) | 3.3 (native), 3.6 (PB backend) |
| **Equality with explanations:** union-find with rollback and a reason forest, so that "which decisions made a = b" is answerable | SMT congruence closure | `owl:sameAs` stage C already keeps a union-find; the tableau's merges get the rollback and reason edges | 3.3 |
| **Watch lists:** universals and role-chain steps fire on `(role, concept)` events, not by scanning a node's restrictions after every edge | SAT watched literals | the hypertableau's and the context core's triggers | 3.1, 3.3 |
| **Hash-consing** of concepts, clauses, chain-automaton states and datatype descriptors | ATP term sharing | `nrese-owl`'s interned IR (exists for terms; extended to clauses) | 3.1 |
| **Adaptive label sets:** a sorted small vector, promoted to a dense or compressed bitmap at a density threshold measured on the machine | SAT and database engineering | §4 "subsumer sets as bitsets", made adaptive | 3.1, 3.7 |
| **Budgeted preprocessing:** an expensive simplification runs only when the cheap feature vector predicts it pays (CaDiCaL's inprocessing schedule) | SAT inprocessing | normalisation stages with budgets; the feature vector of design §4 decides | 3.0, 3.4 |
| **Portfolio and algorithm selection:** a cheap structural feature vector (constructor counts, profile islands, ABox size) routes each module to EL saturation, Horn core, hypertableau variant or a race; a wrong prediction costs time, never correctness | MORe, SAT portfolios, Vampire schedules | the dispatcher of design §4 (by module, per sub-task) with the feature vector logged per task | 3.0 (features), 3.6 (racing) |
| **Learned ordering only:** a model may order choices or pick a strategy; it never prunes a branch | NeuroCore, unsat-core guidance | after the deterministic baselines; off by default | later than phase 3 |
| **Proof-producing normalisation:** each normalised clause carries its source axioms and the rule that made it | proof logging in SAT/ATP | the proof IR of 2.4 already records provenance; normalisation steps become proof steps | 3.0 |

### What NRESE does not take

- **GPU offload for the search:** no evidence for a sound and complete GPU SROIQ reasoner (A, B, H). GPU is used only for a data-parallel kernel that a profile names: batched label intersections, large ABox joins, batches of independent entailment tests (H, after HT-HEDL 2024).
- **Majority votes between reasoners:** NRESE adjudicates (2.3).
- **Whole-ontology SAT encodings with a fixed number of anonymous objects:** under the direct semantics, UNSAT then only means "no model of that size". Bounded encodings are allowed only as a subordinate procedure, or where a completeness bound is proved (H).
- **Fine-grained distributed tableau:** distribution is for independent modules, ontologies and classification jobs only (H).

## 4. NRESE's own levers, level by level

Beyond the state of the art: what only a reasoner built into a store, in Rust, with this
project's existing engines can do.

| Level | Lever | First measurable effect |
|---|---|---|
| Method | **Bounds from the store:** the RL closure as L, a TBox-compiled datalog as U1; the DL engine only on the gap's module (design §8) | opening 1: LUBM, UOBM and OWL2Bench without per-individual tableau work |
| Method | **Incrementality from the store's provenance:** support graph sets and the delta executor for L and U1; context invalidation by footprint | opening 3 |
| Plan | **Clause triggers planned like queries:** the store's statistics choose the trigger atom of each absorbed clause and the join order of each hyperresolution program | fewer rule firings on large ABoxes |
| Operators | **Set-at-a-time rule application** over sorted runs for the Horn part (the RL reasoner's semi-naive evaluation), instead of node-at-a-time | opening 1 |
| Operators | **Worst-case-optimal joins** for cyclic clause bodies (already in the query engine) | role chains, cyclic DL-safe rules |
| Layout | **Dense ids end to end** (store `TermId` → `nrese-owl` term → clause atom), with no string or OWL API object in a hot loop | parse and preprocess phases: Konclude's 2014 paper names the OWL API's overhead |
| Layout | **Subsumer sets as bitsets** where a context's subsumers are dense in a compacted concept order (roaring-style containers otherwise); **subset tests and intersections in SIMD** | classification's set operations (known/possible subsumers, blocking checks) |
| Layout | **64-byte hot nodes, side arenas for cold data, a trail for undo** (design §6) | tableau memory per node, reported apart (two measures, D) |
| Machine | **Work stealing (rayon)** for contexts and classification tests; **per-worker arenas** freed per task (Konclude's block allocation); **lock-free activation** (ELK) | the concurrency curve, measured 1→16 threads |
| Machine | **Profile-guided build** (PGO) once a corpus represents the load; `target-cpu` dispatch where AVX2 or AVX-512 pays | the whole-task times |

## 5. The method: every number measured, nothing claimed

**The DL lab** (`benches/reasoning/dl`, built in 2.3) is the perf lab of phase 3:
- **The same tasks in the same runner:** the reference reasoners in one JVM per batch, and NRESE's engines through `nrese.py` (or a native runner once `nrese-dl` exists).
- **Per-phase times** from NRESE: parse, normalise, saturate, search, classify, serialise.
- **Counters** (design §11): contexts, clauses, rule firings, branches, backjumps, cache hits, peak agenda.
- **Peak RSS** per task.

**The corpora, in tiers:**

| Tier | Contents | Where | When |
|---|---|---|---|
| dev | the ORE 2015 development subsets (80 tasks today; grow to 300+ stratified by track, size and expressivity, ORE's `metadata.csv` giving the strata), the W3C suite, OWL2Bench EL/QL/RL/DL at 1 and 10, LUBM/UOBM | main PC | every work package |
| full | ORE 2015 entire (1,920 × 3 tasks), the Oxford corpus | Draco cluster | milestones |
| large | SNOMED CT (with a licence), the 21 large NCBO BioPortal ontologies of Lam et al. 2023 (most of the six reasoners they evaluated solved fewer than half the tasks), the Gene Ontology, LUBM(800), UOBM(500), ChEMBL, Reactome, UniProt | main PC (64 GB), Draco | phase 3 end and phase 4 |

**Reference reasoners to add:** Whelk (EL+RL, Balhoff et al., TGDK 2024), the newest EL
and RL rival, compared against ELK in its own paper. It joins the runner if its licence
permits.

**The protocol** (report A, completed with H; the general rules are [benches/PROTOCOL.md](../../benches/PROTOCOL.md)):
- **Limits and threads:** a 30-minute limit and 64 GB; results at 1 thread and at all cores.
- **Process:** a fresh process per task for cold numbers.
- **Repetitions:** 10 for the claim runs.
- **Result classes kept apart:** solved, timeout, out of memory, unsupported, wrong.
- **Never only means over solved tasks:**
  - solved counts;
  - median, p90, p95 and p99;
  - a PAR-2 score (a timeout counts twice the limit);
  - cactus plots;
  - peak RSS and bytes per input axiom.
- **Robustness:** a rerun with the axioms in shuffled order (`ore.py manifest --shuffle-seed`, to add). An engine whose time depends on the axiom order is reported with its spread.
- **Blind holdout:** a fixed, seeded 20% of every stratified corpus is never looked at while tuning. Claims are made on the holdout.
- **Ablations:** every optimisation is shown as a chain on one build, with the same hardware, input order and limits. Example: baseline → +adaptive labels → +watch lists → +backjumping → +learning → +parallel. Each link must be an on/off switch.
- **Counters per run:** next to the per-phase times, the counters of design §11 plus H's list:
  - decisions, conflicts, backjumps, learned nogoods;
  - generated nodes, merges, blocking tests and hits;
  - cache queries and hits;
  - rule firings and duplicate derivations;
  - peak nodes and edges, bytes per assertion;
  - steals.

**Regression baselines:**
- **Committed:** NRESE's per-ontology times, as JSON under `benches/baselines/dl/`. A package that slows a tier's sum by more than 5% (beyond noise) explains why, or doesn't merge.
- **Publishing:** HermiT (LGPL-3.0), Konclude 0.7 (LGPL-3.0), ELK (Apache-2.0) and Openllet (AGPL-3.0) results may be published; the commercial systems' may not (`benches/competitors/README.md`).

**Profiling before optimising** (the project's rule): sampling profiles (Windows ETW on the
main PC, `perf` on the office PC) for every tier whose sum regresses or misses its gate;
the counters above say which rule or phase, the profile says which code.

## 6. Phase 3 in the order that leads soonest

The [design](owl2-dl.md) §14 lists phase 3 as hypertableau → datatypes → classification →
context core Horn → SRIQ/SROIQ stages → caches → black-box justifications. Since 2.3,
HermiT, Konclude, Openllet and ELK run as oracles on every corpus. The engine order no
longer has to put the hypertableau first to have something to check against.
**Proposed order** (decided 3 October unless the owner objects):

| Package | Delivers | Correctness gate | Performance gate |
|---|---|---|---|
| 3.0 | **The DL lab completed:** a stratified ORE development set of at least 300 tasks; OWL2Bench DL; UOBM; the reference results on all of them; per-phase timing and counters in NRESE; baselines committed | the references agree, disagreements adjudicated | the references' sums recorded as the bars |
| 3.1 | **The context core, Horn stage**, parallel from the start (contexts with lock-free activation and work stealing), on `nrese-owl`'s clauses; the EL classifier becomes this core's EL mode | taxonomies equal to ELK's and Konclude's on the EL and Horn strata; metamorphic tests (2.5); proofs into the proof IR | EL track: ≥ 2× faster than ELK at equal threads; Horn strata: faster than Konclude |
| 3.2 | **Bounds for large ABoxes:** U1 compiled from the clauses and maintained per commit; the gap computed; consistency through L, U1 and the Horn core | the incremental track against cold rebuilds; answers equal to the references' on LUBM/UOBM where they finish | LUBM(800) and UOBM(500): ≥ 10× faster than Konclude's published preparation times, then on the same machine |
| 3.3 | **The hypertableau, ALC → SROIQ,** with absorption, lazy unfolding, semantic branching, backjumping, anywhere blocking and (un)satisfiability caching from the start | the W3C DL suite green without datatypes; no disagreement on the development set that isn't adjudicated | DL classification on the development set within 3× of Konclude |
| 3.4 | **Classification and realisation driver:** known/possible subsumers from the core and the tableau, parallel tests, model merging | taxonomies equal to the references' | within 1.5× of Konclude's sum; no timeout Konclude doesn't have |
| 3.5 | **The datatype theory** | the W3C DL suite green | datatype-heavy strata reported apart |
| 3.6 | **The context core, SRIQ and SROIQ stages;** dynamic fallback and racing | the design's §5 gates | Oxford sample: ≥ Konclude's solved count; equality-heavy strata no slower than the hypertableau alone |
| 3.7 | **Caches with footprints, completion-graph reuse, saturation coupling;** profiled SIMD | on/off metamorphic tests | Konclude's level or better on the full ORE run (Draco): solved counts, mean, p50, peak RSS |
| 3.8 | **Black-box justifications** over the hypertableau | equal to the glass-box results where both apply | time to the first and all justifications reported |
| 3.9 | **Conflict-driven hypertableau (research):** learned nogoods over stable decision literals (disjuncts, merges, `≤`-choices), activity-based choice order, restarts, nogood database management; a proof that learning keeps soundness and completeness | no answer changes with learning on or off (metamorphic); the proof in the design | an ablation against plain backjumping on the disjunction- and cardinality-heavy strata: a statistically robust gain on the holdout, or the switch stays off |

Report H's cheap, publishable steps sit inside these packages, each with its own ablation:
- adaptive labels (3.1);
- compiled watch lists (3.1, 3.3);
- proof-producing normalisation (3.0);
- parallel taxonomy building (3.4);
- PB-assisted number restrictions (3.6);
- incremental module maintenance (3.2).

Conflict learning (3.9) is the strongest research contribution of the DL half of the
paper.

The order moves the context core and the bounds first:
- **Where the openings are:** they carry openings 1–3, where NRESE can lead by a large factor.
- **What carries the load:** they handle most of real ontologies (78.6% of the Oxford corpus is Horn, report B).
- **The anchor is still required:** the hypertableau remains the completeness anchor for SROIQ(D). No claim of complete OWL 2 DL reasoning is made before 3.3–3.5 pass their gates.

## 7. Risks to the performance claims

- **The subset isn't the corpus.** Every gate is checked again on the full run before a claim.
- **Same-machine comparisons only.** The published means come from other CPUs: on this machine they serve only as orientation, never as a claim (A).
- **Large ABoxes and certain answers.** The 10× target of opening 1 counts only answers whose status is `complete`. If the gap needs exact services, their time is included.
- **Concurrency gains are sub-linear.** ELK: 3.8× on 8 threads. Konclude: 1.2× over its whole corpus. The targets hold at 1 thread too, reported apart.
- **Overfitting to the development set.** Claims are made on the blind holdout and the full corpus (§5).
- **Optimisations that change answers** (H's failure modes). Each has a metamorphic test or a dedicated check:
  - closed-world or unique-name leakage;
  - role-chain rewrites that break the regularity and simple-role conditions;
  - blocking that is too weak (blow-up) or too eager (lost models);
  - caches reused across branches without their dependencies;
  - an equality merge whose effects on counts, edges and blocking aren't propagated as events.

## 8. Lab log: measured changes, kept and rejected

Each entry gives the change, the A/B, and the decision. Setup unless stated otherwise:
- **Corpus:** the EL development stratum (50 ORE 2015 ontologies, `tmp/ore-strata`,
  split `dev`).
- **Measure:** the `classify` example's profile, saturate time, median of ABBA-interleaved
  rounds, summed over the ontologies.
- **Check:** outputs byte-identical between the builds.

| Date | Change | Result | Decision |
|---|---|---|---|
| 3 Oct 2026 | **Don't queue known subsumers** (ELK's check before a conclusion is produced), after the profile showed 56% of conclusions repeating known ones | 1 thread: conclusions −34%, saturation **+9%** (5 rounds; the lookup before each push costs more than the push and pop it saves). 16 threads, own context only: −1.3% (7 rounds), inside the noise (identical code differed by 4% at 1 thread) | rejected. The cost is in the hash sets: the lever is their representation (subsumer bitsets over a compacted concept order, §4), not fewer queue entries |
