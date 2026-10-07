# ADR-0010: Standing product decisions

Status: accepted (collected 6 October 2026 from the roadmap of 25 September, the owner's
decisions of 30 September and later ones; each line keeps its date). These are the
decisions that still bind. Superseded ones are left out; git history has them.

## Product and evidence

| Decision | Since |
|---|---|
| **Reasoning leads.** The headline benchmarks are about ontology reasoning; load, storage, query and write speed are the basics that must still match or beat the best system on each measure (QLever for queries, the fastest loader for loads), never traded for reasoning. | 26 Sep (D9, D10) |
| **Behaviour is configurable, defaults are tuned.** Ruleset, timing, consistency, placement, `sameAs`, maintenance per repository; read model, explanations, freshness per request. Only the stack invariants are fixed. | 26 Sep (D7) |
| **Performance is judged on a multi-metric scorecard,** with tuned defaults and configurable presets. Results of licensed systems (GraphDB, RDFox, Stardog, AnzoGraph, AllegroGraph) are never published without the vendor's written permission. | 26 Sep (D8) |
| **A win counts on the competitor's home turf.** Each competitor is measured on what it does best as well as on our workloads; NRESE runs every case in every profile, competitors only what they support ([benches/PROTOCOL.md](../../benches/PROTOCOL.md) §3). | 5 Oct |

## Architecture

| Decision | Since |
|---|---|
| **Asserted and inferred data in separate index stacks,** not per-entry flags. | 25 Sep (D2) |
| **One shared execution core** (`nrese-exec`) for SPARQL and the reasoner: the same id tables, sorts, joins and memory budgets, so every optimisation speeds up both. | 27 Sep (D11) |
| **Full-text search with tantivy.** | 25 Sep (D3) |
| **All the hardware a machine has.** The hardware is measured once at installation and every layer may rely on that profile (each derived setting overridable). Targets: Intel and AMD desktops and servers, ARM servers, cluster nodes. GPUs take parallel workloads wherever they measure better, the CPU path always available ([hardware-and-scaling.md](../design/hardware-and-scaling.md)). | 7 Oct |

## Code and dependencies

| Decision | Since |
|---|---|
| **Rust, natively.** Protocols and formats are implemented in Rust in the server and its crates: no JVM sidecar, no wrapped reference implementation, no interpreted glue on a request path. | 30 Sep |
| **No C or C++ build dependencies for speed.** Rust first, assembly second, built to beat the foreign library; such a library may serve only as an A/B reference outside the build (vqsort, Highway). | 6 Oct |
| **Libraries behind our own traits, replaced where they cost us.** A library is allowed if its licence permits commercial use without conditions on our code (MIT, Apache-2.0, BSD and similar) and it is maintained; it is measured in the perf lab and replaced by our own implementation where its architecture doesn't fit the engine or it measures worse. | 30 Sep |
| **Licence: MIT OR Apache-2.0.** Contributions come in under the same terms ([CONTRIBUTING.md](../../CONTRIBUTING.md)). Before a first sale, a lawyer checks the copyright line and the parts written with an AI assistant. | 1 Oct (replaced AGPL of 30 Sep) |
