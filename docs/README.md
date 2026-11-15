# NRESE documentation

Find your question below, and the page beside it answers it.

## Using NRESE

| Question | Document |
|---|---|
| How do I run it and send a first query? | the [quick start](../README.md#quick-start), then the [tutorial](tutorial.md). To build from source, [getting started](dev/getting-started.md), and the example's test and its last run are in the [record](dev/first-example-record.md) |
| What can it do, and with which limits? | [what it covers](what-it-covers.md) |
| What doesn't it do, and when should I choose another system? | [Limits and deviations](limits.md) |
| What changed since `main`, and what is slower? | [release notes, 1.5 (prerelease)](releases/v1.5.md) |
| How do I load my own data? | [your own data](tutorial.md#your-own-data), and for large files [bulk loading](ops/server-setup.md#54-bulk-loading) |
| How do I change a setting, or switch reasoning? | [change a setting](tutorial.md#change-a-setting-start-over), [every setting](ops/config-reference.md) |
| How do I run it as a service? | [server setup](ops/server-setup.md) and its [commands](ops/server-setup.md#commands) |
| Which endpoints are there? | [the HTTP interface](ops/http-api.md) |
| What does an error mean, and what do I do? | [status codes](ops/http-api.md#status-codes), [startup errors](ops/server-setup.md#startup-errors) |
| How do I back up and restore? | [a complete drill](ops/backup-restore-drills.md) |
| What does reasoning compute, and what does an answer promise? | [reasoning-semantics.md](spec/reasoning-semantics.md) |
| Which crates can I use on their own? | [ARCHITECTURE.md §2.4](ARCHITECTURE.md#24-dependencies-and-using-a-crate-on-its-own) |

## Working on NRESE

Each question has one document that answers it. When two disagree, the one higher in this
list wins, and the lower one is corrected in the same change.

| Question | Document |
|---|---|
| Who owns what, and in which layer is a problem fixed? | [ARCHITECTURE.md](ARCHITECTURE.md) |
| Which shared contract should a caller use, and where are the known bypasses? | [architecture/contracts.md](architecture/contracts.md) |
| Why was it decided so? | [adr/](adr/), and the standing decisions in [ADR-0010](adr/0010-standing-product-decisions.md) |
| In which order is it built? | [ROADMAP.md](ROADMAP.md) |
| What is done, what is open? | [STATUS.md](STATUS.md), with the open-work list, and before v2 goes to `main`, the status table of the [merge checklist](plan/2026-10-05-v2-merge-checklist.md) |
| Where are the gaps, who owns each? | [plan/2026-10-05-coverage.md](plan/2026-10-05-coverage.md) |
| How is NRESE fast, which ideas worked and which didn't? | [design/performance.md](design/performance.md), with the wins to keep and their guards (§0), the ideas by layer and the lab log |
| How does a component work? | [design/](#design) |
| What does reasoning compute? | [spec/reasoning-semantics.md](spec/reasoning-semantics.md) |
| What can it do, and with which limits? | [what it covers](what-it-covers.md) |
| How is it run and configured? | [ops/](#operations) |
| How is it measured? | [benches/README.md](../benches/README.md) (the four levels, hosts), and the rules in [benches/PROTOCOL.md](../benches/PROTOCOL.md) |
| What should research find out next? | [plan/2026-10-05-research-tasks.md](plan/2026-10-05-research-tasks.md) |

## Where is

These are the tools that exist, so that none is built twice. Each one's own header or README is
its manual.

| What | Where |
|---|---|
| Build, test, format, land: the gate's tiers | `scripts/check.sh commit\|push\|windows\|all`, [CONTRIBUTING.md](../CONTRIBUTING.md#checks), and the hooks in `.githooks/` |
| Builds and tests on other machines | fleet jobs, which the push hook and `scripts/check.sh windows` submit ([CONTRIBUTING.md](../CONTRIBUTING.md#building-and-measuring)), through `scripts/lib/fleet-job.sh` and `scripts/lib/gate-job.sh` |
| The release gate | `scripts/release-gate.sh` (its header), the checklist §4 in [plan/2026-10-05-v2-merge-checklist.md](plan/2026-10-05-v2-merge-checklist.md), and the approved trade-offs and exceptions in `benches/release-tradeoffs.toml` |
| Benchmarks: cases, workloads, systems, results | [benches/README.md](../benches/README.md), with the cases in `benches/cases/`, the catalogue [benches/CATALOG.md](../benches/CATALOG.md), the generated [STATUS](../benches/STATUS.md) and [STANDINGS](../benches/STANDINGS.md), and the runner `benches/suite/suite.py` |
| Timing a change against its base, or bisecting a slowdown (one family per candidate commit) | `benches/suite/abba.py` on the fleet, [benches/abba/README.md](../benches/abba/README.md), and the rules in [benches/PROTOCOL.md](../benches/PROTOCOL.md) |
| Proving HTTP behaviour on a real server | [scripts/verify/README.md](../scripts/verify/README.md), with [which scenario to run](../scripts/verify/README.md#choose-a-scenario) |
| W3C and other conformance suites | `scripts/fetch-w3c-tests.sh`, and each suite's runner and expected failures in its crate's `tests/` |
| Fuzzing, seed sweeps, Miri | [fuzz/README.md](../fuzz/README.md) and `scripts/fuzz-campaign.sh`, `scripts/seed-sweep.py` with `benches/seed-sweeps.toml`, and `scripts/miri.sh` |
| Checked files and lints | `scripts/check-locks.sh` (Cargo lock files current), `scripts/check-names.sh` (Docker names), `scripts/check-doc-paths.py` (paths the docs name exist) |
| Client compatibility | [tests/compat/README.md](../tests/compat/README.md) |
| Plan and state | [ROADMAP.md](ROADMAP.md), [STATUS.md](STATUS.md), the merge checklist above |

## Design

| Document | What |
|---|---|
| [performance.md](design/performance.md) | the performance ideas and their evidence: wins, guards and the lab log |
| [execution-core.md](design/execution-core.md) | the shared execution core (`nrese-exec`) |
| [query-plan.md](design/query-plan.md) | the query plan: logical plan, rewrites, physical choices, EXPLAIN |
| [reasoner-v2.md](design/reasoner-v2.md) | materialisation, maintenance, rulesets |
| [reasoner-provenance.md](design/reasoner-provenance.md) | provenance, explanations, justifications |
| [ql-rewriting.md](design/ql-rewriting.md) | OWL 2 QL answers through existentials: tree-witness rewriting over the closure, and the completeness each answer reports |
| [owl2-dl.md](design/owl2-dl.md), [owl2-dl-performance.md](design/owl2-dl-performance.md) | the OWL 2 DL engines and their performance plan |
| [shacl.md](design/shacl.md) | SHACL compilation and validation |
| [quarantine.md](design/quarantine.md) | what bulk loads do with statements they can't read: the quarantine, its resolution, the API |
| [rdf-bundle.md](design/rdf-bundle.md) | NRESE's own RDF, SPARQL-syntax and XSD crates |
| [research-designs.md](design/research-designs.md) | equality, vectors, compression, graph access, transactions: what the research of 2 October decided, and the query-progress proposal (§8) |
| [hardware-and-scaling.md](design/hardware-and-scaling.md) | using the hardware, scaling out, integration interfaces |
| [reasoning-benchmark.md](design/reasoning-benchmark.md) | the reasoning benchmark's tasks, datasets and oracles |

## Operations

[server-setup.md](ops/server-setup.md) covers deploying, loading and moving data in,
[config-reference.md](ops/config-reference.md) every setting and
[http-api.md](ops/http-api.md) every endpoint, and there are
[replication.md](ops/replication.md) and [backup-restore-drills.md](ops/backup-restore-drills.md).
The integrations are [ResearchSpace](integration/researchspace.md) and
[the Datamodel Workflow](integration/datamodel-workflow.md).
To contribute, there are [CONTRIBUTING.md](../CONTRIBUTING.md), [AGENTS.md](../AGENTS.md) for
agents and the [code rules](dev/code-structure-guidelines.md), and for the console the
[frontend contract](dev/frontend-backend-contract.md) and [extending it](dev/frontend-extension-guide.md).
