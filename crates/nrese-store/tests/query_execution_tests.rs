//! Q1 gate (store side): queries stream into their writer, stop promptly when cancelled,
//! and honour protocol dataset parameters.

use std::io::{self, Write};
use std::time::{Duration, Instant};

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

#[test]
fn cancelling_stops_a_running_query_promptly() {
    let service = service_with_triples(2_000);
    let prepared = PreparedQuery::parse(&SparqlQueryRequest::new(RUNAWAY)).expect("parse");
    let token = CancellationToken::new();
    let (result, stopped_after) = std::thread::scope(|scope| {
        let worker = scope.spawn(|| service.run_query(&prepared, &token, io::sink()));
        std::thread::sleep(Duration::from_millis(200));
        let cancelled_at = Instant::now();
        token.cancel();
        let result = worker.join().expect("worker");
        (result, cancelled_at.elapsed())
    });
    assert!(is_cancelled(&result), "{result:?}");
    eprintln!("stopped {stopped_after:?} after cancelling");
    // Without cancellation this query runs for hours. The evaluator checks the token on
    // every quad it reads, and we check it on every output row. Between two quad reads,
    // spareval's in-memory join and aggregation loops can't be interrupted. Here that is
    // under 1 s in release and about 3 s in debug. The native executor (Pf3) checks per
    // morsel. The bound below leaves debug headroom.
    assert!(
        stopped_after < Duration::from_secs(10),
        "took {stopped_after:?}"
    );
}

/// Cancels the token once `limit` bytes have been written: output arriving before the query
/// finished proves results stream, and cancelling mid-output stops the rest.
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
fn results_stream_and_can_be_cancelled_mid_output() {
    let service = service_with_triples(2_000);
    let query = "SELECT * WHERE { ?a ?b ?c . ?d ?e ?f }"; // 4 M rows
    let prepared = PreparedQuery::parse(&SparqlQueryRequest::new(query)).expect("parse");
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
    assert_eq!(count(SparqlQueryRequest::new(query)), 1);

    let mut request = SparqlQueryRequest::new(query);
    request.default_graphs = vec![
        "http://example.com/g1".to_owned(),
        "http://example.com/g2".to_owned(),
    ];
    assert_eq!(count(request), 2, "the protocol dataset replaces FROM");

    let mut request = SparqlQueryRequest::new("SELECT ?s WHERE { GRAPH ?g { ?s ?p ?o } }");
    request.named_graphs = vec!["http://example.com/g2".to_owned()];
    assert_eq!(count(request), 1);

    let mut invalid = SparqlQueryRequest::new(query);
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
        let mut request = SparqlQueryRequest::new(query);
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
    let mut protocol = SparqlQueryRequest::new(plain);
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
