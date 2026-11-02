//! Q1 gate (store side): queries stream into their writer, stop promptly when cancelled,
//! and honour protocol dataset parameters.

use crate::support::Writes;
use std::io::{self, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc};
use std::time::Duration;

use nrese_store::{
    CancellationToken, PreparedQuery, QueryEvaluationError, SparqlQueryRequest, StoreConfig,
    StoreError, StoreService,
};

fn service_with_triples(count: usize) -> StoreService {
    let service = StoreService::new(StoreConfig::in_memory()).expect("store");
    let triples: String = (0..count)
        .map(|i| {
            format!(
                "<http://example.com/s{i}> <http://example.com/p{}> {i} .\n",
                i % 5
            )
        })
        .collect();
    service
        .execute_update_str(&format!("INSERT DATA {{ {triples} }}"))
        .expect("insert");
    service
}

/// A three-way cross product: about 10¹⁰ rows for 2,000 triples. Never finishes on its own.
const RUNAWAY: &str = "SELECT (COUNT(*) AS ?n) WHERE { ?a ?b ?c . ?d ?e ?f . ?g ?h ?i }";

fn is_cancelled(result: &Result<(), StoreError>) -> bool {
    matches!(
        result,
        Err(StoreError::SparqlEvaluation(
            QueryEvaluationError::Cancelled
        ))
    )
}

/// A query cancelled as the store hands it to the executor, past the store's own checks,
/// stops and reports itself cancelled; the store is unchanged and takes the next write.
/// Without cancellation this query runs for hours.
#[test]
fn cancelling_stops_a_query_in_the_executor() {
    // Not a latency claim: the longest the test waits before it fails instead of hanging.
    const DEADLINE: Duration = Duration::from_secs(60);
    let service = Arc::new(service_with_triples(2_000));
    let revision = service.current_revision();
    let token = CancellationToken::new();
    let entered = Arc::new(AtomicBool::new(false));
    let mut prepared = PreparedQuery::parse(&SparqlQueryRequest::all(RUNAWAY)).expect("parse");
    prepared.on_evaluation({
        let (token, entered) = (token.clone(), Arc::clone(&entered));
        move || {
            entered.store(true, Ordering::Release);
            token.cancel();
        }
    });
    let (done, finished) = mpsc::channel();
    // Not scoped: a query that doesn't stop must fail the test, not hang it.
    std::thread::spawn({
        let (service, token) = (Arc::clone(&service), token.clone());
        move || done.send(service.run_query(&prepared, &token, io::sink()))
    });
    let result = finished.recv_timeout(DEADLINE);
    token.cancel();
    let result = result.expect("the query stopped");
    assert!(
        entered.load(Ordering::Acquire),
        "the query reached the executor"
    );
    assert!(is_cancelled(&result), "{result:?}");
    assert_eq!(service.current_revision(), revision);
    service
        .execute_update_str("INSERT DATA { <http://example.com/after> <http://example.com/p> 1 }")
        .expect("the writer is free");
    assert_ne!(service.current_revision(), revision);
}

/// Cancels the token once `limit` bytes have been written: cancelling mid-output stops the
/// rest of the delivery. Whether the evaluation streams (output before it finishes) is the
/// executor's to show; this writer can't tell.
struct CancelAfter<'a> {
    written: usize,
    limit: usize,
    token: &'a CancellationToken,
}

impl Write for CancelAfter<'_> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.written += bytes.len();
        if self.written >= self.limit {
            self.token.cancel();
        }
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[test]
fn cancelling_mid_output_stops_the_delivery() {
    let service = service_with_triples(2_000);
    let query = "SELECT * WHERE { ?a ?b ?c . ?d ?e ?f }"; // 4 M rows
    let prepared = PreparedQuery::parse(&SparqlQueryRequest::all(query)).expect("parse");
    let token = CancellationToken::new();
    let mut out = CancelAfter {
        written: 0,
        limit: 256 * 1024,
        token: &token,
    };
    let result = service.run_query(&prepared, &token, &mut out);
    assert!(is_cancelled(&result), "{result:?}");
    assert!(
        out.written < 1024 * 1024,
        "stopped soon after cancelling: {} bytes",
        out.written
    );
}

#[test]
fn protocol_dataset_parameters_replace_from_clauses() {
    let service = StoreService::new(StoreConfig::in_memory()).expect("store");
    service
        .execute_update_str(
            "INSERT DATA {
               GRAPH <http://example.com/g1> { <http://example.com/a> <http://example.com/p> 1 }
               GRAPH <http://example.com/g2> { <http://example.com/b> <http://example.com/p> 2 }
             }",
        )
        .expect("insert");
    let count = |request: SparqlQueryRequest| {
        let result = service.execute_query(&request).expect("query");
        String::from_utf8(result.payload)
            .unwrap()
            .matches("\"value\"")
            .count()
    };
    let query = "SELECT ?s FROM <http://example.com/g1> WHERE { ?s ?p ?o }";
    assert_eq!(count(SparqlQueryRequest::all(query)), 1);

    let mut request = SparqlQueryRequest::all(query);
    request.default_graphs = vec![
        "http://example.com/g1".to_owned(),
        "http://example.com/g2".to_owned(),
    ];
    assert_eq!(count(request), 2, "the protocol dataset replaces FROM");

    let mut request = SparqlQueryRequest::all("SELECT ?s WHERE { GRAPH ?g { ?s ?p ?o } }");
    request.named_graphs = vec!["http://example.com/g2".to_owned()];
    assert_eq!(count(request), 1);

    let mut invalid = SparqlQueryRequest::all(query);
    invalid.default_graphs = vec!["not an iri".to_owned()];
    let error = service.execute_query(&invalid).unwrap_err();
    assert!(error.is_request_error(), "{error}");
}

/// `union_default_graph`: a query or an update `WHERE` without a dataset reads the merge
/// of all graphs, where a statement counts once however many graphs hold it. `GRAPH`
/// patterns and explicit datasets keep their meaning.
#[test]
fn the_default_graph_can_be_the_merge_of_all_graphs() {
    const DATA: &str = "INSERT DATA {
        <http://example.com/a> <http://example.com/p> 1 .
        GRAPH <http://example.com/g1> {
          <http://example.com/a> <http://example.com/p> 1 .
          <http://example.com/b> <http://example.com/p> 2 }
        GRAPH <http://example.com/g2> {
          <http://example.com/b> <http://example.com/p> 2 .
          <http://example.com/c> <http://example.com/p> 3 }
      }";
    let service = |union: bool| {
        let service = StoreService::new(StoreConfig {
            union_default_graph: union,
            ..StoreConfig::in_memory()
        })
        .expect("store");
        service.execute_update_str(DATA).expect("data");
        service
    };
    // The values of ?s, in TSV without the header, sorted.
    let subjects = |service: &StoreService, query: &str| -> Vec<String> {
        let mut request = SparqlQueryRequest::all(query);
        request.solutions_format = nrese_store::SolutionsResultFormat::Tsv;
        let result = service.execute_query(&request).expect("query");
        let text = String::from_utf8(result.payload).expect("utf8");
        let mut rows: Vec<String> = text
            .lines()
            .skip(1)
            .map(|line| {
                line.trim_start_matches("<http://example.com/")
                    .trim_end_matches('>')
                    .to_owned()
            })
            .collect();
        rows.sort();
        rows
    };
    let plain = "SELECT ?s WHERE { ?s <http://example.com/p> ?o }";

    let own = service(false);
    assert_eq!(subjects(&own, plain), ["a"]);

    let union = service(true);
    // Three statements, each once: `a` is in two graphs, `b` in two.
    assert_eq!(subjects(&union, plain), ["a", "b", "c"]);
    // Joins, filters and aggregates read the same merge.
    assert_eq!(
        subjects(
            &union,
            "SELECT ?s WHERE { ?s <http://example.com/p> ?o . ?s ?q ?o FILTER(?o > 1) }"
        ),
        ["b", "c"]
    );
    assert_eq!(
        subjects(&union, "SELECT (COUNT(*) AS ?s) WHERE { ?x ?y ?z }"),
        ["3"]
    );
    // `GRAPH` still names graphs: every copy, per graph; the default graph isn't one.
    assert_eq!(
        subjects(&union, "SELECT ?s WHERE { GRAPH ?g { ?s ?p ?o } }"),
        ["a", "b", "b", "c"]
    );
    assert_eq!(
        subjects(
            &union,
            "SELECT ?s WHERE { GRAPH <http://example.com/g2> { ?s ?p ?o } }"
        ),
        ["b", "c"]
    );
    // A query that names its dataset gets that dataset.
    assert_eq!(
        subjects(
            &union,
            "SELECT ?s FROM <http://example.com/g1> WHERE { ?s ?p ?o }"
        ),
        ["a", "b"]
    );
    let mut protocol = SparqlQueryRequest::all(plain);
    protocol.solutions_format = nrese_store::SolutionsResultFormat::Tsv;
    protocol.default_graphs = vec!["http://example.com/g2".to_owned()];
    let text = String::from_utf8(union.execute_query(&protocol).expect("query").payload).unwrap();
    assert_eq!(text.lines().count(), 3, "{text}");

    // An update's WHERE reads the merge too; what it writes without `GRAPH` goes to the
    // default graph.
    let mark =
        "INSERT { ?s <http://example.com/seen> true } WHERE { ?s <http://example.com/p> ?o }";
    union.execute_update_str(mark).expect("update");
    own.execute_update_str(mark).expect("update");
    let seen = "SELECT ?s WHERE { ?s <http://example.com/seen> true }";
    assert_eq!(subjects(&union, seen), ["a", "b", "c"]);
    assert_eq!(subjects(&own, seen), ["a"]);
}

/// Jena's and Fuseki's name for the default graph, `urn:x-arq:DefaultGraph`, which clients
/// written for them use (rdflib's SPARQL store writes a graph so named into
/// `GRAPH <urn:x-arq:DefaultGraph> { … }`): the default graph in update data, in `GRAPH`
/// patterns and in update templates.
#[test]
fn jenas_default_graph_name_is_the_default_graph() {
    let service = StoreService::new(StoreConfig::in_memory()).expect("store");
    let ask = |query: &str| {
        let result = service.execute_query_str(query).expect("query");
        String::from_utf8(result.payload).unwrap().contains("true")
    };
    service
        .execute_update_str(
            "INSERT DATA { GRAPH <urn:x-arq:DefaultGraph> { <http://e/a> <http://e/p> <http://e/b> } }",
        )
        .expect("insert");
    assert!(
        ask("ASK { <http://e/a> <http://e/p> <http://e/b> }"),
        "in the default graph"
    );
    assert!(!ask("ASK { GRAPH ?g { ?s ?p ?o } }"), "no named graph made");
    assert!(ask(
        "ASK { GRAPH <urn:x-arq:DefaultGraph> { <http://e/a> <http://e/p> <http://e/b> } }"
    ));
    service
        .execute_update_str(
            "DELETE { GRAPH <urn:x-arq:DefaultGraph> { ?s ?p ?o } } \
             INSERT { GRAPH <urn:x-arq:DefaultGraph> { ?o ?p ?s } } \
             WHERE { GRAPH <urn:x-arq:DefaultGraph> { ?s ?p ?o } }",
        )
        .expect("delete/insert");
    assert!(
        ask("ASK { <http://e/b> <http://e/p> <http://e/a> }"),
        "templates too"
    );
    assert!(!ask("ASK { <http://e/a> <http://e/p> <http://e/b> }"));
    assert!(!ask("ASK { GRAPH ?g { ?s ?p ?o } }"));
}

/// Jena's name for the default graph as the query's dataset (`FROM`, as rdflib's SPARQL
/// store sends it): the store's default graph.
#[test]
fn jenas_default_graph_name_as_the_dataset() {
    let service = StoreService::new(StoreConfig::in_memory()).expect("store");
    service
        .execute_update_str(
            "INSERT DATA { <http://e/a> <http://e/p> <http://e/b> GRAPH <http://e/g> { <http://e/c> <http://e/p> <http://e/d> } }",
        )
        .expect("insert");
    let result = service
        .execute_query_str("SELECT ?s FROM <urn:x-arq:DefaultGraph> WHERE { ?s ?p ?o }")
        .expect("query");
    let text = String::from_utf8(result.payload).unwrap();
    assert!(
        text.contains("http://e/a") && !text.contains("http://e/c"),
        "{text}"
    );
}
