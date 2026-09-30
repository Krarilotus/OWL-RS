//! SERVICE (native/federation.rs) against a client whose "endpoint" is a second engine:
//! the answers equal the same data read locally through GRAPH, joins send the bound
//! values as VALUES in chunks, and SILENT turns failures into one empty solution.

use std::sync::{Arc, Mutex};

use nrese_engine::{Engine, EngineConfig};
use nrese_sparql::{
    CancellationToken, QueryOptions, QueryResults, ServiceClient, ServiceResults, Services,
    evaluate_query, explain_query,
};
use oxrdf::{GraphName, Literal, NamedNode, Quad};
use spargebra::SparqlParser;

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
        _cancellation: Option<&CancellationToken>,
    ) -> Result<ServiceResults, Box<dyn std::error::Error + Send + Sync>> {
        assert_eq!(endpoint, REMOTE);
        self.queries.lock().unwrap().push(query.to_owned());
        if self.fail {
            return Err("the endpoint is down".into());
        }
        let parsed = SparqlParser::new().parse_query(query)?;
        let snapshot = self.engine.snapshot();
        let QueryResults::Solutions(solutions) =
            evaluate_query(&snapshot, &parsed, &QueryOptions::default())?
        else {
            return Err("not a SELECT".into());
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
