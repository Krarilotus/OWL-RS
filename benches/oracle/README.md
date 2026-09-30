# A second oracle: Jena

The differential tests (`crates/nrese-sparql/tests/native_differential_tests.rs`) compare NRESE's native executor with spareval, Oxigraph's evaluator, on the same data. Where both deviate from the standard in the same way, they agree, and the tests can't see it. This kit asks a second, independent engine: Apache Jena's ARQ answers the same queries over the same data, and every difference is either explained or reported.

```sh
benches/oracle/run.sh                     # dump, answer, compare; the report in benches/oracle/results/<date>.md
```

| Step | What |
|---|---|
| dump | `NRESE_ORACLE_DUMP=<dir>` makes the random-query, pushed-filter and computed-value tests write each dataset (`data.nq`, and `data.canonical.nq` with numbers in canonical lexical forms) and each compared query with NRESE's rows (`q<M>.rq`, `q<M>.nrese`, `q<M>.ordered`): about 9,400 queries over 270 datasets |
| answer | `jena/Oracle.java` (Jena 6.2.0, in `nrese-bench/jena-oracle`) answers every query on both copies of its dataset: `q<M>.jena`, `q<M>.jena-canonical` |
| compare | `compare.py`: rows as multisets (in order where the query orders every column), numbers by value, columns aligned by name; a query agrees if it matches Jena on either copy |

The nightly workflow (`.github/workflows/nightly-oracle.yml`) runs the same steps and keeps the report. It fails when a difference is left unexplained.

## How Jena is run

- **Without ARQ's optimiser.** Its filter-disjunction rewrite turns `FILTER(?d = <c> || …)` into a substitution of `?d`, also in a UNION branch where `?d` is unbound, and invents solutions. An oracle should give the standard's algebra.
- **On a graph that matches terms** (`GraphMemFactory.createDefaultGraphSameTerm`), not values.

## What the differences are

The last run (30 September 2026): 9,403 queries, 8,070 agree, 765 agree on the canonical copy, 568 differ for a known reason, none unexplained.

| Rule | Queries | Why the answers may differ |
|---|---|---|
| `order` | 119 | ORDER BY between incomparable values (literals of different types) is left to the implementation |
| `min-max` | 155 | MIN and MAX over incomparable values follow that order |
| `limit` | 81 | ORDER BY with LIMIT or OFFSET over incomparable values picks different rows |
| `relative-iri` | 138 | `IRI()` of a relative string: Jena resolves it against a `file:///` base; without a BASE, NRESE makes it an error |
| `date-functions` | 1 | `YEAR`, `MONTH`, … take `xsd:dateTime` in SPARQL; Jena also takes `xsd:date` |
| `path-duplicates` | 39 | **NRESE deviates:** path alternatives `(p\|q)` are a UNION in the standard, with duplicates; NRESE (and spareval) give each solution once |
| `lexical-forms` | 27 (and the 765 that agree only on the canonical copy) | **NRESE deviates:** `STR` of `"07"^^xsd:integer` is `"07"` in the standard, `"7"` in NRESE (and spareval), which compute string functions from the value |
| `zero-length` | 5 | **NRESE deviates:** zero-length paths from a term outside the graph (completion plan 4.3) |
| `jena-path-values` | 2 | **Jena deviates:** its property paths find `"1"^^xsd:int` for a bound `"01"^^xsd:integer`. `SELECT * { ?s <p> ?d . ?s (<q>\|<r>) ?d }` over `<s> <p> "01"^^xsd:integer . <s> <q> "1"^^xsd:int` gives one row; the plain join gives none |
| `jena-exists-literal-predicate` | 1 | **Jena deviates:** `FILTER NOT EXISTS { ?d ?c <o> . ?d <p> "x" }` with `?c` bound to a literal drops the row; with one pattern it keeps it. The substituted pattern matches nothing, so NOT EXISTS holds |

**Found and fixed:** decimal multiplication and division with a zero operand (`0 * 1.5`, `0 / 1.5`) were errors in `oxsdatatypes`, which both NRESE's executor and spareval use, so `BIND(?x * 1.5 AS ?k)` left `?k` unbound and joined it with everything. The workspace carries a patched copy (`vendor/oxsdatatypes/NRESE-PATCH.md`).

The three NRESE deviations are on the completion plan (4.3, 4.6). Once one is fixed, its rule goes, and with the lexical forms fixed, the canonical copy goes too. A rule that explains a difference only says the difference is one of a known kind; `compare.py` applies the narrowest test it can (the rows of one engine contained in the other's, the columns of MIN and MAX masked). The report lists every explained query, for review.

## Cleaning up

`run.sh` removes the dump when it ends. The images `nrese-bench/jena:6.2.0` and `nrese-bench/jena-oracle` are tools; `scripts/bench-cleanup.sh --tools` removes them.
