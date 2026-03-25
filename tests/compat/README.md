# Compatibility Test Track

This folder anchors compatibility-oriented validation assets for NRESE vs Fuseki behavior.

Current executable harness:

- `benches/nrese-bench-harness` (run in `compat` mode)

Recommended usage:

1. Start NRESE and Fuseki with equivalent dataset content.
2. Run compatibility check:
   - `cargo run --manifest-path benches/nrese-bench-harness/Cargo.toml -- compat --nrese-base-url <NRESE> --fuseki-base-url <FUSEKI> --report-json artifacts/compat-report.json`
3. Track mismatches by case name and add dedicated fixtures in `fixtures/compat/`.

Supported comparators include:

- `ask-boolean`
- `solutions-count`
- `construct-triples-set` (canonicalized N-Triples set comparison)

This directory is reserved for additional conformance/compatibility assets as the suite expands.
