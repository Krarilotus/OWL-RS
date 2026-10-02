# From here to two papers: the order of work (2 October 2026)

The owner's direction of 2 October, for the time after the A+B milestone:

> The engine core, RL reasoning, SHACL, the protocols and the engineering basics are in
> place; the query plan is half migrated; DL reasoning is designed but not started.
>
> 1. Finish and prove what we have: complete the query-plan migration and the cleanup,
>    then measure the "semantic commit" against the rivals and write paper 1 from it.
> 2. Lay the DL foundations: the OWL model, the proof format, and the test and
>    reference-reasoner infrastructure.
> 3. Build DL reasoning: a complete engine first, then the fast layers in front of it.
> 4. Bring DL into the store: complete query answers and explanations on top of the
>    materialisation, then paper 2.
>
> How you get there is yours to decide.

This plan is how. It draws on:
- the seven research reports of 2 October and their check (outside the repository:
  `output/dl-research-A…G.md` and `output/OWL-RS-dl-research-review-2026-10-02.md`);
- the competitor check (`output/OWL-RS-rust-competitors-2026-10-02.md`);
- [ADR-0009](../adr/0009-owl2-dl-reasoning.md), revised the same day.

What is done is in [STATUS.md](../STATUS.md); this plan holds the order.

Throughout every phase:
- **Bugs and security:** the bug hunt continues (the fuzz campaign, the oracle). Fixes go to `main` as hotfixes once CI and the oracle are green.
- **Milestones:** each phase ends in one, with an independent review of its diff and a perf-lab check against the previous one, before it is merged into `main`.

## Phase 1: finish and prove what we have (paper 1)

The claim of paper 1, from the competitor check: **reasoning maintained inside
transactions in a persistent SPARQL store, with consistency and explanations at commit
time, measured as correct, at interactive latency.** No rival in the field does all of
it. The check of the research reports calls this the *semantic commit*: explicit data,
closure, equality, consistency, SHACL and proofs as one commit.

1. **The query plan finished** ([design](2026-10-02-plan-ir.md)):
   - **Step 2's rest:** filter placement on the plan itself, and the executor's built-in strategies as named rewrites (paths from their bound end, limit placement, bind joins to `SERVICE`). `MIN`, `MAX` and `AVG` in eager aggregation, once their error semantics are checked.
   - **Step 3's rest:**
     - characteristic pairs for chain joins;
     - the physical operator chosen per node (index probe, merge, hash, leapfrog, semi-join reduction, grouped or factorised aggregation);
     - estimates checked against actual rows over the benchmark queries (q-error reported).
   - **Step 4:** the executor reads the plan, node kind by node kind. `native/mod.rs` is split into plan, execution, functions and federation, and numeric promotion lives once, in `nrese-xsd`.
2. **The cleanup:**
   - the audit's leftovers: `nrese_reasoner::v2` flattened, `nrese-core` folded in, `v1_scenarios.rs` renamed;
   - the duplicate dependency versions that are ours to fix;
   - the R1 residue (an older index rule kept until a rebuild);
   - Miri as a recurring job (F2);
   - coverage measured once (F3), its list of untested modules worked through.
3. **The semantic-commit benchmark** (the paper's own contribution; no standard exists):
   - **The update stream:** deterministic. It covers inserts, deletes, schema changes, `owl:sameAs` merges and splits, and deliberately invalid SHACL and consistency updates.
   - **Delete classes:** four (local, fan-out, redundant support, equality split).
   - **Checks:** a clean rematerialisation after every commit compares the closure; p50, p95 and p99 latency.
   - **Maintenance strategies:** DRed with backward/forward proofs (what NRESE has) against counting (which it lacks). Counting is built only if the benchmark shows it is needed.
   - **Rivals on the same machine:**
     - sparq;
     - Jena;
     - VLog, Nemo (with sparq's validated rule encoding; the scorecard's 319 s against sparq's 55 s is settled first) and reasonable;
     - open-ontologies;
     - RDFox and GraphDB where their licences allow.

     Result counts are compared before times.
4. **The query engine's credibility:** the 298 WDQS queries (Patel-Schneider 2025) and Sparqloscope against QLever, result hashes compared before times. The goal is within about 2× of QLever at 100 % correct answers; the gaps go back into the plan work above.
5. **Paper 1**, with the owner. The HisQu integration (RG × GS × GND) is its workload.

Milestone: the C work (logical plan, rewrites, estimates, eager aggregation, the
physical plan) reviewed independently and measured in the perf lab, then merged.

## Phase 2: DL foundations

Nothing of the DL engine is built before its correctness can be checked.

1. **The OWL model:**
   - the OWL 2 structural model (SROIQ(D) class and property expressions, axioms, datatypes) read from the store's triples by the reverse RDF mapping, with diagnostics for ill-formed input;
   - normalised into DL-clauses; the EL classifier becomes a client of it;
   - written for NRESE's ids, not on Horned-OWL (LGPL).
2. **The proof format:**
   - one proof representation for RL, EL and DL steps;
   - every step mapped back through the normalisation to the *source OWL axioms*, not to clauses or bare triples;
   - an API for justifications: `one`, `top-k`, `core`, `union` and lazy `all`;
   - PULi-style enumeration over the RL and EL derivations NRESE already records (useful before any DL code).
3. **The correctness infrastructure:**
   - **Conformance:** the W3C OWL 2 conformance suite for the direct semantics.
   - **Reference reasoners:** KoncludeCLI (not through OWLLink: its adapter, not Konclude, caused the out-of-memory failures in Lam et al. 2023); HermiT, Openllet and ELK through our own OWL API runner, not ROBOT, for timing; rustdl as a comparison, not an oracle.
   - **Output and disputes:** a canonical taxonomy and realisation format; disagreements recorded as `DISPUTED` and settled by hand, never by a majority vote.
   - **Testing the engine:** grammar-based SROIQ(D) fuzzing, with metamorphic tests ("optimisation on or off gives the same answers").
   - **Corpora:** fetched by hash and never committed (the ORE corpus has Zenodo terms; SNOMED is licensed). The full ORE runs go to the Draco cluster; the main PC keeps the development subsets.

## Phase 3: the DL engine

1. **The complete engine, a hypertableau:**
   - DL-clauses, anywhere blocking, individual reuse, dependency-directed backjumping and caching;
   - correctness first. Gates, in order: the W3C DL suite green, then parity with HermiT on the ORE 2015 corpus (1,751 classifications and 1,881 consistency checks in Lam et al. 2023).
2. **The fast layers in front of it:**
   - one context-saturation core (Bate et al.; Sequoia) whose rule families switch on step by step: Horn, then disjunction, cardinality and equality, then nominals;
   - the EL saturation stays a separate fast path (Sequoia needed about 131 s on SNOMED, ELK 2 s);
   - fallback to the hypertableau decided *dynamically* by the measured growth of clauses and equalities, with that telemetry from the first day;
   - additions incremental; deletions incremental while their dependency cone is bounded, else the affected module saturated again (incremental TBox deletion beyond EL is an open research problem); every path checked against a clean rebuild.
3. **The bar:** Konclude's 1,862 of 1,920 ORE classifications is the final target, not the first.

## Phase 4: DL in the store (paper 2)

1. **Complete answers:**
   - the RL closure as the persistent lower bound L;
   - a candidate upper bound U1 in a stack of its own, never visible as inferred data;
   - tighter bounds per query where needed;
   - every answer with a **completeness status** (`sound`, `complete`, the lower and upper counts, `unresolved`).

   A hypertableau is a complete oracle for consistency, entailment and atomic instance queries, not for arbitrary conjunctive queries. So the exact query service names the class of queries it is complete for, and non-monotone operators (`MINUS`, `NOT EXISTS`, aggregates) get exact answers only when the gap is closed.
2. **Explanations of DL answers** through the proof format, with justifications.
3. **Benchmarks where none exist:** incremental updates under full DL, explanation performance, complete certain answers over SPARQL.
4. **Paper 2.**

## Not in the four phases

These come after phase 1 or when a phase needs them, in an order decided with the owner:
- vector search (G5);
- hardware and scale-out (G6);
- the smaller items (G7);
- the enterprise evidence the competitor research lists (an audit log, an SBOM and signed releases, a threat model including inference side channels, a semantic recovery test);
- the frontend.
