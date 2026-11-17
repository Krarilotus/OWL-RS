//! Results that may use RDF 1.2 announce it (RDF 1.2 Concepts §2.1; SPARQL 1.2 Query
//! Results JSON §3.1.3): `version=1.2` in the media type, `"version"` in JSON's head,
//! `VERSION "1.2"` first in Turtle and N-Triples. Results that can't don't.

use crate::support::Writes;
use nrese_store::{
    CancellationToken, GraphResultFormat, SerializedQueryResult, SparqlQueryRequest, StoreConfig,
    StoreService,
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
        nrese_store::parse_payload(
            format,
            None,
            &graph.payload,
            &nrese_exec::Envelope::unlimited(),
        )
        .expect("parses");
    }
}

/// What one run of a prepared query told, `header` the media type its report saw before
/// its first byte, `body` its answer: the announcement of each, and whether a triple term
/// was answered.
fn told(header: &str, body: &[u8]) -> String {
    let body = String::from_utf8_lossy(body);
    let version = |announced: bool| if announced { "1.2" } else { "none" };
    let header = version(header.contains("version=1.2"));
    let row = match body.contains("{\"type\":\"triple\"") {
        true => "the term's row",
        false => "no term",
    };
    let body = version(body.contains("\"version\":\"1.2\""));
    format!("header {header} body {body}, {row}")
}

/// Another run of a prepared query changes nothing of a run's announcement: run again (a
/// clone of it, inside the first run's report, after the data's only triple term was
/// deleted), the first run still announces the term it read, its header and its body
/// alike, and the second's header and body agree. With the result cache off and on.
#[test]
fn a_run_announces_what_it_read_whatever_another_run_of_its_query_reads() {
    let term = "<urn:s> <urn:p> <<( <urn:a> <urn:b> <urn:c> )>>";
    let mut wrong = Vec::new();
    for (cache, bytes) in [("no cache", 0), ("a cache", 64 << 20)] {
        let config = StoreConfig {
            query_cache_bytes: bytes,
            ..StoreConfig::in_memory()
        };
        let store = StoreService::new(config).expect("store");
        store
            .execute_update_str(&format!("INSERT DATA {{ {term} }}"))
            .expect("the term");
        let request = SparqlQueryRequest::all("SELECT * WHERE { ?s ?p ?o }");
        let first = store.prepare_query(&request).expect("prepared");
        let again = first.clone();
        let token = CancellationToken::new();
        let (mut header, mut body, mut header2, mut body2) = ("", Vec::new(), "", Vec::new());
        // The second run whole, inside the first's report: after the first decided its
        // announcement, before its first byte.
        let run = store.run_query_reporting(&first, &token, &mut body, |_, _| {
            header = first.media_type();
            let delete = format!("DELETE DATA {{ {term} }}");
            store.execute_update_str(&delete).expect("deleted");
            let second = store.run_query_reporting(&again, &token, &mut body2, |_, _| {
                header2 = again.media_type();
                None
            });
            second.expect("the second run");
            None
        });
        run.expect("the first run");
        // The first read a triple term: it announces it, both ways. The second read none,
        // and may announce either (as a query prepared before a delete always did), its
        // header and its body alike.
        let alike = [
            "header 1.2 body 1.2, no term",
            "header none body none, no term",
        ];
        for (run, got, want) in [
            (
                "the first run, the term read",
                told(header, &body),
                &["header 1.2 body 1.2, the term's row"][..],
            ),
            (
                "the second run, the term deleted",
                told(header2, &body2),
                &alike[..],
            ),
        ] {
            let cell = format!("{run}, {cache}");
            let want = want.join(" or ");
            match want.split(" or ").any(|w| w == got) {
                true => println!("CASE {cell}: right {got}"),
                false => {
                    println!("CASE {cell}: WRONG {got}, want {want}");
                    wrong.push(cell);
                }
            }
        }
    }
    assert!(
        wrong.is_empty(),
        "a run announced what another read: {wrong:?}"
    );
}

/// A result's media type, read when its documentation says it is final, announces what
/// its body announces: for a reader of every graph, from its preparation on; for a reader
/// of some graphs, from its run's report on (read after preparation it tells nothing of
/// the store, with a triple term in a graph that reader can't read or in one it can).
#[test]
fn a_media_type_read_when_it_is_final_announces_what_the_body_announces() {
    let term = "<urn:s> <urn:p> <<( <urn:a> <urn:b> <urn:c> )>>";
    let token = CancellationToken::new();
    let mut cells = Vec::new();
    let store = StoreService::new(StoreConfig::in_memory()).expect("store");
    store
        .execute_update_str(&format!("INSERT DATA {{ {term} }}"))
        .expect("the term");
    let every = SparqlQueryRequest::all("SELECT * WHERE { ?s ?p ?o }");
    let prepared = store.prepare_query(&every).expect("prepared");
    let header = prepared.media_type();
    let mut body = Vec::new();
    store.run_query(&prepared, &token, &mut body).expect("run");
    let want = "header 1.2 body 1.2, the term's row";
    let cell = "a reader of every graph, the media type read after prepare";
    cells.push((cell.to_owned(), told(header, &body), want));
    for (graph, where_term, at_report) in [
        (
            "urn:g/closed",
            "only in a graph it can't read",
            "header none body none, no term",
        ),
        (
            "urn:g/open",
            "in a graph it reads",
            "header 1.2 body 1.2, the term's row",
        ),
    ] {
        let store = StoreService::new(StoreConfig::in_memory()).expect("store");
        let insert = format!("INSERT DATA {{ GRAPH <{graph}> {{ {term} }} }}");
        store.execute_update_str(&insert).expect("the term");
        let access = nrese_sparql::GraphAccess {
            graphs: vec!["urn:g/open".to_owned()],
            ..nrese_sparql::GraphAccess::default()
        };
        let scope = nrese_store::ReadScope::Graphs(std::sync::Arc::new(access));
        let some = SparqlQueryRequest::new("SELECT * WHERE { GRAPH ?g { ?s ?p ?o } }", scope);
        let prepared = store.prepare_query(&some).expect("prepared");
        let after_prepare = prepared.media_type();
        let (mut header, mut body) = ("", Vec::new());
        let run = store.run_query_reporting(&prepared, &token, &mut body, |_, _| {
            header = prepared.media_type();
            None
        });
        run.expect("run");
        let reader = format!("a reader of some graphs, the term {where_term}");
        let told_at_report = told(header, &body);
        let cell = format!("{reader}, the media type read at the report");
        cells.push((cell, told_at_report, at_report));
        let after = match after_prepare.contains("version=1.2") {
            true => "header 1.2",
            false => "header none",
        };
        let cell = format!("{reader}, the media type read after prepare");
        cells.push((cell, after.to_owned(), "header none"));
    }
    let mut wrong = Vec::new();
    for (cell, got, want) in cells {
        match got == want {
            true => println!("CASE {cell}: right {got}"),
            false => {
                println!("CASE {cell}: WRONG {got}, want {want}");
                wrong.push(cell);
            }
        }
    }
    assert!(
        wrong.is_empty(),
        "a media type read when final told otherwise: {wrong:?}"
    );
}
