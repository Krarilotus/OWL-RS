//! A query may use the repository's namespace prefixes without declaring them, as GraphDB
//! and RDF4J repositories allow; its own declarations win, and the result cache keeps
//! queries apart that the same text means differently under other namespaces.

use nrese_store::{SparqlQueryRequest, StoreConfig, StoreService};

fn answer(store: &StoreService, query: &str) -> Result<String, nrese_store::StoreError> {
    let result = store.execute_query(&SparqlQueryRequest::all(query))?;
    Ok(String::from_utf8(result.payload).expect("utf-8"))
}

#[test]
fn undeclared_prefixes_are_the_repositorys() {
    // The result cache on: the same text twice must not answer from another namespace.
    let store = StoreService::new(StoreConfig {
        query_cache_bytes: 1 << 20,
        ..StoreConfig::in_memory()
    })
    .expect("store");
    store
        .execute_update_str(
            "INSERT DATA { <urn:one:a> <urn:one:p> \"one\" . <urn:two:a> <urn:two:p> \"two\" }",
        )
        .expect("data");
    let query = "SELECT ?o WHERE { ex:a ex:p ?o }";
    // Not bound: a syntax error, as without namespaces.
    assert!(matches!(
        answer(&store, query),
        Err(nrese_store::StoreError::SparqlSyntax(_))
    ));
    store.namespaces().set("ex", "urn:one:").expect("bound");
    assert!(answer(&store, query).expect("answered").contains("one"));
    // Bound elsewhere: the same text is another query, not the cached answer.
    store.namespaces().set("ex", "urn:two:").expect("rebound");
    let second = answer(&store, query).expect("answered");
    assert!(
        second.contains("two") && !second.contains("one"),
        "{second}"
    );
    // The query's own declaration wins.
    let declared = answer(&store, &format!("PREFIX ex: <urn:one:> {query}")).expect("answered");
    assert!(
        declared.contains("one") && !declared.contains("two"),
        "{declared}"
    );
    // A mistake of the query's own is reported as such.
    let error = answer(&store, "SELECT ?o WHERE { ex:a ex:p ?o").expect_err("unclosed");
    assert!(matches!(error, nrese_store::StoreError::SparqlSyntax(_)));
}

#[test]
fn updates_use_the_repositorys_prefixes_too() {
    let store = StoreService::new(StoreConfig::in_memory()).expect("store");
    assert!(
        store
            .execute_update_str("INSERT DATA { ex:b ex:p 3 }")
            .is_err()
    );
    store.namespaces().set("ex", "urn:three:").expect("bound");
    store
        .execute_update_str("INSERT DATA { ex:b ex:p 3 }")
        .expect("inserted");
    let found = answer(&store, "ASK { <urn:three:b> <urn:three:p> 3 }").expect("asked");
    assert!(found.contains("true"), "{found}");
}
