# A second oracle: Jena

The differential tests (`crates/nrese-sparql/tests/it/native_differential_tests.rs`)
compare NRESE's native executor with `nrese-sparql-reference` on the same data. The
reference independently evaluates algebra but shares expression and aggregate-value
helpers; agreement cannot independently validate those helpers. This kit asks a second,
independent engine: Apache Jena's ARQ answers the same queries over the same data, and
every difference is either explained or reported. The dated results below retain their
historical evaluator and dependency names.

```sh
benches/oracle/run.sh                     # dump, answer, compare; the report in benches/oracle/results/<date>.md
```

| Step | What |
|---|---|
| dump | `NRESE_ORACLE_DUMP=<dir>` makes the random-query, pushed-filter and computed-value tests write each dataset (`data.nq`, and `data.canonical.nq` with numbers in canonical lexical forms) and each compared query with NRESE's rows (`q<M>.rq`, `q<M>.nrese`, `q<M>.ordered`): about 9,400 queries over 270 datasets |
| answer | `jena/Oracle.java` (Jena 6.2.0, in `nrese-bench/jena-oracle`) answers every query on both copies of its dataset: `q<M>.jena`, `q<M>.jena-canonical` |
| compare | `compare.py`: rows as multisets (in order where the query orders every column), terms as the answer contract compares them (`suite/suitekit/answers.py`: the same datatype and value), columns aligned by name; a query agrees if it matches Jena on either copy. Blank nodes, decimal division without an oracle and numbers equal only across datatypes are reported apart (`blank-nodes`, `division`, `cross-type`), never as agreement |

The oracle workflow (`.github/workflows/oracle.yml`, run at milestones with `gh workflow run oracle.yml --ref <branch>`) runs the same steps and keeps the report. It fails when a difference is left unexplained.

## How Jena is run

- **Without ARQ's optimiser.** An oracle should give the standard's algebra, not an optimiser's rewrites of it.
- **On a graph that matches terms** (`GraphMemFactory.createDefaultGraphSameTerm`), not values.

## What the differences are

The last run (2 October 2026): 9,390 queries, 9,077 agree, 313 differ for a known reason, none unexplained. Between 1 and 2 October the workflow compared nothing: it named the tests as they were before the Oxigraph migration renamed them, found none, and passed; `compare.py` now fails when no query was compared. Its first real run found 61 unexplained differences, 59 of them one NRESE bug (below).

| Rule | Queries | Why the answers may differ |
|---|---|---|
| `order` | 119 | ORDER BY between incomparable values (literals of different types) is left to the implementation |
| `min-max` | 155 | MIN and MAX over incomparable values follow that order |
| `limit` | 81 | ORDER BY with LIMIT or OFFSET over incomparable values picks different rows |
| `relative-iri` | 138 | `IRI()` of a relative string: Jena resolves it against a `file:///` base; without a BASE, NRESE makes it an error |
| `date-functions` | 1 | `YEAR`, `MONTH`, … take `xsd:dateTime` in SPARQL; Jena also takes `xsd:date` |
| `path-duplicates` | 39 | **Fixed 2 October 2026:** path alternatives `(p\|q)` are a UNION in the standard, with duplicates; NRESE gave each solution once and now follows the standard |
| `lexical-forms` | 27 (and the 765 that agree only on the canonical copy) | **NRESE deviates:** `STR` of `"07"^^xsd:integer` is `"07"` in the standard, `"7"` in NRESE, which computes string functions from the value |
| `zero-length` | 5 | **NRESE deviates:** zero-length paths from a term outside the graph (completion plan 4.3) |
| `jena-path-values` | 2 | A property path from a bound `"01"^^xsd:integer` and the same value written as `"1"^^xsd:int`: the reference matches values, NRESE matches terms, as a plain join does |
| `jena-path-values-aggregated` | 3 | As `jena-path-values`, under an aggregate (a count one higher), where the rows don't show it. Explained only when NRESE on the canonical copy (`q<M>.nrese-canonical`) agrees with the reference there, the reference answers both copies alike (it matched by value) and NRESE doesn't (it matched terms) |
| `date-timezones` | 3 | `=` and `!=` between a date or dateTime with a timezone and one without: NRESE's answer has every row of the reference's, and its extra rows all compare the two kinds |
| `jena-exists-literal-predicate` | 1 | `FILTER NOT EXISTS { ?d ?c <o> . ?d <p> "x" }` with a variable predicate (`?c`) bound to a literal: NRESE's answer has every row of the reference's, and more |

**Found and fixed (2 October):** an integer-derived literal outside its datatype's range (`"300"^^xsd:byte`) is ill-typed, but NRESE's executor and its reference evaluator took it for the number 300, so `?b * 1.5`, `?b + 1`, `SUM`, `MIN`, `MAX` and comparisons computed with it where the standard (and Jena) have an error. The value ranges of the thirteen derived types are checked now.

**Found and fixed (30 September):** decimal multiplication and division with a zero operand (`0 * 1.5`, `0 / 1.5`) were errors in NRESE's executor and its reference evaluator, so `BIND(?x * 1.5 AS ?k)` left `?k` unbound and joined it with everything. Fixed then; the current workspace computes them in `crates/rdf/nrese-xsd`.

The three NRESE deviations are on the completion plan (4.3, 4.6). Once one is fixed, its rule goes, and with the lexical forms fixed, the canonical copy goes too. A rule that explains a difference only says the difference is one of a known kind; `compare.py` applies the narrowest test it can (the rows of one engine contained in the other's, the columns of MIN and MAX masked). The report lists every explained query, for review.

## Cleaning up

`run.sh` removes the dump when it ends. The images `nrese-bench/jena:6.2.0` and `nrese-bench/jena-oracle` are tools; `scripts/bench-cleanup.sh --tools` removes them.
