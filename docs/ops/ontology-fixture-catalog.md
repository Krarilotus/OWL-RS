# Ontology Fixture Catalog

## Purpose

This document defines the staged real-world ontology catalog used to harden NRESE and the Fuseki parity harness with non-synthetic inputs.

The catalog file is:

- `benches/nrese-bench-harness/fixtures/catalog/ontologies.toml`

The catalog is now also typed by processing metadata, not only by URL:

- serialization syntax used for ingest
- semantic dialect families that operators should expect to parse and preserve
- reasoning features that the ontology is useful for exercising
- service coverage areas that should be validated with that fixture

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
- `vcard`
  - source: `https://www.w3.org/2006/vcard/ns.ttl`
  - tier: `medium`
- `dcterms`
  - source: `https://www.dublincore.org/specifications/dublin-core/dcmi-terms/dublin_core_terms.ttl`
  - tier: `medium`
- `odrl`
  - source: `https://www.w3.org/ns/odrl/2/ODRL22.ttl`
  - tier: `broad`

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
  exercises transitive, inverse, and subproperty declarations in a widely used W3C thesaurus model
- `sosa` / `ssn`
  exercise property chains plus functional / inverse-functional declarations in the sensor-observation stack
- `dcat`
  adds another official W3C source with property-chain-heavy metadata semantics
- `vcard`
  adds another official W3C vocabulary that is widely exchanged as Turtle and useful for Graph Store and `tell` validation
- `dcterms`
  adds DCMI metadata terms with broad RDF ecosystem adoption and strong subproperty/domain-range coverage
- `odrl`
  adds a broader OWL-heavy vocabulary that is useful for unsupported-construct diagnostics and list/restriction handling

Current explicit service-validation focus:

- `foaf`
  - RDF/XML preload and `tell` ingest acceptance
  - subclass-driven `foaf:Person -> foaf:Agent` inference in `rules-mvp`
- `org`
  - Graph Store roundtrip on an official Turtle ontology
  - inverse-property plus domain/range reasoning on `org:memberOf` / `org:hasMember`
- `time`
  - official ontology-backed inverse/transitive property reasoning
- `skos`
  - RDF/XML ontology-backed `skos:broader -> skos:broaderTransitive` closure
  - inverse transitive closure on `skos:narrowerTransitive`
- `prov`
  - Turtle ontology preload with file-derived base IRI for official `prov.ttl`
  - subclass, inverse-property, and domain/range reasoning on `prov:Person`, `prov:wasGeneratedBy`, and `prov:generated`
- `dcat`
  - domain/range reasoning on `dcat:dataset`
- `vcard`
  - named-graph store roundtrip on an official Turtle ontology
- `odrl`
  - currently cataloged for unsupported-construct and list/restriction hardening, not yet used as a positive inference fixture

## Catalog Metadata

Each ontology entry in `ontologies.toml` must define:

- `serialization`
  - current typed values: `turtle`, `rdf-xml`
- `semantic_dialects`
  - current typed values include `rdfs`, `owl`, `foaf`, `org`, `time`, `prov-o`, `skos`, `sosa`, `ssn`, `dcat`, `vcard`, `dcmi-terms`, `odrl`
- `reasoning_features`
  - current typed values include `subclass-closure`, `subproperty-closure`, `domain-range-typing`, `inverse-property`, `transitive-property`, `symmetric-property`, `disjointness`, `identity`, `restrictions`, `list-axioms`
- `service_coverage`
  - current typed values include `catalog-sync`, `tell`, `graph-store`, `query`, `reasoner`, `benchmark`

The harness validates that these fields are present when reading the catalog, so the catalog remains a real source of truth instead of a loose download list.

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
- `benches/nrese-bench-harness/fixtures/packs/vcard-baseline/pack.toml`
- `benches/nrese-bench-harness/fixtures/packs/dcterms-baseline/pack.toml`
- `benches/nrese-bench-harness/fixtures/packs/odrl-baseline/pack.toml`

Example:

```powershell
cargo run --manifest-path benches/nrese-bench-harness/Cargo.toml -- pack `
  --nrese-base-url http://127.0.0.1:8080 `
  --fuseki-base-url http://127.0.0.1:3030/ds `
  --workload-pack benches/nrese-bench-harness/fixtures/packs/foaf-baseline/pack.toml `
  --report-dir artifacts/foaf-baseline
```
