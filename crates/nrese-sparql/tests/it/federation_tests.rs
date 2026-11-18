//! SERVICE (native/federation.rs) against a client whose "endpoint" is a second engine:
//! the answers equal the same data read locally through GRAPH, joins send the bound
//! values as VALUES in chunks, and SILENT turns failures into one empty solution.

use std::sync::{Arc, Mutex};

use nrese_engine::{Engine, EngineConfig};
use nrese_rdf::{BlankNode, GraphName, Literal, NamedNode, Quad, Term, Triple, Variable};
use nrese_sparql::{
    QueryEvaluationError, QueryOptions, QueryResults, ServiceCall, ServiceClient, ServiceResults,
    Services, evaluate_query, explain_query,
};
use nrese_sparql_syntax::SparqlParser;

const EX: &str = "http://example.com/";
const REMOTE: &str = "http://remote.example/sparql";

fn ex(local: &str) -> NamedNode {
    NamedNode::new_unchecked(format!("{EX}{local}"))
}

/// An endpoint that is an engine in the same process, recording the queries it gets.
struct Endpoint {
    engine: Engine,
    queries: Mutex<Vec<String>>,
    fail: bool,
}

impl ServiceClient for Endpoint {
    fn select(
        &self,
        endpoint: &str,
        query: &str,
        _call: ServiceCall<'_>,
    ) -> Result<ServiceResults, QueryEvaluationError> {
        assert_eq!(endpoint, REMOTE);
        self.queries.lock().unwrap().push(query.to_owned());
        if self.fail {
            return Err(QueryEvaluationError::Service("the endpoint is down".into()));
        }
        let parsed = SparqlParser::new()
            .parse_query(query)
            .map_err(|error| QueryEvaluationError::Service(error.into()))?;
        let snapshot = self.engine.snapshot();
        let QueryResults::Solutions(solutions) =
            evaluate_query(&snapshot, &parsed, &QueryOptions::default())?
        else {
            return Err(QueryEvaluationError::Service("not a SELECT".into()));
        };
        let variables = solutions.variables().to_vec();
        let mut rows = Vec::new();
        for solution in solutions {
            let solution = solution?;
            rows.push(variables.iter().map(|v| solution.get(v).cloned()).collect());
        }
        Ok(ServiceResults { variables, rows })
    }
}

/// Local data: people and where they live. Remote data: cities and their countries.
fn data() -> (Vec<Quad>, Vec<Quad>) {
    let mut local = Vec::new();
    let mut remote = Vec::new();
    for i in 0..600 {
        local.push(Quad::new(
            ex(&format!("person{i}")),
            ex("livesIn"),
            ex(&format!("city{}", i % 450)),
            GraphName::DefaultGraph,
        ));
    }
    for c in 0..400 {
        remote.push(Quad::new(
            ex(&format!("city{c}")),
            ex("country"),
            ex(&format!("country{}", c % 7)),
            GraphName::DefaultGraph,
        ));
        remote.push(Quad::new(
            ex(&format!("city{c}")),
            ex("name"),
            Literal::new_simple_literal(format!("City {c}")),
            GraphName::DefaultGraph,
        ));
    }
    (local, remote)
}

fn engine(quads: &[Quad]) -> Engine {
    let engine = Engine::new(EngineConfig::default()).unwrap();
    let mut tx = engine.transaction();
    for quad in quads {
        tx.insert(quad.as_ref());
    }
    tx.commit().unwrap();
    engine
}

fn rows(engine: &Engine, text: &str, options: &QueryOptions) -> Result<Vec<String>, String> {
    let query = SparqlParser::new().parse_query(text).unwrap();
    let snapshot = engine.snapshot();
    let results = evaluate_query(&snapshot, &query, options).map_err(|e| e.to_string())?;
    let QueryResults::Solutions(solutions) = results else {
        panic!("solutions")
    };
    let variables = solutions.variables().to_vec();
    let mut out: Vec<String> = solutions
        .map(|s| {
            let s = s.unwrap();
            variables
                .iter()
                .map(|v| s.get(v).map_or("-".to_owned(), ToString::to_string))
                .collect::<Vec<_>>()
                .join(" ")
        })
        .collect();
    out.sort();
    Ok(out)
}

#[test]
fn service_answers_equal_the_same_data_read_locally() {
    let (local, remote) = data();
    let endpoint = Arc::new(Endpoint {
        engine: engine(&remote),
        queries: Mutex::default(),
        fail: false,
    });
    let federated = engine(&local);
    // The same data in one store, the remote part in a named graph.
    let graph = NamedNode::new_unchecked(REMOTE);
    let combined: Vec<Quad> = local
        .iter()
        .cloned()
        .chain(remote.iter().map(|q| {
            Quad::new(
                q.subject.clone(),
                q.predicate.clone(),
                q.object.clone(),
                graph.clone(),
            )
        }))
        .collect();
    let combined = engine(&combined);
    let options = QueryOptions {
        services: Some(Services(endpoint.clone())),
        ..QueryOptions::default()
    };
    for (body, bind_requests) in [
        // Alone: one request.
        (
            "{remote ?city <http://example.com/country> ?country }",
            Some(1),
        ),
        // Joined: the 450 distinct cities go as VALUES in chunks of 200.
        (
            "?person <http://example.com/livesIn> ?city . {remote ?city <http://example.com/country> ?country ; <http://example.com/name> ?name }",
            Some(3),
        ),
        // The SERVICE written first: the local pattern still binds it.
        (
            "{remote ?city <http://example.com/country> <http://example.com/country3> } ?person <http://example.com/livesIn> ?city",
            Some(3),
        ),
        // OPTIONAL: people in cities the endpoint doesn't know keep their row.
        (
            "?person <http://example.com/livesIn> ?city OPTIONAL { {remote ?city <http://example.com/country> ?country } }",
            Some(3),
        ),
        // A filter on both sides, applied locally after the join.
        (
            "?person <http://example.com/livesIn> ?city . {remote ?city <http://example.com/name> ?name } FILTER(STRENDS(?name, \"7\"))",
            None,
        ),
    ] {
        let federated_query = format!(
            "SELECT * WHERE {{ {} }}",
            body.replace("{remote", &format!("SERVICE <{REMOTE}> {{"))
        );
        let local_query = format!(
            "SELECT * WHERE {{ {} }}",
            body.replace("{remote", &format!("GRAPH <{REMOTE}> {{"))
        );
        endpoint.queries.lock().unwrap().clear();
        let answers = rows(&federated, &federated_query, &options).unwrap();
        let expected = rows(&combined, &local_query, &QueryOptions::default()).unwrap();
        assert_eq!(answers, expected, "{federated_query}");
        assert!(!answers.is_empty(), "{federated_query}");
        let sent = endpoint.queries.lock().unwrap().clone();
        if let Some(count) = bind_requests {
            assert_eq!(sent.len(), count, "{federated_query}: {sent:#?}");
        }
        if sent.len() > 1 {
            assert!(sent.iter().all(|q| q.contains("VALUES")), "{sent:#?}");
        }
        let parsed = SparqlParser::new().parse_query(&federated_query).unwrap();
        assert_eq!(
            explain_query(&federated.snapshot(), &parsed, &options)
                .unwrap()
                .executor,
            "native"
        );
    }
}

#[test]
fn silent_services_fail_into_one_empty_solution() {
    let (local, remote) = data();
    let federated = engine(&local);
    let down = QueryOptions {
        services: Some(Services(Arc::new(Endpoint {
            engine: engine(&remote),
            queries: Mutex::default(),
            fail: true,
        }))),
        ..QueryOptions::default()
    };
    let off = QueryOptions::default();
    let query = |silent: &str| {
        format!(
            "SELECT ?person ?country WHERE {{ ?person <{EX}livesIn> <{EX}city1> . SERVICE {silent} <{REMOTE}> {{ <{EX}city1> <{EX}country> ?country }} }}"
        )
    };
    for options in [&down, &off] {
        let error = rows(&federated, &query(""), options).unwrap_err();
        assert!(
            error.contains("down") || error.contains("not enabled"),
            "{error}"
        );
        // SILENT: the endpoint's part is one solution without bindings.
        let answers = rows(&federated, &query("SILENT"), options).unwrap();
        assert_eq!(answers.len(), 2, "{answers:?}");
        assert!(answers.iter().all(|row| row.ends_with(" -")), "{answers:?}");
    }
}

/// An endpoint whose answer is made from the query text it gets.
struct Canned(Box<dyn Fn(&str) -> ServiceResults + Send + Sync>);

impl ServiceClient for Canned {
    fn select(
        &self,
        endpoint: &str,
        query: &str,
        _call: ServiceCall<'_>,
    ) -> Result<ServiceResults, QueryEvaluationError> {
        assert_eq!(endpoint, REMOTE);
        Ok((self.0)(query))
    }
}

fn canned(answer: impl Fn(&str) -> ServiceResults + Send + Sync + 'static) -> QueryOptions {
    QueryOptions {
        services: Some(Services(Arc::new(Canned(Box::new(answer))))),
        ..QueryOptions::default()
    }
}

fn count(n: u64) -> Vec<String> {
    vec![
        Literal::new_typed_literal(
            n.to_string(),
            NamedNode::new_unchecked("http://www.w3.org/2001/XMLSchema#integer"),
        )
        .to_string(),
    ]
}

/// A blank node label in an endpoint's answer names a node of that answer only (SPARQL 1.1
/// Query Results JSON §3.2.2 and XML §2.3.1: labels are scoped to the results document): the
/// same label twice in one answer is one node; in two answers, or in an answer and the store,
/// two nodes that don't join. Triple terms holding one are scoped the same way.
#[test]
fn remote_blank_nodes_are_local_to_their_answer() {
    let blank = || BlankNode::new_unchecked("b0");
    // Every answer: two rows, each with `_:b0` for ?x and `<< _:b0 :p :o >>` for ?t,
    // as far as the query mentions them.
    let options = canned(move |query| {
        let mut variables = Vec::new();
        let mut row = Vec::new();
        if query.contains("?x") {
            variables.push(Variable::new_unchecked("x"));
            row.push(Some(Term::from(blank())));
        }
        if query.contains("?t") {
            variables.push(Variable::new_unchecked("t"));
            row.push(Some(Term::Triple(Box::new(Triple::new(
                blank(),
                ex("p"),
                ex("o"),
            )))));
        }
        ServiceResults {
            variables,
            rows: vec![row.clone(), row],
        }
    });
    // The store has a blank node labelled b0 too.
    let local = engine(&[Quad::new(
        blank(),
        ex("p"),
        ex("o"),
        GraphName::DefaultGraph,
    )]);
    for (body, expected) in [
        // One answer: its two rows name one node.
        (
            format!(
                "SELECT (COUNT(DISTINCT ?x) AS ?n) WHERE {{ SERVICE <{REMOTE}> {{ ?x <{EX}p> <{EX}o> }} }}"
            ),
            1,
        ),
        // Two answers: their nodes differ.
        (
            format!(
                "SELECT (COUNT(*) AS ?n) WHERE {{ SERVICE <{REMOTE}> {{ ?x <{EX}p> <{EX}o> }} SERVICE <{REMOTE}> {{ ?x <{EX}q> <{EX}o> }} }}"
            ),
            0,
        ),
        // An answer and the store: their nodes differ.
        (
            format!(
                "SELECT (COUNT(*) AS ?n) WHERE {{ ?x <{EX}p> <{EX}o> SERVICE <{REMOTE}> {{ ?x <{EX}q> <{EX}o> }} }}"
            ),
            0,
        ),
        // Inside triple terms of two answers: they differ.
        (
            format!(
                "SELECT (COUNT(*) AS ?n) WHERE {{ SERVICE <{REMOTE}> {{ ?t <{EX}p> <{EX}o> }} SERVICE <{REMOTE}> {{ ?t <{EX}q> <{EX}o> }} }}"
            ),
            0,
        ),
    ] {
        assert_eq!(
            rows(&local, &body, &options).unwrap(),
            count(expected),
            "{body}"
        );
    }
}

/// `SERVICE SILENT ?e`: a row whose ?e is no IRI (a literal, or unbound) can't be sent
/// anywhere, so it is joined to one empty solution, as a failed call's rows are: kept once,
/// and joined to no other endpoint's answer. Joined and OPTIONAL alike.
#[test]
fn silent_service_variables_keep_rows_without_an_endpoint() {
    let typed = |s: &str| {
        Quad::new(
            ex(s),
            NamedNode::new_unchecked("http://www.w3.org/1999/02/22-rdf-syntax-ns#type"),
            ex("T"),
            GraphName::DefaultGraph,
        )
    };
    let local = engine(&[
        typed("s1"),
        typed("s2"),
        typed("s3"),
        Quad::new(
            ex("s1"),
            ex("ep"),
            NamedNode::new_unchecked(REMOTE),
            GraphName::DefaultGraph,
        ),
        Quad::new(
            ex("s2"),
            ex("ep"),
            Literal::new_simple_literal("elsewhere"),
            GraphName::DefaultGraph,
        ),
    ]);
    // The endpoint knows one city's name.
    let options = canned(|_| ServiceResults {
        variables: vec![Variable::new_unchecked("x"), Variable::new_unchecked("c")],
        rows: vec![vec![
            Some(ex("city").into()),
            Some(Literal::new_simple_literal("one").into()),
        ]],
    });
    let expected = vec![
        format!("<{EX}s1> \"one\""),
        format!("<{EX}s2> -"),
        format!("<{EX}s3> -"),
    ];
    for service in [
        "SERVICE SILENT ?e { ?x <http://example.com/name> ?c }",
        "OPTIONAL { SERVICE SILENT ?e { ?x <http://example.com/name> ?c } }",
    ] {
        let query = format!(
            "SELECT ?s ?c WHERE {{ ?s a <{EX}T> OPTIONAL {{ ?s <{EX}ep> ?e }} {service} }}"
        );
        assert_eq!(rows(&local, &query, &options).unwrap(), expected, "{query}");
    }
}

/// The endpoint evaluates the block under the query's BASE: `IRI("rel")` there resolves
/// against it, as it would locally (SPARQL 1.1 §17.4.2.8).
#[test]
fn service_blocks_resolve_against_the_query_base() {
    let endpoint = Arc::new(Endpoint {
        engine: engine(&[]),
        queries: Mutex::default(),
        fail: false,
    });
    let options = QueryOptions {
        services: Some(Services(endpoint)),
        ..QueryOptions::default()
    };
    let query = format!(
        "BASE <http://base.example/> SELECT ?x WHERE {{ SERVICE <{REMOTE}> {{ BIND(IRI(\"rel\") AS ?x) }} }}"
    );
    assert_eq!(
        rows(&engine(&[]), &query, &options).unwrap(),
        vec!["<http://base.example/rel>".to_owned()]
    );
}

/// `SERVICE ?e` joined to no solutions: no solution names an endpoint, so nothing is
/// called and the join has no solutions, SILENT or not (SPARQL 1.1 Federated Query §4: the
/// endpoint is each solution's value of ?e).
#[test]
fn service_variables_joined_to_no_solutions_have_none() {
    let options = canned(|query| panic!("no endpoint to call, yet called with {query}"));
    let local = engine(&[]);
    for service in ["SERVICE", "SERVICE SILENT"] {
        for body in [
            format!("?s <{EX}ep> ?e {service} ?e {{ ?x <{EX}name> ?c }}"),
            format!("?s <{EX}ep> ?e OPTIONAL {{ {service} ?e {{ ?x <{EX}name> ?c }} }}"),
        ] {
            let query = format!("SELECT * WHERE {{ {body} }}");
            assert_eq!(rows(&local, &query, &options), Ok(Vec::new()), "{query}");
        }
    }
}
