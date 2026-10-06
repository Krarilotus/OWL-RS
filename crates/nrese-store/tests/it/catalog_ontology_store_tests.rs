use crate::support::catalog_fixture_path;
use nrese_store::{QueryResultKind, StoreConfig, StoreService};

#[test]
fn store_preload_accepts_official_prov_turtle_fixture_with_relative_base_iri()
-> Result<(), Box<dyn std::error::Error>> {
    let service = StoreService::new(
        StoreConfig::in_memory().with_ontology(catalog_fixture_path("prov.ttl")),
    )?;

    let ask = service.execute_query_str(
        "PREFIX owl: <http://www.w3.org/2002/07/owl#>
         PREFIX prov: <http://www.w3.org/ns/prov#>
         ASK WHERE {
           prov:generated owl:inverseOf prov:wasGeneratedBy
         }",
    )?;

    assert_eq!(ask.kind, QueryResultKind::Boolean);
    assert!(String::from_utf8(ask.payload)?.contains("true"));
    Ok(())
}
