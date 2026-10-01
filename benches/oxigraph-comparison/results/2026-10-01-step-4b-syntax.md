# Step 4b: nrese-sparql-syntax against spargebra

Run 1 October 2026 on the main development PC (AMD Ryzen 7 7800X3D, 16 threads, 64 GB,
Windows 11), release builds (fat LTO), 9 rounds each (median). `spargebra` 0.4.6 as
released, with `sparql-12`, `sep-0002` and `sep-0006` (MIT/Apache-2.0, so these numbers may
be published).

## Conformance

- **W3C syntax tests** (`nrese-sparql-syntax/tests/w3c_syntax`, suites pinned in
  `scripts/fetch-w3c-tests.sh`). All 1,289 pass; no expected failures are listed.

  | Suite | Query, positive | Query, negative | Update, positive | Update, negative |
  |---|---:|---:|---:|---:|
  | SPARQL 1.0 | 423/423 | 50/50 | – | – |
  | SPARQL 1.1 | 363/363 | 47/47 | 128/128 | 13/13 |
  | SPARQL 1.2 | 161/161 | 81/81 | 21/21 | 2/2 |

  The queries and updates of the evaluation tests count as positive syntax tests. Every
  query and update that parses is also written back and parsed again, and comes back as
  the same algebra.
- **Differential against spargebra** (`cargo test --release --test syntax`). Every `.rq`
  and `.ru` file of the three suites and of the repository is parsed by both, and
  spargebra's algebra is converted into nrese's. Before comparing, the generated names
  are numbered by first appearance and `-(1)` is folded into the literal `-1`.

  | Outcome | Files |
  |---|---:|
  | Same algebra | 1,189 |
  | Both reject | 198 |
  | Explained differences | 8 |

  In each of the 8, nrese follows the specification or the W3C tests:

  | File | What happens |
  |---|---|
  | `sparql10/syntax-sparql3/syn-bad-26.rq` | W3C negative test, accepted by spargebra: by the longest-token rule, `<?a&&?b>` is an IRI, not `<` and `&&`. |
  | `sparql12/syntax/nested-aggregate-functions.rq` | W3C negative test, accepted by spargebra: an aggregate inside an aggregate. |
  | `sparql12/syntax-triple-terms-negative/tripleterm-subject-03.rq` | W3C negative test, accepted by spargebra: a triple term as the subject of a triple term in an expression. |
  | `sparql12/syntax-triple-terms-negative/tripleterm-subject-06.rq` | W3C negative test, accepted by spargebra: a literal as that subject. |
  | `sparql12/grouping/select-variable-reuse.rq` | W3C SPARQL 1.2 test, rejected by spargebra: a select expression uses an earlier one's variable. |
  | `sparql10/expr-builtin/case-insensitive-booleans.rq` | W3C test, rejected by spargebra: `TRUE` (keywords are case-insensitive, SPARQL 1.1 §19.3). |
  | `sparql10/expr-ops/query-add-literals.rq` | spargebra reads `a + b + c` as `a + (b + c)`. For `-` and `/` this changes the value: spargebra gives 7 for `10 - 5 - 2`, nrese gives 3. |
  | `sparql10/i18n/normalization-02.rq` | An absolute IRI with dot segments: nrese resolves it as RFC 3986 §5.2.2 says (as nrese-rdf-io does for data); oxiri keeps the segments. |
- **Own tests** (`nrese-sparql-syntax/tests/parser_tests.rs`):
  - left associativity, and signed literals;
  - deterministic generated names, kept clear of the query's own names;
  - the nesting limit, and the stack it needs;
  - where aggregates may stand;
  - switching SPARQL 1.2 and `LATERAL` off;
  - error positions;
  - writer exactness for the shapes federation sends.

## Parse and write time (`cargo run --release --bin syntax`)

The W3C corpus is the 951 queries and 149 updates both parsers accept. The generated
queries are:
- a BGP of 6,000 triple patterns (2,000 subjects with three properties each);
- a `VALUES` block of 20,000 rows;
- a `FILTER` of 3,000 alternatives.

The time ratio is nrese's time divided by spargebra's, so a ratio below 1 means nrese is
faster. Ratios over three runs:

| Case | nrese MB/s | spargebra MB/s | ratio |
|---|---:|---:|---:|
| parse W3C queries | 52 | 28 | 0.55–0.57 |
| parse W3C updates | 30 | 20 | 0.63–0.66 |
| parse BGP of 6,000 patterns | 34 | 14 | 0.38–0.40 |
| parse VALUES of 20,000 rows | 152 | 69 | 0.45–0.47 |
| parse FILTER of 3,000 alternatives | 56 | 19 | 0.35 |
| write W3C queries | 255 | 135–235 | 0.53–0.79 |
| write BGP of 6,000 patterns | 324 | 235 | 0.72–0.75 |

Writing is algebra to SPARQL text, which federation does for each `SERVICE` call.

## What it took

- **One pass, no backtracking.** Each rule dispatches on the next byte or word. The PEG
  of spargebra tries alternatives in order, and re-parses after each failed one.
- **Duplicate removal for `SELECT *` in a hash set.** The first version, like spargebra,
  removed duplicate in-scope variables with a list search. That made each distinct
  variable cost about 300 ns in a 6,000-pattern BGP, quadratic in the number of variables.
- **No copies of subjects and predicates.** The last triple of a subject takes the subject
  instead of copying it.
- **The writer outputs terms itself** instead of going through `write!` patterns. The
  literal quoting of `nrese-rdf`, used by every N-Triples-style output, now writes
  unescaped runs in one piece instead of one character at a time. The new quoting is
  tested against the character-by-character definition on all combinations of escaped,
  multi-byte and plain pieces.

  Before these two changes, writing took 1.07× (corpus) and 1.76× (BGP) of spargebra's
  time.
- **Stack.** At the default nesting limit (128), a release build needs at most about
  330 KiB of stack: bracketed expressions 327 KiB, groups 263 KiB, `EXISTS` 263 KiB,
  paths 135 KiB. So the limit is safe on a 1 MiB stack (Windows' main thread). A debug
  build needs up to 2.2 MiB. Deeper input is an error, not a crash.
