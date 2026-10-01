# Step 4a: nrese-sparql-results against sparesults

Run 1 October 2026 on the main development PC (AMD Ryzen 7 7800X3D, 16 threads, 64 GB,
Windows 11), release builds (fat LTO), 7 rounds each (median). `sparesults` 0.3.3 as
released, with `sparql-12` (MIT/Apache-2.0, so these numbers may be published).

## Conformance

- **Differential reading** (`cargo test --release --test results`): every `.srj`, `.srx`
  and `.tsv` file of the W3C SPARQL 1.1 and 1.2 suites (pinned in
  `scripts/fetch-w3c-tests.sh`), read by both: 372 read alike, 0 differ.
- **Round trips** (`nrese-sparql-results/tests`): JSON, XML and TSV, from a slice and
  through a reader that gives one byte per call, with every kind of term: escapes, edge
  whitespace, control characters, directional literals, nested triple terms, unbound
  values. The written bytes are those of `sparesults` (checked byte for byte in the
  benchmark).
- **Where nrese and `sparesults` write different CSV, nrese follows the specification.**
  - A triple term in CSV is `<<( s p o )>>` with its parts in TSV syntax (SPARQL 1.2
    CSV/TSV §2.2); `sparesults` writes the three parts without brackets or term syntax.
  - An IRI with a comma is quoted (RFC 4180); `sparesults` writes it bare, which splits
    the field.

## Throughput (`cargo run --release --bin results`)

200,000 generated solutions over five variables (fixed seed):
- IRIs and blank nodes;
- strings with escapes, markup characters and non-ASCII;
- language tags and typed literals;
- unbound values.

nrese writes each document once and both read the same bytes. Both sides get their terms
from parsing the same TSV, so the terms lie in memory alike. "Rows" is nrese's
`serialize_row` (values in variable order, as the engine writes them); "named" is the API
both have (pairs of variable and value). The time ratio is nrese's time divided by
`sparesults`' time, so a ratio below 1 means nrese is faster.

| Case | nrese MB/s | sparesults MB/s | ratio |
|---|---:|---:|---:|
| write JSON, rows | 976 | 295 | 0.30 |
| write JSON, named | 941 | 283 | 0.30 |
| write XML, rows | 946 | 278 | 0.29 |
| write XML, named | 921 | 276 | 0.30 |
| write TSV, rows | 685 | 522 | 0.76 |
| write TSV, named | 650 | 574 | 0.88 |
| write CSV, rows | 743 | 636 | 0.86 |
| write CSV, named | 675 | 615 | 0.91 |
| read JSON, slice | 195 | 115 | 0.59 |
| read JSON, reader (16 KiB reads) | 210 | 94 | 0.45 |
| read XML, slice | 172 | 137 | 0.80 |
| read XML, reader | 146 | 132 | 0.90 |
| read TSV, slice | 146 | 89 | 0.61 |
| read TSV, reader | 182 | 123 | 0.68 |

Ratios move by up to about 10% between runs: the machine also ran other work, and builds
run under `nice`.

## What it took

The first complete version was slower than `sparesults` in four cases:

| Case | Ratio | Cause | Fix |
|---|---:|---|---|
| CSV write | 1.22–1.45 | Every IRI was scanned for all four quoting characters | Only `,` is checked in IRIs (the only one an IRI can hold), with `memchr`; literals are checked with one bit test per byte that vectorises |
| TSV write by name | 1.11 | — | A row stays on the stack (up to 16 variables), and a name is first compared with the next variable in order |
| XML read | 1.20–1.26 | Every element allocated its name and each attribute | Element names became an enum; one reused attribute buffer; the text of a term is taken without a copy |
| JSON read | 0.67–0.87 (already faster) | Every key and `type` was copied | Keys and types are matched as borrowed strings. The tokenizer `nrese-json` now finds the end of a string in one pass over 8-byte words, where it made two passes before (`memchr2` for quotes and backslashes, then a check for control characters); the word search is tested against a byte search at every offset and alignment |

The comparison also showed a measurement trap. When nrese's solutions were generated
directly while Oxigraph's were parsed, nrese's terms lay scattered in memory, and every
write case was about 15% slower for that reason alone. Both sides now get their terms from
parsing the same TSV.
