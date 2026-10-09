use super::*;
use nrese_engine::{Engine, EngineConfig};
use nrese_rdf_io::{RdfFormat, RdfParser};
use nrese_sparql::ql::{Closure, QlRewriting};
use std::sync::Arc;

#[test]
fn ql_status_passes_the_request_token_to_its_nested_probe_without_caching_failure() {
    let engine = Engine::new(EngineConfig::default()).unwrap();
    let data = "@prefix : <http://example.org/> .
        @prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .
        @prefix owl: <http://www.w3.org/2002/07/owl#> .
        :Employee rdfs:subClassOf [ a owl:Restriction ; owl:onProperty :worksFor ;
            owl:someValuesFrom :Organisation ] .
        :ann a :Employee ; :worksFor :acme .";
    let mut tx = engine.transaction();
    for q in RdfParser::from_format(RdfFormat::Turtle).for_reader(data.as_bytes()) {
        tx.insert(q.unwrap().as_ref());
    }
    tx.commit().unwrap();
    let snapshot = engine.snapshot();
    let prepared = PreparedQuery::parse(&SparqlQueryRequest::all(
        "SELECT ?x WHERE { ?x <http://example.org/worksFor> ?y }",
    ))
    .unwrap();
    let settings = StoreSettings::default();
    *settings.ql.write().unwrap() = Some(Arc::new(QlRewriting::new(Closure { lists: true })));
    let token = CancellationToken::new();
    token.cancel();
    let cancelled = ql_status(&snapshot, &prepared, &settings, &token).unwrap();
    assert_eq!(
        (cancelled.patterns, cancelled.realised, cancelled.checks),
        (1, 0, 1)
    );
    let token = CancellationToken::new();
    let successful = ql_status(&snapshot, &prepared, &settings, &token).unwrap();
    assert_eq!(
        (successful.patterns, successful.realised, successful.checks),
        (0, 1, 1)
    );
    let cached = ql_status(&snapshot, &prepared, &settings, &token).unwrap();
    assert_eq!((cached.realised, cached.checks), (1, 0));
}
