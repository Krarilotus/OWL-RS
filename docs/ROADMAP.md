# Roadmap

This page gives the order of work, the detail of the "planned" list in the
[scope](what-it-covers.md#scope), and only a stage marked done here is in the code. What is done
is in [STATUS.md](STATUS.md), what is left before v2 goes to `main` is the status table of the
[merge checklist](plan/2026-10-05-v2-merge-checklist.md), and why things are decided as they
are is in [adr/](adr/), above all [ADR-0010](adr/0010-standing-product-decisions.md).

## Goal

The goal is an RDF database and ontology platform with reasoning at its centre
([vision](spec/00-vision-and-scope.md)). What it does today, with its limits, is listed
in [What it covers](what-it-covers.md). What is still to come is the order below, and across
machines only read replicas are built so far.

## The order of work

This order dates from 5 October 2026.

1. Finish v2 with the open items of the merge checklist, as its
   [status table](plan/2026-10-05-v2-merge-checklist.md) lists them.
2. Benchmark it with the fast regression suite, the core workloads with a real
   reasoning workload, workloads other stores are designed for as well (so that no result
   rests on a choice that suits NRESE), and a defaults track beside the
   best-configuration track ([benches/PROTOCOL.md](../benches/PROTOCOL.md)).
3. Merge v2 into `main` when every guard is green and nothing regressed.
4. Concise docs and a major refactoring, with the README and docs short enough to
   contribute from, files over about 600 lines split, Miri over the unsafe code, measured
   test coverage, work on duplicate dependency versions and the last typed errors.
5. v3.
6. The new goals G8 to G13, each described in [STATUS](STATUS.md#engine-gaps):
   - G8, property-graph queries in ISO GQL and openCypher over the same store, compiled to the
     same algebra so that reasoning applies to them
   - G9, enterprise operations, which means automatic failover and a write cluster, backups to
     object storage, Kubernetes packaging, LDAP, SAML and SCIM besides OIDC, and tenants with
     quotas
   - G10, data integration, which means mappings from relational, CSV and JSON sources, virtual
     graphs over live SQL sources, a SQL endpoint for BI tools, a GraphQL API and change streams
   - G11, AI integration, which means an MCP server for agents, questions in natural language
     turned into SPARQL, GraphRAG over the graph and its indexes and local models besides hosted
     ones
   - G12, exploration and history, which means visual exploration in the console, versioned
     graphs (queries at a revision, diffs and branches) and suggested `owl:sameAs` matches
   - G13, ontology work and publishing, which means SKOS taxonomy editing, an ontology editor in
     the browser, Linked Data publishing, more rule languages and a Python SDK

## How work is checked

- Correctness comes first, with differential tests against an oracle for every optimisation
  (the reference SPARQL evaluator, the naive rule evaluator, HermiT and Konclude for DL),
  the fuzz campaign and the W3C suites.
- Each win keeps its guard, a cheap deterministic test (counts, plan shapes, bounds)
  in the same commit as the win ([design/performance.md](design/performance.md) §0), so a
  regression fails where it happens, not in a large benchmark run.
- Benchmarks run in bulk at the end of a batch, interleaved, with confidence intervals.
- Milestones end with an independent review of the diff and a perf-lab check against
  the previous milestone before they go to `main`. Bug and security fixes go to `main` as
  hotfixes once CI is green.
- One paper comes at the end, about the complete store including DL.

## Reasoning that leads, on every level

Every reasoning component starts from the state of the art and is then worked top-down
(method, plan, operators, data layout, machine code). At each level it uses what a reasoner built into
its own store can do that a reasoner bolted onto a store can't.

| Level | The state of the art to reach | What NRESE's full stack adds |
|---|---|---|
| Method | Parallel materialisation (Motik et al., AAAI 2014); incremental maintenance by DRed, B/F and counting, chosen per change; modular reasoning for closures, hierarchies and equality (Hu, Motik, Horrocks); equality by rewriting; for DL, [ADR-0009](adr/0009-owl2-dl-reasoning.md) | Maintenance inside the commit; the schema compiled first; the DL engines fed by the persistent RL closure |
| Plan | Rule bodies ordered by cost; semi-naive evaluation; magic sets where nothing is materialised | Rule bodies planned on the store's own statistics (exact counts, characteristic sets), shared with queries |
| Operators | Worst-case-optimal joins; SCC closures; union-find for equality | The query executor's operators reused by the reasoner; deltas as sorted runs joined without materialised intermediates |
| Layout | Compact ids; inferred facts apart from asserted ones | The inferred stack as runs beside the asserted ones; inline literals compared without decoding; equality as representatives |
| Machine | Parallelism across rules and partitions | Morsel parallelism and SIMD intersections shared with queries; memory budgets and cancellation shared; hardware-aware defaults ([hardware and scaling](design/hardware-and-scaling.md)) |

## DL

The design is in [design/owl2-dl.md](design/owl2-dl.md), component by component, and its
performance plan in [design/owl2-dl-performance.md](design/owl2-dl-performance.md).

1. The foundations are done (3 October). They are the OWL 2 structural model and normaliser,
   one proof format for RL, EL and DL steps with justifications, the W3C suite and the reference
   reasoners (Konclude, HermiT, Openllet, ELK), with disputes settled by hand, never by majority.
2. The engine is done for coverage, and its performance is open. The hypertableau was made
   complete first (every case of the W3C DL suite decided), with fast layers in front of it, a
   context-saturation core whose rule families switch on step by step (the Horn stage done, the
   SRIQ and SROIQ stages open) and the EL saturation as a separate fast path. The fallback is
   decided by measured growth. Speed on the ORE 2015 classifications is the open work
   ([STATUS](STATUS.md#owl-2-dl-v15-branch-v15dl-complete-12-october)).
3. DL in the store (G3) is done, and tighter bounds are open. The RL closure is the persistent
   lower bound, and a candidate upper bound is never visible as inferred data, with tighter
   bounds per query. Completeness is always reported (`sound`, `complete`, the bounds,
   `unresolved`), work is routed by profile (EL to the consequence-based core, Horn to rules, the
   rest to the tableau), and explanations go through the proof format.

## Not yet ordered

Not yet ordered are the smaller items (STATUS G7), the enterprise evidence (audit log, SBOM and
signed releases, a threat model including inference side channels), the frontend on the engine
API ([ADR-0007](adr/0007-one-engine-api.md)) and scaling across machines
([hardware and scaling](design/hardware-and-scaling.md)).
