//! Results that may use RDF 1.2 announce it (RDF 1.2 Concepts §2.1; SPARQL 1.2 Query
//! Results JSON §3.1.3): `version=1.2` in the media type, `"version"` in JSON's head,
//! `VERSION "1.2"` first in Turtle and N-Triples. Results that can't don't.

use nrese_store::{
    GraphResultFormat, SerializedQueryResult, SparqlQueryRequest, StoreConfig, StoreService,
};

fn run(store: &StoreService, query: &str, graph: GraphResultFormat) -> SerializedQueryResult {
    let request = SparqlQueryRequest {
        graph_format: graph,
        ..SparqlQueryRequest::all(query)
    };
    store.execute_query(&request).expect("answered")
}

fn text(result: &SerializedQueryResult) -> String {
    String::from_utf8(result.payload.clone()).expect("utf-8")
}

#[test]
fn results_announce_rdf12_when_they_may_use_it() {
    let store = StoreService::new(StoreConfig::in_memory()).expect("store");
    store
        .execute_update_str("INSERT DATA { <urn:a> <urn:p> <urn:b> }")
        .expect("data");
    let select = "SELECT * WHERE { ?s ?p ?o }";
    // Nothing of RDF 1.2: nothing announced.
    let plain = run(&store, select, GraphResultFormat::Turtle);
    assert_eq!(plain.media_type, "application/sparql-results+json");
    assert!(!text(&plain).contains("version"), "{}", text(&plain));
    let construct = run(
        &store,
        "CONSTRUCT WHERE { ?s ?p ?o }",
        GraphResultFormat::Turtle,
    );
    assert_eq!(construct.media_type, "text/turtle");
    assert!(!text(&construct).contains("VERSION"));

    // A query that makes a triple term announces it, whatever the data.
    let made = run(
        &store,
        "SELECT ?t WHERE { ?s ?p ?o BIND(TRIPLE(?s, ?p, ?o) AS ?t) }",
        GraphResultFormat::Turtle,
    );
    assert_eq!(
        made.media_type,
        "application/sparql-results+json; version=1.2"
    );
    assert!(
        text(&made).starts_with("{\"head\":{\"vars\":[\"t\"],\"version\":\"1.2\"}"),
        "{}",
        text(&made)
    );

    // Once the data holds a triple term, every result may.
    store
        .execute_update_str("INSERT DATA { <urn:a> <urn:says> <<( <urn:x> <urn:y> <urn:z> )>> }")
        .expect("a triple term");
    let select = run(&store, select, GraphResultFormat::Turtle);
    assert_eq!(
        select.media_type,
        "application/sparql-results+json; version=1.2"
    );
    let ask = run(&store, "ASK { ?s ?p ?o }", GraphResultFormat::Turtle);
    assert!(text(&ask).contains("\"version\":\"1.2\""), "{}", text(&ask));
    for (format, media_type) in [
        (GraphResultFormat::Turtle, "text/turtle; version=1.2"),
        (
            GraphResultFormat::NTriples,
            "application/n-triples; version=1.2",
        ),
    ] {
        let graph = run(&store, "CONSTRUCT WHERE { ?s ?p ?o }", format);
        assert_eq!(graph.media_type, media_type);
        let document = text(&graph);
        assert!(document.starts_with("VERSION \"1.2\"\n"), "{document}");
        // What was written reads back.
        nrese_store::parse_payload(format, None, &graph.payload).expect("parses");
    }
}
