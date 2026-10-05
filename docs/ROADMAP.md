# Roadmap

The order of work. What is done is in [STATUS.md](STATUS.md); what is left before v2
goes to `main` is the status table of the
[merge checklist](plan/2026-10-05-v2-merge-checklist.md); why things are decided as they
are is in [adr/](adr/), above all [ADR-0010](adr/0010-standing-product-decisions.md).

## Goal

An RDF database and ontology platform that reads like QLever, governs data like GraphDB
and leads in reasoning: OWL 2 RL materialised and maintained incrementally, OWL 2 DL with
complete answers where the plan has a complete path, on every core of one machine and
across machines where a use case needs it ([vision](spec/00-vision-and-scope.md)).

## The order (owner, 5 October 2026)

1. **Finish v2:** the open items of the merge checklist (DL classification and its
   outliers, the store's DL mode, the query items, materialisation memory, OWL 2 QL
   rewriting).
2. **Benchmark it:** the fast regression suite, the core workloads with the Zebratlas
   reasoning workload, each competitor on its home turf, a defaults track beside the
   best-configuration track ([benches/PROTOCOL.md](../benches/PROTOCOL.md)).
3. **Merge v2 into `main`** when every guard is green and nothing regressed.
4. **Concise docs and a major refactoring:** the README and docs short enough to
   contribute from; files over about 600 lines split; Miri over the unsafe code; measured
   test coverage; duplicate dependency versions; the last typed errors.
5. **v3.**
6. **The new goals G8–G13** (STATUS): property graphs (ISO GQL, openCypher) over the same
   store, enterprise operations, data integration, AI integration, exploration and
   history, ontology work and publishing.

## How work is checked

- **Correctness first:** differential tests against an oracle for every optimisation
  (the reference SPARQL evaluator, the naive rule evaluator, HermiT and Konclude for DL),
  the fuzz campaign, the W3C suites.
- **Each win keeps its guard:** a cheap deterministic test (counts, plan shapes, bounds)
  in the same commit as the win ([design/performance.md](design/performance.md) §0), so a
  regression fails where it happens, not in a large benchmark run.
- **Benchmarks in bulk** at the end of a batch, interleaved, with confidence intervals.
- **Milestones** end with an independent review of the diff and a perf-lab check against
  the previous milestone before they go to `main`; bug and security fixes go to `main` as
  hotfixes once CI is green.
- **One paper, at the end,** about the complete store including DL.

## Reasoning that leads, on every level

Every reasoning component starts from the state of the art and is then worked top-down
(method, plan, operators, data layout, machine code), using what a reasoner built into
its own store can do that a reasoner bolted onto a store can't.

| Level | The state of the art to reach | What NRESE's full stack adds |
|---|---|---|
| Method | Parallel materialisation (RDFox; Motik et al., AAAI 2014); incremental maintenance by DRed, B/F and counting, chosen per change; modular reasoning for closures, hierarchies and equality (Hu, Motik, Horrocks); equality by rewriting; for DL, [ADR-0009](adr/0009-owl2-dl-reasoning.md) | Maintenance inside the commit; the schema compiled first; the DL engines fed by the persistent RL closure |
| Plan | Rule bodies ordered by cost; semi-naive evaluation; magic sets where nothing is materialised | Rule bodies planned on the store's own statistics (exact counts, characteristic sets), shared with queries |
| Operators | Worst-case-optimal joins; SCC closures; union-find for equality | The query executor's operators reused by the reasoner; deltas as sorted runs joined without materialised intermediates |
| Layout | Compact ids; inferred facts apart from asserted ones | The inferred stack as runs beside the asserted ones; inline literals compared without decoding; equality as representatives |
| Machine | Parallelism across rules and partitions | Morsel parallelism and SIMD intersections shared with queries; memory budgets and cancellation shared; hardware-aware defaults ([hardware and scaling](design/hardware-and-scaling.md)) |

## DL

The design, component by component: [design/owl2-dl.md](design/owl2-dl.md); its
performance plan: [design/owl2-dl-performance.md](design/owl2-dl-performance.md).

1. **Foundations** (done 3 October): the OWL 2 structural model and normaliser, one proof
   format for RL, EL and DL steps with justifications, the W3C suite, reference reasoners
   (Konclude, HermiT, Openllet, ELK), disputes settled by hand, never by majority.
2. **The engine** (in progress): the hypertableau, complete first (W3C DL suite, then
   HermiT parity on ORE 2015); fast layers in front of it (a context-saturation core whose
   rule families switch on step by step, the EL saturation as a separate fast path), the
   fallback decided by measured growth. Konclude's 1,862 of 1,920 ORE classifications is
   the final bar.
3. **DL in the store** (G3, next): the RL closure as the persistent lower bound, a
   candidate upper bound never visible as inferred data, tighter bounds per query.
   Completeness is always reported (`sound`, `complete`, the bounds, `unresolved`);
   routing by profile (EL to the consequence-based core, Horn to rules, the rest to the
   tableau); explanations through the proof format.

## Not yet ordered

The smaller items (STATUS G7), the enterprise evidence (audit log, SBOM and signed
releases, a threat model including inference side channels), the frontend on the engine
API ([ADR-0007](adr/0007-one-engine-api.md)), scaling across machines
([hardware and scaling](design/hardware-and-scaling.md)).
