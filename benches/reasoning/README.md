# Reasoning benchmark kit (Pf0-R)

This kit is the headline evidence for NRESE: ontology reasoning measured against the systems that reason today, with every result checked for correctness. The plan and task definitions (RT1–RT7) are in [docs/design/reasoning-benchmark.md](../../docs/design/reasoning-benchmark.md). The load and query basics are measured separately, in [../competitors](../competitors/README.md).

The licence rules in [../competitors/README.md](../competitors/README.md) apply unchanged: GraphDB, RDFox, AnzoGraph and Stardog results stay in the git-ignored `results/` unless the vendor gives written permission.

## Quick start

```sh
benches/reasoning/prepare-lubm.sh 1 10 100              # LUBM(N) + univ-bench ontology
benches/reasoning/prepare-owl2bench.sh RL:1 EL:1 QL:1 DL:1
benches/reasoning/reasoning-scorecard.sh lubm-1 owl2bench-rl-1 lubm-10
```

Everything runs in Docker on the same host. The data lives in the volume `nrese-bench-data`, and results go to `results/<date>/`.

## What's here

| Path | What |
|---|---|
| `reasoning-scorecard.sh` | Driver. Per dataset and system: materialisation time, inferred count, peak memory, correctness against the oracle, query answers. |
| `compare_inferred.py` | Correctness: a system's inferred set against the oracle's. Instance and schema levels are reported separately, after a documented normalisation of tautologies, and verified outside-RL entailments count as explained. |
| `prepare-lubm.sh`, `lubm/` | The LUBM UBA generator (rvesse/lubm-uba, pinned) and the univ-bench ontology |
| `prepare-owl2bench.sh`, `owl2bench/` | The OWL2Bench generator (kracr/owl2bench, pinned; seed 1 as in the paper), with one TBox per profile |
| `queries/lubm/` | The 14 LUBM queries, plus the published LUBM(1) answer counts (`expected-lubm-1.tsv`) |
| `queries/owl2bench/` | The 22 OWL2Bench queries (Zenodo 10.5281/zenodo.3838735, CC-BY-4.0) and the profiles each one targets |
| `oracle/` | owlrl (the reference OWL 2 RL implementation) computes the closure, and pyoxigraph answers the queries over it. Correctness only; never a performance number. |
| NRESE v2 | `crates/nrese-reasoner/examples/v2_closure.rs`: reasoner v2's batch executor, built in Docker; `local-bulk.sh` adds end-to-end load, queries and commit latency through the store |
| `jena/` | Jena's rule reasoners (RDFS, OWL micro/mini/full), in memory |
| `nemo/` | Nemo (Rust datalog, TU Dresden) with three encodings: `owl2rl.rls` (our translation of the W3C OWL 2 RL/RDF rule tables), `owl2rl-schemafirst.rls` (the same, schema folded first) and sparq's LUBM-tailored one; provenance and closure checks in [nemo/README.md](nemo/README.md) |

## Reading the results

- **Oracle validation.** On LUBM(1), the oracle's closure answers all 14 queries with the published counts. That's what makes it usable as the reference.
- **precision / recall** are instance-level (facts about individuals, which queries see). Normalisation removes facts that are trivially true for everything: `x a owl:Thing`, `x sameAs x`, `p rdfs:domain owl:Thing`, and axiomatic vocabulary triples.
- **explained** counts extra facts that are entailed but aren't in the OWL 2 RL/RDF rules. Two structural checks against the input verify them:
  - `owl:differentFrom` between members of an `owl:AllDifferent`
  - `x p x` where p is an `owl:ReflexiveProperty`
- **schema_precision / schema_recall** cover subClassOf, domain, inverseOf, disjointWith and meta-typing. Systems differ here by design: Jena, for example, states `inverseOf` and `disjointWith` symmetrically, and the RL rules don't.
- **queries_correct** is `k/n` against the published answers where they exist (LUBM(1)). Everywhere else, the answer counts are cross-checked across systems at the end of each dataset.

## Data quirks fixed on the way in (documented, reproducible)

- **LUBM:** the ontology is published under `swat.cse.lehigh.edu`, but UBA data and the queries use `www.lehigh.edu/~zhp2/2004/0401/univ-bench.owl#`. The ontology is rewritten to match. The `<> owl:imports` header lines aren't valid N-Triples and are dropped.
- **OWL2Bench:** the generator (since commit 1539dad, 2022) writes `https://kracr.iiitd.edu.in/OWL2Bench#`, but the TBoxes at the repository root still use `http://benchmark/OWL2Bench#`. With those TBoxes, no axiom touches the ABox. The kit uses the matching TBoxes from `OWL2Bench/`, and the queries use the generator's namespace.

## Cleaning up after a run

A run leaves nothing behind but its results. The scorecard scripts call `scripts/bench-cleanup.sh` when they exit, also when they fail or are interrupted. It removes:
- the run's containers and store volumes;
- the dataset volume `nrese-bench-data` and local datasets under `~/nrese-bench`;
- the images built here (`nrese-bench/*`) and the images the run pulled itself (an image that was already on the machine stays);
- inferred-set dumps (`*.inferred.nt`); the scorecard keeps their counts and comparisons.

Scorecards (CSV), logs and reports stay.

For several runs in a row, set `NRESE_BENCH_KEEP=1` so datasets and images survive between them, and run `scripts/bench-cleanup.sh` once at the end. `--dry-run` shows what it would remove. It only removes things by the names these scripts use, never "everything unused", so other projects' containers, volumes and images are safe.
