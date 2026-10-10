# NRESE documentation

Each question has one document that answers it. When two disagree, the one higher in this
list wins and the lower one is corrected in the same change.

| Question | Document |
|---|---|
| Who owns what; in which layer is a problem fixed? | [ARCHITECTURE.md](ARCHITECTURE.md) |
| Why was it decided so? | [adr/](adr/), the standing decisions in [ADR-0010](adr/0010-standing-product-decisions.md) |
| In which order is it built? | [ROADMAP.md](ROADMAP.md) |
| What is done, what is open? | [STATUS.md](STATUS.md); before v2 goes to `main`: the status table of the [merge checklist](plan/2026-10-05-v2-merge-checklist.md) |
| Where are the gaps, who owns each? | [plan/2026-10-05-coverage.md](plan/2026-10-05-coverage.md) |
| How is NRESE fast, which ideas worked and which didn't? | [design/performance.md](design/performance.md): the wins to keep with their guards (§0), the ideas by layer, the lab log |
| How does a component work? | [design/](#design) |
| What does reasoning compute? | [spec/reasoning-semantics.md](spec/reasoning-semantics.md) |
| Where do we stand against QLever and GraphDB? | [spec/06-target-capability-matrix.md](spec/06-target-capability-matrix.md) |
| How is it run and configured? | [ops/](#operations) |
| How is it measured? | [benches/README.md](../benches/README.md), the rules in [benches/PROTOCOL.md](../benches/PROTOCOL.md) |
| What should research find out next? | [plan/2026-10-05-research-tasks.md](plan/2026-10-05-research-tasks.md) |

## Design

| Document | What |
|---|---|
| [performance.md](design/performance.md) | the performance ideas and their evidence; wins, guards, lab log |
| [execution-core.md](design/execution-core.md) | the shared execution core (`nrese-exec`) |
| [query-plan.md](design/query-plan.md) | the query plan: logical plan, rewrites, physical choices, EXPLAIN |
| [reasoner-v2.md](design/reasoner-v2.md) | materialisation, maintenance, rulesets |
| [reasoner-provenance.md](design/reasoner-provenance.md) | provenance, explanations, justifications |
| [ql-rewriting.md](design/ql-rewriting.md) | OWL 2 QL answers through existentials: tree-witness rewriting over the closure, and the completeness each answer reports |
| [owl2-dl.md](design/owl2-dl.md), [owl2-dl-performance.md](design/owl2-dl-performance.md) | the OWL 2 DL engines and their performance plan |
| [shacl.md](design/shacl.md) | SHACL compilation and validation |
| [rdf-bundle.md](design/rdf-bundle.md) | NRESE's own RDF, SPARQL-syntax and XSD crates |
| [research-designs.md](design/research-designs.md) | equality, vectors, compression, graph access, transactions: what the research of 2 October decided |
| [hardware-and-scaling.md](design/hardware-and-scaling.md) | using the hardware, scaling out, integration interfaces |
| [reasoning-benchmark.md](design/reasoning-benchmark.md) | the reasoning benchmark's tasks, datasets and oracles |

## Operations

[server-setup.md](ops/server-setup.md) (deploying, loading, moving data in),
[config-reference.md](ops/config-reference.md) (every setting),
[http-api.md](ops/http-api.md) (every endpoint),
[replication.md](ops/replication.md), [backup-restore-drills.md](ops/backup-restore-drills.md).
Integrations: [ResearchSpace](integration/researchspace.md),
[the Datamodel Workflow](integration/datamodel-workflow.md).
Contributing: [CONTRIBUTING.md](../CONTRIBUTING.md), for agents [AGENTS.md](../AGENTS.md), [code rules](dev/code-structure-guidelines.md),
the console: [frontend contract](dev/frontend-backend-contract.md),
[extending it](dev/frontend-extension-guide.md).

## Implementation plans and proposals

[Performance and architecture, 9 October](plan/2026-10-09-performance-architecture.md):
source-based review, authorised bounded refactoring, work packages, ownership and verification criteria.
Not an accepted replacement for the architecture, roadmap or v2 merge checklist.

[Query progress, 10 October](plan/2026-10-10-query-progress.md): proposed replacement
of blocking cache/worker coordination, its ownership boundary and unresolved callback
contract. Design only; not an implemented or accepted execution model.
