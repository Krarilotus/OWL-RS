//! Every read of the store takes a `ReadScope` (the audit of 2 October, §2.3): a restricted
//! scope sees its graphs only, whatever the caller forgets, and the operations over the
//! whole dataset refuse it.

mod support;

use std::sync::Arc;

use nrese_rdf::GraphName;
use nrese_sparql::GraphAccess;
use nrese_store::{
    DatasetBackupFormat, GraphReadRequest, GraphResultFormat, GraphTarget, ReadScope,
    SparqlQueryRequest, SparqlUpdateRequest, StatementPattern, StoreService,
};
use support::in_memory_store_config;

fn store() -> StoreService {
    let store = StoreService::new(in_memory_store_config()).unwrap();
    store
        .execute_update(&SparqlUpdateRequest::new(
            "INSERT DATA { <urn:a> <urn:p> 1 .
               GRAPH <urn:g:public> { <urn:b> <urn:p> 2 }
               GRAPH <urn:g:secret> { <urn:c> <urn:p> 3 } }",
        ))
        .unwrap();
    store
}

fn public() -> ReadScope {
    ReadScope::Graphs(Arc::new(GraphAccess {
        prefixes: vec!["urn:g:public".to_owned()],
        default_graph: true,
        ..GraphAccess::default()
    }))
}

#[test]
fn a_restricted_scope_reads_its_graphs_only() {
    let store = store();
    let scope = public();
    let quads = store
        .read_statements(&scope, &StatementPattern::default(), false)
        .unwrap();
    let graphs: Vec<GraphName> = quads.iter().map(|q| q.graph_name.clone()).collect();
    assert_eq!(quads.len(), 2, "{quads:?}");
    assert!(!graphs.iter().any(|g| g.to_string().contains("secret")));
    assert_eq!(
        store.count_statements(&scope, &StatementPattern::default(), false),
        2
    );
    assert_eq!(
        store.count_statements(&ReadScope::All, &StatementPattern::default(), false),
        3
    );
    let contexts: Vec<String> = store
        .contexts(&scope)
        .iter()
        .map(ToString::to_string)
        .collect();
    assert_eq!(contexts, ["<urn:g:public>"]);
    assert_eq!(store.graph_sizes(&scope).len(), 2);
    // A query: the dataset is restricted before anything is evaluated.
    let result = store
        .execute_query(&SparqlQueryRequest::new(
            "SELECT (COUNT(*) AS ?n) WHERE { GRAPH ?g { ?s ?p ?o } }",
            scope.clone(),
        ))
        .unwrap();
    let text = String::from_utf8(result.payload).unwrap();
    assert!(text.contains("\"1\""), "{text}");
    // A graph the scope doesn't read doesn't exist for it.
    let read = |scope: &ReadScope| {
        store
            .execute_graph_read(
                scope,
                &GraphReadRequest {
                    target: GraphTarget::NamedGraph("urn:g:secret".to_owned()),
                    format: GraphResultFormat::NTriples,
                },
            )
            .unwrap()
    };
    assert!(!read(&scope).exists);
    assert!(read(&ReadScope::All).exists);
}

#[test]
fn operations_over_the_whole_dataset_refuse_a_restricted_scope() {
    let store = store();
    let scope = public();
    assert!(store.autocomplete(&scope, "a", 5, true).is_err());
    assert!(store.autocomplete(&ReadScope::All, "a", 5, true).is_ok());
    assert!(
        store
            .export_dataset(&scope, DatasetBackupFormat::NQuads)
            .is_err()
    );
    assert!(store.classify(&scope).is_err());
    assert!(store.classify(&ReadScope::All).is_ok());
}
