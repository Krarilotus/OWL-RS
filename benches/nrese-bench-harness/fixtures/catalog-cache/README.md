# Vendored ontology fixtures

Copies of the published vocabularies listed in [`../catalog/ontologies.toml`](../catalog/ontologies.toml), committed so that tests are hermetic. Tests must not depend on network downloads or upstream changes; that dependency is how the SOSA/SSN mix-up went unnoticed until 2026-09-25.

- **Refresh deliberately** with `catalog-sync --refresh true`, review the diff, and re-run the tests. The catalog's `media_type` is sent as the `Accept` header, since several W3C namespaces use content negotiation.
- **Provenance and licences:** each file is an unmodified copy of its catalog `url`, redistributed under the original publisher's terms:
  - W3C vocabularies (ORG, Time, PROV-O, SKOS, SOSA, SSN, DCAT, vCard, ODRL): W3C Software and Document License
  - FOAF: Creative Commons Attribution 1.0
  - DCMI Metadata Terms: Creative Commons Attribution 4.0

Last full refresh: 2026-09-25.
