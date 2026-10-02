# Integration workload: Repertorium Germanicum × Germania Sacra × GND

A real-world reasoning workload for the benchmark suite: the data integration of the HisQu project, which links persons of the Repertorium Germanicum (RG, vol. 5), the Germania Sacra person database (GS) and the GND authority files by `owl:sameAs`, aligns the three vocabularies on the GND ontology, and answers ten competency questions over the result.

- **Source:** <https://github.com/HisQu/rgonline-gs-data-integration> (D. Motz, P. Stahl), read at commit `0efe44f` (17 April 2026).
- **Why it is here:** it is what reasoning stores are used for, and it is hard in a way the synthetic benchmarks are not. Every identity link replicates the statements of the linked resources, so the closure and the query results grow with the size of the identity groups. The project ran it on Fuseki with Jena's OWL rule reasoner over an in-memory model, and only with a large heap.
- **Not vendored.** The repository has no licence, and the RG source data is not public. Nothing of it is copied here: the scripts take a checkout (`RGGS_REPO`) and read queries and data from it. Publishing results that name the project needs its authors' agreement.

The task definitions (RT1 materialisation, RT3 queries under entailment) and the fairness rules are those of the [reasoning benchmark](../../docs/design/reasoning-benchmark.md) (§7 there describes this workload and what was found on it).

## Running it

```sh
git clone https://github.com/HisQu/rgonline-gs-data-integration ~/rgonline
export RGGS_REPO=~/rgonline

benches/integration/run.sh nrese example            # NRESE, OWL 2 RL
benches/integration/run.sh nrese-plain example      # NRESE without reasoning
(cd $RGGS_REPO && just fuseki-fetch)                # Fuseki 6.0.0, as the project uses it
FUSEKI_HEAP=8g benches/integration/run.sh fuseki-owl example
```

No containers: the systems run as processes, so the same script works on a workstation and on a cluster node ([../cluster](../cluster/README.md)). It needs `curl`, a Rust toolchain for NRESE and the query client (built on first use, through `scripts/cargo-guarded.sh`), and Java 21 for Fuseki.

## Tiers

| Tier | Input | Size | Where it comes from |
|---|---|---|---|
| `example` | The project's committed example data: the focused exports of four linked persons, the three sources' example extracts, and the alignment axioms | 21,261 statements | In the repository. A stand-in: enough to check that a system runs the workload and answers the questions |
| `current`, `TIER_NAME=cohort` | `data/harmonized/statements.ttl` after `just use-cohort`, `just harmonize` | persons active 1361–1447 | The project's pipeline. Needs the RG XML (private; a `GITHUB_TOKEN` of a project member) |
| `current`, `TIER_NAME=full` | the same after `just use-full` | all of GS (83 k persons) and the complete GND person and place dumps (1.4 GB compressed) | The project's pipeline, as above |

`ONTOLOGY=gndo` adds the GND ontology (247 classes, domains and ranges over `owl:unionOf` lists) and the RG vocabulary to the input. The project's own merge contains only its alignment axioms (`ONTOLOGY=project`, the default).

## Systems

| System | What runs | State |
|---|---|---|
| `nrese` | `nrese-server load` with `owl2-rl` (materialised, on disk), then the server | runs |
| `nrese-plain` | the same without reasoning | runs |
| `fuseki-owl` | Fuseki with the project's `fuseki-config.ttl`: Jena's `OWLFBRuleReasoner` over an in-memory model. The reasoner works at the first query, which the script times | runs |
| Fuseki with the project's `sameAs` rules (`fuseki-config-lightweight.ttl`) | | not runnable: the configuration names `rules/sameas.rules`, which is not in the repository |
| QLever, Oxigraph, Virtuoso (no reasoning: the plain baseline) | | to add, with the [basics kit's](../competitors/README.md) settings |
| GraphDB, RDFox, Stardog, AnzoGraph | | to add once licensed; their results stay in the git-ignored `results/` until the vendors permit publishing ([licence rules](../competitors/README.md)) |

## What is measured

One line per run in `results/<date>/integration.csv`:

| Column | Meaning |
|---|---|
| `load_s`, `load_peak_mib` | Until the data is loaded (for NRESE: loaded, reasoned and durable). Peak resident memory of the loading process |
| `asserted`, `inferred` | As the system reports them |
| `statements_answered`, `first_answer_s` | `SELECT (COUNT(*) …) { ?s ?p ?o }` as the first query: every statement the endpoint answers with, and how long that took. A reasoner that works lazily does its work here, so for Fuseki this is its reasoning time |
| `serve_peak_mib` | Peak resident memory of the server after the queries |
| `queries_answered`, `queries_failed`, `sum_p50_ms` | The competency questions through the harness's `query-mix`: one warm-up and `RUNS` measured runs each, the full result fetched as SPARQL JSON. Per query: `queries.json` |

Comparing systems means comparing answers first. The row counts per query are in `queries.json`; two systems under the same entailment regime must agree on them, and a difference is investigated before any time is compared.

## What the systems must agree on: the entailment regime

The answers depend on how much of `owl:sameAs` a system derives:

| Regime | What it derives | Who |
|---|---|---|
| Full equality (OWL 2 RL `eq-*` rules) | The identity closure, and every statement about a resource for each of its identities | NRESE `owl2-rl`, Jena's OWL rule reasoner; GraphDB and RDFox under OWL 2 RL, to be confirmed when they run |
| Identity closure only | `owl:sameAs` symmetric and transitive; statements are not replicated | The project's lightweight rules |
| None | | QLever, Oxigraph, Virtuoso as plain stores; `nrese-plain` |

The competency questions are written for the second regime: they follow `(owl:sameAs|^owl:sameAs)*` themselves. Under full equality they return more rows, because every answer appears once per identity of each resource in it. Results are compared within a regime.

## Known about the queries

- **CQ01** is an empty file (prefixes and comments). Every system answers it with a syntax error.
- **CQ05** joins a person's dates, places, affiliations and co-mentioned persons as independent OPTIONALs. Under full equality that is a product of about 10¹⁰ rows on the example tier (477 × 318 × 417 × 5 × 31). NRESE stops it at the query memory budget, and Fuseki didn't answer it in 120 s; see the reasoning benchmark design, §7.
- **An abandoned query keeps running in Fuseki.** The project's configuration sets no query timeout, so after the client gives up on a question, the questions after it share the machine with it.
- **CQ10** uses a `SELECT` alias in `HAVING`, which the standard doesn't define. NRESE reads it as Jena does.
