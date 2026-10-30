# Cases

Every measured case of the benchmark suite, one schema for all of them: a focused case of one
bottleneck and a home-turf comparison with a competitor's benchmark alike. The schema has
this one owner; [suite/suitekit/cases.py](../suite/suitekit/cases.py) validates it
(`python benches/suite/suite.py --check`, and the gate's suite contracts on every commit).

## Files

One file per group, so that the groups' authors never edit the same file. A case's name is
unique across all of them; its group is its file's name.

| File | Group | What |
|---|---|---|
| `core.toml` | core | NRESE's own focused cases: every reasoning profile, maintenance, DL, memory caps, query shapes and operators, services, kernels, layout, plans (the fast suite) |
| `sparql.toml` | sparql | the SPARQL stores on their home turf: QLever, Virtuoso, MillenniumDB, Oxigraph, Jena; Sparqloscope, BSBM, WatDiv, SP2Bench, WDBench, G-CARE |
| `reasoning.toml` | reasoning | the rule reasoners on theirs: RDFox, GraphDB, Nemo, VLog, Soufflé; Claros, SSPE, SPB, Datalog programs, OWL2Bench RL and QL |
| `dl.toml` | dl | the DL reasoners and OBDA on theirs: Konclude, HermiT, Openllet, ELK, PAGOdA, Ontop; ORE 2015, OWL2Bench EL and DL, NPD |
| `breadth.toml` | breadth | SHACL, GeoSPARQL, text, RDF 1.2, federation, vectors, larger than memory |
| `pg.toml` | pg | property graphs: LDBC SNB mapped to RDF 1.2, the storage measurements (load counts, a hop's micro-costs) |

A benchmark family (BSBM, WatDiv, LUBM, ...) is an entry in
[suite/workloads.toml](../suite/workloads.toml): its source, licence, tiers and state, for
the coverage matrix. A case is what runs: one family at one tier, or one bottleneck, and it
names its family with `workload` and `tier`.

## A case

```toml
[[case]]
name = "bsbm-explore-100k"                 # unique across benches/cases/
area = "query"
what = "BSBM explore mix, 100 k products: 12 queries of an e-commerce browsing session"
workload = "bsbm"; tier = "100k"            # the family it measures (workloads.toml)
levels = ["full"]                           # focused (level 2), full (level 3), or both
semantics = "none"                          # what NRESE computes
data = "volume:bsbm-100k.nt"                # a frozen dataset (datasets.toml), or a generator
queries = "@bsbm-explore"                   # benches/competitors/queries/bsbm-explore
metric = "sum_p50_ms"
check = { "q.q01.rows" = 10 }               # known answers, before any time counts
expect = { answers = ["jena", "oxigraph"] } # the reference systems, in order (answers.py)
systems = ["virtuoso:plain", "qlever:plain", "oxigraph:plain", "jena:plain"]
home_turf = "virtuoso"                      # whose documented strength this is
scenario = "native"                         # native, matched or frontier (PROTOCOL.md §3)
needs = ["docker"]                          # what the host must have (hosts.toml `has`)
cap_gb = 16; est_min = 20
```

| Field | Meaning |
|---|---|
| `name`, `area`, `what` | the case, its area (below), what it measures in a line. Required |
| `workload`, `tier` | the benchmark family and tier it runs (a key of suite/workloads.toml) |
| `levels` | `focused`, `full`: the levels that run it ([../README.md](../README.md#four-levels)); default both |
| `est_min` | minutes a run is expected to take on a measure host: what the queue plans with |
| `needs` | `docker`, `java`, `apptainer`: what the host must have; a runner refuses a host without |
| `scenario` | `native` (default; each system as its documentation tunes it), `matched` (the same CPUs and memory for all) or `frontier` (one dimension swept) |
| `semantics` | what NRESE computes: `none`, `rdfs`, `rdfs-full`, `rdfs-plus`, `owl-horst`, `owl2-ql`, `owl2-rl`, `owl2-el`, `owl2-dl`, `custom` (N3 rules), `el`, `dl`, `canonical`. Required |
| `data` | `KIND key=value ...` for `perf_lab generate` (seeded, with its own answers in `.expect.json`), `volume:FILE` (the datasets: a full-level case's file is frozen in [../datasets.toml](../datasets.toml)), `input:NAME/PATH` (a file of a verified shared input, [../hosts.toml](../hosts.toml) `[inputs]`, its receipts beside it), `ore:ID` (an ORE 2015 ontology), `concat:A+B`, or `none`. Required |
| `extra`, `data_alt` | more data specs loaded with it; a second file the DL tools check |
| `queries` | a directory of `.rq` files: `NAME` (benches/fast/queries/NAME), `@FAMILY` (benches/competitors/queries/FAMILY), `lubm` or `owl2bench` (benches/reasoning/queries), or a generated set (`@vectors`, `@parse`, `@cache-log`) |
| `tool`, `args`, `env`, `setup`, `repeat`, `sweep`, `tool_loads` | how NRESE measures it: `perf_lab` (default), `tableau`, `classify-el`, `classify-horn`, `pg_hops` (nrese-engine's example: a property graph's load counts and hop micro-costs, run by benches/property-graphs); their arguments (`{data}`, `{scratch}` substituted), environment, a one-time setup, fresh-process repetitions, argument sweeps |
| `page_cache` | `["warm"]` (default) or `["warm", "cold"]`: fresh processes alternate, the page cache emptied before each cold one by the host's `cold_cache` command; a host without one runs warm and says so |
| `timeout_s`, `cap_gb` | the time limit of a DL tool; the memory cap without swap (required) |
| `metric`, `counters` | the number the case is timed by (a path in the result: `sum_p50_ms`, `reason.p50_ms`, `load_s`, `cold_ms`, `whole_ms`, `clients.p99_ms`, ...); further work counters to compare exactly. `metric` is required |
| `check`, `pinned` | known answers, checked before a time counts: `path = value` (`expect.KEY` is the generator's answer; a number or string is pinned, with `pinned` saying where it comes from), or `">= 0.9"`, `"has X"` |
| `route` | the plan or engine path the case must take: `path = "OP VALUE"` (`==`, `<=`, `>=`, `has`, `lacks`) |
| `expect` | `{ answers = [SYSTEM, ...] }`: the reference systems the comparators' captured answers are checked against, in order (the first with a complete, stable answer, confirmed by the next; never NRESE: [../suite/suitekit/answers.py](../suite/suitekit/answers.py)); default jena, oxigraph, virtuoso, qlever. Single answers the case checks (`q.NAME.value`, `q.NAME.rows`) are known answers to every system |
| `reference` | a reference taxonomy file for the classification cases |
| `outcome`, `known`, `frontier` | a known product issue: the outcome a run ends as (`memory-limit`, `gave-up`, `timeout`, `unsupported`, `wrong`), what the issue is (required with an outcome), and the settings to run it once more past its limit (`cap_gb`, `timeout_s`) |
| `systems` | the comparators, `SYSTEM:SEMANTICS`: `plain` (the same SPARQL on the same data), `closure` (NRESE's closure, for stores without reasoning), `rewritten` (NRESE's rewritten queries), or the system's own `rdfs`, `rdfs-plus`, `owl-horst`, `owl2-rl`, `owl2-ql`, `custom`, `el`, `dl`, `shacl`, `text`, `geosparql`, `federation`, `vector`. A system is a key of [../suite/systems.toml](../suite/systems.toml). `[]`: NRESE only. Required |
| `home_turf` | the competitor whose documented strength the case is ([../CATALOG.md](../CATALOG.md) §6); the standings count the case for it |
| `rewrite`, `hangs`, `tracks` | what rewriting can express (`expressible`, `per-query`, `not-expressible`); comparator pairs known to hang (`"system:semantics" = "why"`, not run again); comparator settings tracks (defaults, best configuration) |
| `guards` | the wins of docs/design/performance.md §0 the case measures |

Areas: `access`, `cache`, `concurrency`, `dl`, `federation`, `geo`, `kernel`, `layout`, `load`,
`maintenance`, `memory`, `operator`, `parse`, `plan`, `property-graphs`, `query`, `rdf12`,
`reasoning`, `rules`, `service`, `shacl`, `text`, `vector`. `access` is reasoning under access
control: what a restricted reader may see of the inferred statements. `property-graphs` is a
property graph stored as RDF 1.2 (edges with properties reified): its storage, hops and paths.

Paths in a perf-lab result: `reason.inferred`, `reason.rounds`, `reason.p50_ms`,
`commits.p50_ms`, `commits.incremental`, `closure_after_commits.equal`, `loaded`, `load_s`,
`shacl.results`, `clients.qps`, `canonicalize.equal`, `q.QUERY.rows`, `q.QUERY.value`,
`q.QUERY.operators`, `sum_p50_ms`, `peak_mib`, `cgroup_peak_mib`; the DL tools': `answer`,
`whole_ms`, their telemetry's counters (`branch_points`, `clauses`, `nodes_created`, ...),
`hash`.

## Rules every case keeps

- **Answers before time.** A case checks what it can know (`check`, the generator's
  answers); every comparator's full answers are captured and checked against the reference
  systems ([../suite/suitekit/answers.py](../suite/suitekit/answers.py)) before its time
  counts. A wrong or incomplete answer is a verdict, never a time.
- **Equal terms.** All systems of a comparison run on the same host and configuration
  ([../hosts.toml](../hosts.toml); compare refuses otherwise), under the case's `scenario`;
  native means each as its documentation tunes it, matched the same CPUs and memory.
- **Home turf.** A competitor's case is its own benchmark, its own data and queries, at its
  published scale, with its recommended configuration; NRESE runs the same.
- **Licences.** Results of GraphDB, RDFox, Stardog and AnzoGraph never leave the machine
  without the vendor's written consent ([../competitors/README.md](../competitors/README.md)).
- **Known issues stay visible.** An expected outcome fails no gate but every run's verdict
  says it; a case is never weakened to pass.

## Running

| What | How |
|---|---|
| NRESE's measurement of cases | `python benches/fast/fast.py run --cases A,B` (Docker, or `NRESE_FAST_RUNTIME=native`) |
| the comparators of cases | `python benches/fast/fast.py compete --cases A,B [--systems S]` |
| a comparison of two runs | `python benches/fast/fast.py compare BASE NEW` |
| validation | `python benches/suite/suite.py --check` |
