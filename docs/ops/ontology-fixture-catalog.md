# Ontology Fixture Catalog

## Purpose

This document defines the initial real-world ontology catalog used to harden NRESE and the Fuseki parity harness with non-synthetic inputs.

The catalog file is:

- `benches/nrese-bench-harness/fixtures/catalog/ontologies.toml`

The current source set is intentionally staged from small to broader:

- `foaf`
  - source: `https://xmlns.com/foaf/spec/index.rdf`
  - tier: `small`
- `org`
  - source: `https://www.w3.org/ns/org.ttl`
  - tier: `medium`
- `time`
  - source: `https://www.w3.org/2006/time.ttl`
  - tier: `medium`
- `prov`
  - source: `https://www.w3.org/ns/prov.ttl`
  - tier: `broad`
- `skos`
  - source: `https://www.w3.org/2009/08/skos-reference/skos.rdf`
  - tier: `medium`
- `sosa`
  - source: `https://www.w3.org/ns/ssn/sosa.ttl`
  - tier: `medium`
- `ssn`
  - source: `https://www.w3.org/ns/ssn/ssn.ttl`
  - tier: `broad`
- `dcat`
  - source: `https://www.w3.org/ns/dcat.ttl`
  - tier: `medium`

Selection rationale:

- `foaf`
  exercises bounded inverse-functional / functional semantics and class disjointness
- `org`
  exercises property chains, transitive properties, and class/property structure
- `time`
  exercises transitive and functional semantics over a more formal temporal vocabulary
- `prov`
  exercises property chains, functional properties, and disjointness
- `skos`
  exercises transitive and symmetric vocabulary declarations in a widely used W3C thesaurus model
- `sosa` / `ssn`
  exercise property chains plus functional / inverse-functional declarations in the sensor-observation stack
- `dcat`
  adds another official W3C source with property-chain-heavy metadata semantics

## Sync Ontologies Locally

```powershell
cargo run --manifest-path benches/nrese-bench-harness/Cargo.toml -- catalog-sync
```

Sync only one tier:

```powershell
cargo run --manifest-path benches/nrese-bench-harness/Cargo.toml -- catalog-sync --tier small
```

Refresh existing cached files:

```powershell
cargo run --manifest-path benches/nrese-bench-harness/Cargo.toml -- catalog-sync --refresh true
```

Default cache output:

- `benches/nrese-bench-harness/fixtures/catalog-cache/`

## Use In Parity Runs

The catalog is the source of truth for real-world ontology inputs.

Recommended workflow:

1. sync a small ontology first
2. seed NRESE and Fuseki with the cached file
3. run compat and bench
4. then move to medium and broad ontologies

This keeps parity and performance work incremental instead of jumping straight to the largest ontology inputs.

Prebuilt workload packs:

- `benches/nrese-bench-harness/fixtures/packs/foaf-baseline/pack.toml`
- `benches/nrese-bench-harness/fixtures/packs/org-baseline/pack.toml`
- `benches/nrese-bench-harness/fixtures/packs/time-baseline/pack.toml`
- `benches/nrese-bench-harness/fixtures/packs/prov-baseline/pack.toml`
- `benches/nrese-bench-harness/fixtures/packs/skos-baseline/pack.toml`
- `benches/nrese-bench-harness/fixtures/packs/sosa-baseline/pack.toml`
- `benches/nrese-bench-harness/fixtures/packs/ssn-baseline/pack.toml`
- `benches/nrese-bench-harness/fixtures/packs/dcat-baseline/pack.toml`

Example:

```powershell
cargo run --manifest-path benches/nrese-bench-harness/Cargo.toml -- pack `
  --nrese-base-url http://127.0.0.1:8080 `
  --fuseki-base-url http://127.0.0.1:3030/ds `
  --workload-pack benches/nrese-bench-harness/fixtures/packs/foaf-baseline/pack.toml `
  --report-dir artifacts/foaf-baseline
```
