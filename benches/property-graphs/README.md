# Property graphs: LDBC SNB over RDF 1.2

The storage measurements for property-graph traversal (area `property-graphs`, cases in
[../cases/pg.toml](../cases/pg.toml), workload `ldbc-snb-interactive-v2`): what a load of the
SNB graph holds (M0) and where a hop's time goes (M1). NRESE only. Every result is **not an LDBC
benchmark result** (LDBC fair-use policy): an unaudited run of the data, not of the benchmark.

| Step | Tool | What |
|---|---|---|
| D1 | `snb-fetch.sh SF...` | the datagen's published Interactive v2 initial data sets (the BI initial snapshots, composite-merged-fk CSV, which the Interactive v2 implementations load), hashed and extracted read-only |
| D2 | `snb-map.sh SF...`, `snb_to_rdf.py` | CSV → RDF 1.2 N-Triples, variants A and B, and the CSV's counts (`counts.json`) |
| D3 | `snb-receipt.sh` | one `SHA256SUMS` over the set, checked in full; a `MANIFEST.md` |
| M0 | `pg-m0.sh SF` | each variant loaded, counted (`pg_hops m0`) and checked against the CSV's counts (`snb_check.py`) |
| M1 | `pg-m1.sh SF` | the hop micro-costs (`pg_hops m1`, nrese-engine's example): work counters (`MODE=counters`), then times and `perf stat` |

The steps run as fleet jobs on the hosts with the shared input `snb` ([../hosts.toml](../hosts.toml)
`[inputs]`): `/home/krarilotus/nrese-assets/ldbc-snb-20261030` on office-pc and phuoc-yu, each
host's copy made there and its receipt compared. Never copied into a worktree.

## The mapping (snb_to_rdf.py)

- A node is `<http://ldbcouncil.org/snb/{Type}/{id}>` with `rdf:type snb:{Type}`; a `type` cell
  (Place: City, Country, Continent; Organisation: University, Company) adds a second label.
- A node column is `snb:{column}`: `creationDate` `xsd:dateTime`, `birthday` `xsd:date`, `id`
  `xsd:long`, `length`, `classYear`, `workFrom` `xsd:int`, the rest plain strings, each in the
  datagen's lexical form. Person's `language` and `email` hold `;`-separated values, one triple
  each; an empty cell is no triple.
- An edge is `s snb:{label} o`: a node file's foreign-key column (`hasCreator`, `isLocatedIn`,
  `replyOf`, `containerOf` from the forum, `hasModerator`, `isPartOf`, `hasType`,
  `isSubclassOf`) or an edge file's row (`Src_label_Dst`).
- An edge with properties (every edge-file column besides its two ends: `creationDate` on every
  dynamic edge, `classYear`, `workFrom`) has a reifier `<…/e/{label}/{n}>`: `r rdf:reifies
  <<( s p o )>>` and `r snb:{column}` per property. `n` counts the label's edges (node files,
  then edge files, each in part order), so parallel edges keep a reifier each.
- **Variant A** reifies the edges with properties (every edge-file edge); **variant B** every edge,
  the foreign-key edges with a reifier that has no property: the uniform mapping, to measure.

`counts.json` holds, per node type and edge label, the rows and cells read and the triples written,
the largest id per type, the parallel edges per label and each date column's extremes with a
subject holding them. `snb_check.py` turns it into what a store must hold: per predicate the
statements less those a set merges (parallel base triples, a value repeated in one cell), the
reifier statements and triple terms, the nodes per class, and the probed extremes with their
values.

## The sets (receipts on the hosts; SHA-256)

| File | Lines | Bytes | SHA-256 |
|---|---:|---:|---|
| source/bi-sf1-composite-merged-fk.tar.zst | | 216,780,094 | a72938e244e6aa9d99632fcd5065e50c669ecf4d00f60bd162b266df4a7aba13 |
| source/bi-sf3-composite-merged-fk.tar.zst | | 658,253,052 | d3ee75fa41c516fec4013898eac66e5edbc2ccf110f15ece2cf500d14fe9ff52 |
| rdf/sf1/snb-a.nt | 55,098,022 | 8,724,445,706 | 4086747beb164e850d18368b8af09bd6f52c29d9b41d0ff705e33f0f842da347 |
| rdf/sf1/snb-b.nt | 63,816,695 | 10,899,103,763 | 0097c0dd0e700cb3f996804ef4473e05dd4e3ad51b72d182802a94920169cd80 |
| rdf/sf1/counts.json | | 8,613 | b083847163e7dcc0f0f78e4d61a82ec67b8643519f1653f4603a096e7a83faa4 |
| rdf/sf3/snb-a.nt | 162,950,658 | 25,998,181,876 | 0b87b1cd404c8323dc2fc8f0438178933cd68ed62a7f9e81326c8ebb8ca003a0 |
| rdf/sf3/snb-b.nt | 187,897,810 | 32,234,385,755 | f4d5d75b0d7b244e4cb5eb969a9f03ee3b66535ba9c5a3c70392fc41024fee04 |
| rdf/sf3/counts.json | | 8,634 | 3b68c21be477ca3eb9792e31bbe08371cf79bbf0dcc2ae2cafca3398e69f2ba7 |

Both hosts' copies hash alike (the mapping under Python 3.12 and 3.14). SNB has no parallel
edges at SF1 or SF3; the mapping keeps them where they occur (`test_snb_to_rdf.py`).

## M1's phases

`pg_hops m1` reads the store and never writes it; a store under updates is a copy that
`pg_hops update` gave three commits (a deletion of 1/256 of the statements, then two partial
re-insertions), each under a quarter of the run before, left in the log so the asserted stack
has four runs. Every phase runs `--reps` times in a seeded order; each repetition's answer is
checked against the first's and against every other access path to it before a time is kept.
The phases and the counters: the docs of `crates/nrese-engine/examples/pg_hops/`. Their reference
test, `crates/nrese-engine/tests/pg_hops_reference.rs`, checks answers and counters against a slow
walk over one run and over four runs with deletions.
