mod support;

use support::{assert_inferred_triple, run_rules_mvp_catalog_fixture};

#[test]
fn rules_mvp_infers_foaf_agent_from_official_foaf_fixture() -> Result<(), Box<dyn std::error::Error>>
{
    let inferred = run_rules_mvp_catalog_fixture(
        "foaf.rdf",
        "PREFIX foaf: <http://xmlns.com/foaf/0.1/>
         INSERT DATA { <http://example.com/alice> a foaf:Person . }",
    )?;

    assert_inferred_triple(
        &inferred,
        "http://example.com/alice",
        "http://www.w3.org/1999/02/22-rdf-syntax-ns#type",
        "http://xmlns.com/foaf/0.1/Agent",
    );
    Ok(())
}

#[test]
fn rules_mvp_infers_time_inverse_and_transitive_property_closure_from_official_fixture()
-> Result<(), Box<dyn std::error::Error>> {
    let inferred = run_rules_mvp_catalog_fixture(
        "time.ttl",
        "PREFIX time: <http://www.w3.org/2006/time#>
         INSERT DATA {
           <http://example.com/a> time:before <http://example.com/b> .
           <http://example.com/b> time:before <http://example.com/c> .
         }",
    )?;

    assert_inferred_triple(
        &inferred,
        "http://example.com/a",
        "http://www.w3.org/2006/time#before",
        "http://example.com/c",
    );
    assert_inferred_triple(
        &inferred,
        "http://example.com/b",
        "http://www.w3.org/2006/time#after",
        "http://example.com/a",
    );
    assert_inferred_triple(
        &inferred,
        "http://example.com/c",
        "http://www.w3.org/2006/time#after",
        "http://example.com/b",
    );
    Ok(())
}

#[test]
fn rules_mvp_infers_org_inverse_and_domain_range_types_from_official_fixture()
-> Result<(), Box<dyn std::error::Error>> {
    let inferred = run_rules_mvp_catalog_fixture(
        "org.ttl",
        "PREFIX org: <http://www.w3.org/ns/org#>
         INSERT DATA {
           <http://example.com/alice> org:memberOf <http://example.com/org1> .
         }",
    )?;

    assert_inferred_triple(
        &inferred,
        "http://example.com/org1",
        "http://www.w3.org/ns/org#hasMember",
        "http://example.com/alice",
    );
    assert_inferred_triple(
        &inferred,
        "http://example.com/alice",
        "http://www.w3.org/1999/02/22-rdf-syntax-ns#type",
        "http://xmlns.com/foaf/0.1/Agent",
    );
    assert_inferred_triple(
        &inferred,
        "http://example.com/org1",
        "http://www.w3.org/1999/02/22-rdf-syntax-ns#type",
        "http://www.w3.org/ns/org#Organization",
    );
    Ok(())
}

#[test]
fn rules_mvp_infers_skos_transitive_superproperty_and_inverse_closure_from_official_fixture()
-> Result<(), Box<dyn std::error::Error>> {
    let inferred = run_rules_mvp_catalog_fixture(
        "skos.rdf",
        "PREFIX skos: <http://www.w3.org/2004/02/skos/core#>
         INSERT DATA {
           <http://example.com/c1> skos:broader <http://example.com/c2> .
           <http://example.com/c2> skos:broader <http://example.com/c3> .
         }",
    )?;

    assert_inferred_triple(
        &inferred,
        "http://example.com/c1",
        "http://www.w3.org/2004/02/skos/core#broaderTransitive",
        "http://example.com/c2",
    );
    assert_inferred_triple(
        &inferred,
        "http://example.com/c2",
        "http://www.w3.org/2004/02/skos/core#broaderTransitive",
        "http://example.com/c3",
    );
    assert_inferred_triple(
        &inferred,
        "http://example.com/c1",
        "http://www.w3.org/2004/02/skos/core#broaderTransitive",
        "http://example.com/c3",
    );
    assert_inferred_triple(
        &inferred,
        "http://example.com/c2",
        "http://www.w3.org/2004/02/skos/core#narrowerTransitive",
        "http://example.com/c1",
    );
    assert_inferred_triple(
        &inferred,
        "http://example.com/c3",
        "http://www.w3.org/2004/02/skos/core#narrowerTransitive",
        "http://example.com/c1",
    );
    Ok(())
}

#[test]
fn rules_mvp_infers_prov_subclass_inverse_and_domain_range_from_official_fixture()
-> Result<(), Box<dyn std::error::Error>> {
    let inferred = run_rules_mvp_catalog_fixture(
        "prov.ttl",
        "PREFIX prov: <http://www.w3.org/ns/prov#>
         INSERT DATA {
           <http://example.com/x> a prov:Person .
           <http://example.com/entity> prov:wasGeneratedBy <http://example.com/activity> .
         }",
    )?;

    assert_inferred_triple(
        &inferred,
        "http://example.com/x",
        "http://www.w3.org/1999/02/22-rdf-syntax-ns#type",
        "http://www.w3.org/ns/prov#Agent",
    );
    assert_inferred_triple(
        &inferred,
        "http://example.com/activity",
        "http://www.w3.org/ns/prov#generated",
        "http://example.com/entity",
    );
    assert_inferred_triple(
        &inferred,
        "http://example.com/entity",
        "http://www.w3.org/1999/02/22-rdf-syntax-ns#type",
        "http://www.w3.org/ns/prov#Entity",
    );
    assert_inferred_triple(
        &inferred,
        "http://example.com/activity",
        "http://www.w3.org/1999/02/22-rdf-syntax-ns#type",
        "http://www.w3.org/ns/prov#Activity",
    );
    Ok(())
}

#[test]
fn rules_mvp_infers_dcat_domain_and_range_types_from_official_fixture()
-> Result<(), Box<dyn std::error::Error>> {
    let inferred = run_rules_mvp_catalog_fixture(
        "dcat.ttl",
        "PREFIX dcat: <http://www.w3.org/ns/dcat#>
         INSERT DATA {
           <http://example.com/catalog> dcat:dataset <http://example.com/dataset1> .
         }",
    )?;

    assert_inferred_triple(
        &inferred,
        "http://example.com/catalog",
        "http://www.w3.org/1999/02/22-rdf-syntax-ns#type",
        "http://www.w3.org/ns/dcat#Catalog",
    );
    assert_inferred_triple(
        &inferred,
        "http://example.com/dataset1",
        "http://www.w3.org/1999/02/22-rdf-syntax-ns#type",
        "http://www.w3.org/ns/dcat#Dataset",
    );
    Ok(())
}
