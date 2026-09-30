# oxsdatatypes 0.2.2, patched

The XSD datatypes crate of [Oxigraph](https://github.com/oxigraph/oxigraph) (`MIT OR Apache-2.0`, used here under MIT: `LICENSE-MIT`), version 0.2.2 as published on crates.io, with one fix. The workspace uses it in place of the published crate (`[patch.crates-io]` in `Cargo.toml` and in `benches/nrese-bench-harness/Cargo.toml`), so NRESE's native executor, spareval and the datatype checks compute alike.

**The fix.** `Decimal::checked_mul` and `Decimal::checked_div` strip trailing zeros from their operands and compute the shift that restores the scale of 18 fractional digits. When the operands carry fewer than 18 trailing zeros together, the shift was negative and the operation returned `None`, an error:

- a zero factor or dividend next to a number with a fractional part: `0 * 1.5`, `1.5 * 0`, `0 / 1.5`;
- a product with more than 18 fractional digits: `1e-18 * 1e-18`.

In SPARQL an error leaves the variable unbound, so `BIND(?x * 1.5 AS ?k)` with `?x = 0` joined `?k` with everything. Now zero operands give zero, and a product past 18 fractional digits is truncated. The division of a dividend too large to shift far enough multiplies back instead of failing. The tests `mul_nrese_patch` and `div_nrese_patch` in `src/decimal.rs` cover the cases.

Found by the Jena oracle (benches/oracle, 30 September 2026). Not yet reported upstream: that is the owner's decision. When spareval moves to a newer oxsdatatypes, check whether the fix is in it and drop this copy.

Changed: `src/decimal.rs` (the two functions and the two tests, marked "NRESE patch"). Everything else is the published crate.
