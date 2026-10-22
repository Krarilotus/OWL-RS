//! SERVICE (native/federation.rs) against a client whose "endpoint" is a second engine:
//! the answers equal the same data read locally through GRAPH, joins send the bound
//! values as VALUES in chunks, and SILENT turns failures into one empty solution.

use std::sync::{Arc, Mutex};

use nrese_engine::{Engine, EngineConfig};
use nrese_rdf::{GraphName, Literal, NamedNode, Quad};
use nrese_sparql::{
    CancellationToken, QueryEvaluationError, QueryOptions, QueryResults, ServiceClient,
    ServiceResults, Services, evaluate_query, explain_query,
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
        budget: &Arc<nrese_exec::Budget>,
        cancellation: Option<&CancellationToken>,
    ) -> Result<ServiceResults, QueryEvaluationError> {
        assert_eq!(endpoint, REMOTE);
        self.queries.lock().unwrap().push(query.to_owned());
        if self.fail {
            return Err(QueryEvaluationError::Service("the endpoint is down".into()));
        }
        let parsed = SparqlParser::new()
            .parse_query(query)
            .map_err(|e| QueryEvaluationError::Service(Box::new(e)))?;
        let mut options = QueryOptions::default();
        options.resources.memory = nrese_exec::Envelope::from_budget(Arc::clone(budget));
        options.resources.cancellation = cancellation.cloned();
        assert!(std::ptr::eq(&*options.resources.memory, &**budget));
        let snapshot = self.engine.snapshot();
        let QueryResults::Solutions(solutions) = evaluate_query(&snapshot, &parsed, &options)?
        else {
            return Err(QueryEvaluationError::Service("not a SELECT".into()));
        };
        let mut result = ServiceResults::new(solutions.variables().to_vec(), budget);
        let stop = nrese_exec::StopSource::new(cancellation.map(CancellationToken::flag), None);
        let mut solutions = solutions;
        loop {
            stop.check().map_err(QueryEvaluationError::from)?;
            let next = solutions.next();
            stop.check().map_err(QueryEvaluationError::from)?;
            let Some(solution) = next else {
                break;
            };
            result.push_row(solution?.values(), &stop)?;
        }
        Ok(result)
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

/// Response modes exercise the existing public client API, including typed
/// failures and a client that notices cancellation only as it returns.
enum N7Mode {
    Engine(Engine),
    Nodes,
    Memory,
    Cancel(bool),
    FailSecond,
}

struct N7Endpoint {
    mode: N7Mode,
    queries: Mutex<Vec<(String, String)>>,
}

impl N7Endpoint {
    fn new(mode: N7Mode) -> Arc<Self> {
        Arc::new(Self {
            mode,
            queries: Mutex::default(),
        })
    }

    fn options(self: &Arc<Self>) -> QueryOptions {
        QueryOptions {
            services: Some(Services(self.clone())),
            ..QueryOptions::default()
        }
    }
}

impl ServiceClient for N7Endpoint {
    fn select(
        &self,
        endpoint: &str,
        query: &str,
        budget: &Arc<nrese_exec::Budget>,
        cancellation: Option<&CancellationToken>,
    ) -> Result<ServiceResults, QueryEvaluationError> {
        let number = {
            let mut queries = self.queries.lock().unwrap();
            queries.push((endpoint.to_owned(), query.to_owned()));
            queries.len()
        };
        if endpoint.ends_with("/bad") {
            return Err(QueryEvaluationError::Service("endpoint failed".into()));
        }
        if endpoint.ends_with("/empty") {
            return Ok(ServiceResults::new(
                vec![nrese_rdf::Variable::new_unchecked("answer")],
                budget,
            ));
        }
        match &self.mode {
            N7Mode::Memory => Err(budget
                .charge(budget.remaining().saturating_add(1))
                .unwrap_err()
                .into()),
            N7Mode::Cancel(fail) => {
                cancellation.expect("test supplies a token").cancel();
                if *fail {
                    Err(QueryEvaluationError::Service("client stopped".into()))
                } else {
                    Ok(ServiceResults::new(Vec::new(), budget))
                }
            }
            N7Mode::FailSecond => {
                if number == 2 {
                    return Err(QueryEvaluationError::Service(
                        "later bind chunk failed".into(),
                    ));
                }
                let mut result =
                    ServiceResults::new(vec![nrese_rdf::Variable::new_unchecked("answer")], budget);
                let stop =
                    nrese_exec::StopSource::new(cancellation.map(CancellationToken::flag), None);
                result.push_row(&[Some(Literal::new_simple_literal("prefix").into())], &stop)?;
                Ok(result)
            }
            N7Mode::Nodes => {
                let node = nrese_rdf::BlankNode::new_unchecked("remote");
                let nested = nrese_rdf::Triple::new(
                    node.clone(),
                    ex("p"),
                    nrese_rdf::Triple::new(ex("inner"), ex("q"), node.clone()),
                );
                let row = vec![Some(node.into()), Some(nested.into())];
                let mut result = ServiceResults::new(
                    vec![
                        nrese_rdf::Variable::new_unchecked("x"),
                        nrese_rdf::Variable::new_unchecked("t"),
                    ],
                    budget,
                );
                let stop =
                    nrese_exec::StopSource::new(cancellation.map(CancellationToken::flag), None);
                result.push_row(&row, &stop)?;
                result.push_row(&row, &stop)?;
                Ok(result)
            }
            N7Mode::Engine(engine) => {
                let parsed = SparqlParser::new()
                    .parse_query(query)
                    .map_err(|e| QueryEvaluationError::Service(Box::new(e)))?;
                let mut options = QueryOptions::default();
                options.resources.memory = nrese_exec::Envelope::from_budget(Arc::clone(budget));
                options.resources.cancellation = cancellation.cloned();
                assert!(std::ptr::eq(&*options.resources.memory, &**budget));
                let snapshot = engine.snapshot();
                let QueryResults::Solutions(solutions) =
                    evaluate_query(&snapshot, &parsed, &options)?
                else {
                    return Err(QueryEvaluationError::Service("expected SELECT".into()));
                };
                let mut result = ServiceResults::new(solutions.variables().to_vec(), budget);
                let stop =
                    nrese_exec::StopSource::new(cancellation.map(CancellationToken::flag), None);
                let mut solutions = solutions;
                loop {
                    stop.check().map_err(QueryEvaluationError::from)?;
                    let next = solutions.next();
                    stop.check().map_err(QueryEvaluationError::from)?;
                    let Some(solution) = next else {
                        break;
                    };
                    result.push_row(solution?.values(), &stop)?;
                }
                Ok(result)
            }
        }
    }
}

#[test]
fn n7_variable_service_preserves_group_bags_and_undef() {
    let local = engine(&[]);
    let values = format!(
        "VALUES (?ep ?key ?tag) {{
        (<{REMOTE}> 1 \"a\") (<{REMOTE}> 1 \"a\") (<{REMOTE}> UNDEF \"u\")
        (<{EX}bad> 1 \"b\") (\"literal\" 1 \"l\") (UNDEF 1 \"n\") (<{EX}empty> 1 \"e\") }}"
    );
    let remote = "SERVICE SILENT ?ep { VALUES ?key { 1 1 2 } BIND(\"remote\" AS ?answer) }";
    for optional in [false, true] {
        let endpoint = N7Endpoint::new(N7Mode::Engine(engine(&[])));
        let right = if optional {
            format!("OPTIONAL {{ {remote} }}")
        } else {
            remote.to_owned()
        };
        let query = format!("SELECT ?tag ?key ?answer WHERE {{ {values} {right} }}");
        let mut expected = vec!["(\"a\" 1 \"remote\")"; 4];
        expected.extend([
            "(\"u\" 1 \"remote\")",
            "(\"u\" 1 \"remote\")",
            "(\"u\" 2 \"remote\")",
            "(\"b\" 1 UNDEF)",
            "(\"l\" 1 UNDEF)",
            "(\"n\" 1 UNDEF)",
        ]);
        if optional {
            expected.push("(\"e\" 1 UNDEF)");
        }
        let oracle = format!(
            "SELECT ?tag ?key ?answer WHERE {{ VALUES (?tag ?key ?answer) {{ {} }} }}",
            expected.join(" ")
        );
        assert_eq!(
            rows(&local, &query, &endpoint.options()).unwrap(),
            rows(&local, &oracle, &QueryOptions::default()).unwrap(),
            "optional={optional}"
        );
        let calls = endpoint.queries.lock().unwrap();
        assert_eq!(calls.len(), 3);
        assert_eq!(
            calls
                .iter()
                .filter(|(endpoint, _)| endpoint == REMOTE)
                .count(),
            1
        );
        drop(calls);
        let query = query.replace("SERVICE SILENT", "SERVICE");
        assert!(rows(&local, &query, &endpoint.options()).is_err());
    }
}

#[test]
fn n7_variable_service_does_not_reassign_remote_endpoint_bindings() {
    let local = engine(&[]);
    for optional in [false, true] {
        let endpoint = N7Endpoint::new(N7Mode::Engine(engine(&[])));
        let service = format!(
            "SERVICE SILENT ?ep {{ BIND(<{EX}other> AS ?ep) BIND(\"remote\" AS ?answer) }}"
        );
        let right = if optional {
            format!("OPTIONAL {{ {service} FILTER(?tag != \"a\") }}")
        } else {
            service
        };
        let query = format!(
            "SELECT ?tag ?answer WHERE {{ VALUES (?ep ?tag) {{ (<{REMOTE}> \"a\") (<{EX}other> \"b\") (UNDEF \"u\") }} {right} }}"
        );
        let expected = if optional {
            "(\"a\" UNDEF) (\"b\" \"remote\") (\"u\" UNDEF)"
        } else {
            "(\"b\" \"remote\") (\"u\" UNDEF)"
        };
        let oracle =
            format!("SELECT ?tag ?answer WHERE {{ VALUES (?tag ?answer) {{ {expected} }} }}");
        assert_eq!(
            rows(&local, &query, &endpoint.options()).unwrap(),
            rows(&local, &oracle, &QueryOptions::default()).unwrap()
        );
    }
}

#[test]
fn n7_distinct_variable_service_uses_bound_endpoint_groups() {
    let local = engine(&[]);
    for optional in [false, true] {
        for silent in [false, true] {
            let endpoint = N7Endpoint::new(N7Mode::Engine(engine(&[])));
            let values = if silent {
                format!(
                    "(<{REMOTE}> \"a\") (<{REMOTE}> \"a\") (<{EX}bad> \"b\")
                    (\"literal\" \"l\") (UNDEF \"u\") (<{EX}empty> \"e\")"
                )
            } else {
                format!("(<{REMOTE}> \"a\") (<{REMOTE}> \"a\")")
            };
            let service = format!(
                "SERVICE {} ?ep {{ BIND(\"remote\" AS ?answer) }}",
                if silent { "SILENT" } else { "" }
            );
            let right = if optional {
                format!("OPTIONAL {{ {service} }}")
            } else {
                service
            };
            let query = format!(
                "SELECT DISTINCT ?tag ?answer WHERE {{ VALUES (?ep ?tag) {{ {values} }} {right} }}"
            );
            let mut expected = vec!["(\"a\" \"remote\")"];
            if silent {
                expected.extend(["(\"b\" UNDEF)", "(\"l\" UNDEF)", "(\"u\" UNDEF)"]);
                if optional {
                    expected.push("(\"e\" UNDEF)");
                }
            }
            let oracle = format!(
                "SELECT ?tag ?answer WHERE {{ VALUES (?tag ?answer) {{ {} }} }}",
                expected.join(" ")
            );
            let actual = rows(&local, &query, &endpoint.options());
            assert_eq!(
                actual,
                rows(&local, &oracle, &QueryOptions::default()),
                "optional={optional}, silent={silent}"
            );
            assert_eq!(
                endpoint.queries.lock().unwrap().len(),
                if silent { 3 } else { 1 }
            );
        }
    }
}

#[test]
fn n7_invalid_and_empty_endpoint_groups_keep_the_selected_profile() {
    let local = engine(&[]);
    let endpoint = N7Endpoint::new(N7Mode::Engine(engine(&[])));
    let options = endpoint.options();
    // No endpoint column, explicit UNDEF, a literal, and a blank-node value. The
    // project chooses unit on invalid SILENT invocations; section 4 is informative.
    for body in [
        "VALUES ?tag { 1 1 } SERVICE SILENT ?ep { BIND(1 AS ?x) }",
        "VALUES (?ep ?tag) { (UNDEF 1) (UNDEF 1) } SERVICE SILENT ?ep { BIND(1 AS ?x) }",
        "VALUES (?ep ?tag) { (\"literal\" 1) (\"literal\" 1) } SERVICE SILENT ?ep { BIND(1 AS ?x) }",
        "VALUES ?tag { 1 1 } BIND(BNODE(\"bad\") AS ?ep) SERVICE SILENT ?ep { BIND(1 AS ?x) }",
    ] {
        let query = format!("SELECT ?tag ?x WHERE {{ {body} }}");
        let oracle = "SELECT ?tag ?x WHERE { VALUES (?tag ?x) { (1 UNDEF) (1 UNDEF) } }";
        assert_eq!(
            rows(&local, &query, &options).unwrap(),
            rows(&local, oracle, &QueryOptions::default()).unwrap()
        );
        assert!(
            rows(
                &local,
                &query.replace("SERVICE SILENT", "SERVICE"),
                &options
            )
            .is_err()
        );
    }
    assert!(
        rows(
            &local,
            "SELECT * WHERE { VALUES ?ep {} SERVICE SILENT ?ep { BIND(1 AS ?x) } }",
            &options
        )
        .unwrap()
        .is_empty()
    );
    assert_eq!(
        rows(
            &local,
            "SELECT * WHERE { SERVICE SILENT ?ep { BIND(1 AS ?x) } }",
            &options
        )
        .unwrap()
        .len(),
        1
    );
    assert!(endpoint.queries.lock().unwrap().is_empty());
}

#[test]
fn n7_response_blank_nodes_are_fresh_and_recursive() {
    let local = engine(&[Quad::new(
        nrese_rdf::BlankNode::new_unchecked("remote"),
        ex("p"),
        ex("local"),
        GraphName::DefaultGraph,
    )]);
    // Install a real nontrivial equality class so canonical=true reaches Names::id,
    // rather than just setting an option on a store with no canonical read model.
    let mut tx = local.transaction();
    let same_as = tx.intern(nrese_rdf::vocab::owl::SAME_AS.into());
    let alias = tx.intern(ex("alias").as_ref().into());
    let representative = tx.intern(ex("representative").as_ref().into());
    tx.insert_inferred(nrese_engine::EncodedTriple {
        subject: alias,
        predicate: same_as,
        object: representative,
    });
    tx.commit().unwrap();
    local.set_equality(Some(same_as));
    let endpoint = N7Endpoint::new(N7Mode::Nodes);
    for canonical in [false, true] {
        let mut options = endpoint.options();
        options.equality_canonical = canonical;
        let one = format!("SELECT ?x ?t WHERE {{ SERVICE <{REMOTE}> {{ ?x <{EX}p> ?t }} }}");
        let parsed = SparqlParser::new().parse_query(&one).unwrap();
        let snapshot = local.snapshot();
        let QueryResults::Solutions(found) = evaluate_query(&snapshot, &parsed, &options).unwrap()
        else {
            panic!("SELECT")
        };
        let found: Vec<_> = found.map(Result::unwrap).collect();
        assert_eq!(found.len(), 2);
        assert_eq!(found[0].get("x"), found[1].get("x"));
        let nrese_rdf::Term::Triple(outer) = found[0].get("t").unwrap() else {
            panic!("triple")
        };
        assert_eq!(
            found[0].get("x").unwrap().to_string(),
            outer.subject.to_string()
        );
        let nrese_rdf::Term::Triple(inner) = &outer.object else {
            panic!("nested triple")
        };
        assert_eq!(found[0].get("x"), Some(&inner.object));
        let two = format!(
            "SELECT * WHERE {{ SERVICE <{REMOTE}> {{ ?x <{EX}p> ?t }} SERVICE <{REMOTE}> {{ ?x <{EX}p> ?t }} }}"
        );
        assert!(rows(&local, &two, &options).unwrap().is_empty());
        let collision = format!(
            "SELECT * WHERE {{ ?stored <{EX}p> <{EX}local> . SERVICE <{REMOTE}> {{ ?x <{EX}p> ?t }} FILTER(?stored = ?x) }}"
        );
        assert!(rows(&local, &collision, &options).unwrap().is_empty());
    }
    let keys = (0..201)
        .map(|key| key.to_string())
        .collect::<Vec<_>>()
        .join(" ");
    endpoint.queries.lock().unwrap().clear();
    let query = format!(
        "SELECT DISTINCT ?x WHERE {{ VALUES ?key {{ {keys} }} SERVICE <{REMOTE}> {{ ?key <{EX}p> ?x }} }}"
    );
    // DISTINCT uses the existing set evaluator, which calls this fixed IRI once
    // without bind chunks. One physical answer therefore has one node identity.
    assert_eq!(rows(&local, &query, &endpoint.options()).unwrap().len(), 1);
    assert_eq!(endpoint.queries.lock().unwrap().len(), 1);
    endpoint.queries.lock().unwrap().clear();
    // A bag query reaches the ordinary bind join. Deduplicate only in the test,
    // after verifying the complete bag from two physical responses. Nodes mode
    // leaves ?key unbound, so each of its four returned rows joins all 201 keys.
    let bag = query.replace("SELECT DISTINCT ?x", "SELECT ?x");
    let found = rows(&local, &bag, &endpoint.options()).unwrap();
    assert_eq!(found.len(), 804);
    assert_eq!(
        found
            .into_iter()
            .collect::<std::collections::BTreeSet<_>>()
            .len(),
        2
    );
    assert_eq!(endpoint.queries.lock().unwrap().len(), 2);
}

#[test]
fn n7_service_serialization_carries_base_to_chunks_and_fallback() {
    let local = engine(&[]);
    let endpoint = N7Endpoint::new(N7Mode::Engine(engine(&[])));
    let keys = (0..201)
        .map(|key| key.to_string())
        .collect::<Vec<_>>()
        .join(" ");
    let query = format!(
        "BASE <http://example.org/base/> SELECT ?key ?x WHERE {{ VALUES ?key {{ {keys} }} SERVICE <{REMOTE}> {{ VALUES ?key {{ {keys} }} BIND(IRI(\"relative%20é#part\") AS ?x) }} }}"
    );
    let expected = format!(
        "SELECT ?key ?x WHERE {{ VALUES ?key {{ {keys} }} BIND(<http://example.org/base/relative%20é#part> AS ?x) }}"
    );
    assert_eq!(
        rows(&local, &query, &endpoint.options()).unwrap(),
        rows(&local, &expected, &QueryOptions::default()).unwrap()
    );
    assert_eq!(endpoint.queries.lock().unwrap().len(), 2);
    assert!(
        endpoint
            .queries
            .lock()
            .unwrap()
            .iter()
            .all(|(_, query)| query.starts_with("BASE <http://example.org/base/>"))
    );
    endpoint.queries.lock().unwrap().clear();
    let distinct = query.replace("SELECT ?key ?x", "SELECT DISTINCT ?x");
    let one = "SELECT ?x WHERE { VALUES ?x { <http://example.org/base/relative%20é#part> } }";
    assert_eq!(
        rows(&local, &distinct, &endpoint.options()).unwrap(),
        rows(&local, one, &QueryOptions::default()).unwrap()
    );
    assert_eq!(endpoint.queries.lock().unwrap().len(), 1);
    endpoint.queries.lock().unwrap().clear();
    let fallback = format!(
        "BASE <http://example.org/base/> SELECT ?key ?x WHERE {{ VALUES ?key {{ UNDEF 1 }} SERVICE <{REMOTE}> {{ VALUES ?key {{ 1 2 }} BIND(IRI(\"relative%20é#part\") AS ?x) }} }}"
    );
    let expected = "SELECT ?key ?x WHERE { VALUES (?key ?x) {
        (1 <http://example.org/base/relative%20é#part>)
        (1 <http://example.org/base/relative%20é#part>)
        (2 <http://example.org/base/relative%20é#part>) } }";
    assert_eq!(
        rows(&local, &fallback, &endpoint.options()).unwrap(),
        rows(&local, expected, &QueryOptions::default()).unwrap()
    );
    assert_eq!(endpoint.queries.lock().unwrap().len(), 1);
    assert!(
        endpoint.queries.lock().unwrap()[0]
            .1
            .starts_with("BASE <http://example.org/base/>")
    );
}

#[test]
fn n7_silent_preserves_typed_stops_and_discards_partial_chunks() {
    use nrese_sparql::QueryEvaluationError;
    let local = engine(&[]);
    let text = format!("SELECT * WHERE {{ SERVICE SILENT <{REMOTE}> {{ BIND(1 AS ?x) }} }}");
    let query = SparqlParser::new().parse_query(&text).unwrap();
    for mode in [N7Mode::Memory, N7Mode::Cancel(false), N7Mode::Cancel(true)] {
        let memory = matches!(mode, N7Mode::Memory);
        let endpoint = N7Endpoint::new(mode);
        let mut options = endpoint.options();
        options.resources.cancellation = Some(CancellationToken::new());
        options.resources.memory = nrese_exec::Envelope::new(Some(1 << 20), None);
        let snapshot = local.snapshot();
        let result = evaluate_query(&snapshot, &query, &options);
        if memory {
            assert!(matches!(result, Err(QueryEvaluationError::MemoryLimit(_))));
        } else {
            assert!(matches!(result, Err(QueryEvaluationError::Cancelled)));
        }
    }
    let endpoint = N7Endpoint::new(N7Mode::FailSecond);
    let keys = (0..201)
        .map(|key| key.to_string())
        .collect::<Vec<_>>()
        .join(" ");
    let query = format!(
        "SELECT ?key ?answer WHERE {{ VALUES ?key {{ {keys} }} SERVICE SILENT <{REMOTE}> {{ ?key <{EX}p> ?answer }} }}"
    );
    let oracle = format!("SELECT ?key ?answer WHERE {{ VALUES ?key {{ {keys} }} }}");
    assert_eq!(
        rows(&local, &query, &endpoint.options()).unwrap(),
        rows(&local, &oracle, &QueryOptions::default()).unwrap()
    );
    assert_eq!(endpoint.queries.lock().unwrap().len(), 2);
}

#[test]
fn n7_nested_endpoint_bag_and_escaping_response_share_the_invocation_budget() {
    let endpoint = Endpoint {
        engine: engine(&[]),
        queries: Mutex::default(),
        fail: false,
    };
    let budget = Arc::new(nrese_exec::Budget::new(1 << 20));
    let mut caller = nrese_exec::Reservation::new(Arc::clone(&budget));
    caller.charge(64).unwrap();
    let token = CancellationToken::new();
    let response = endpoint
        .select(
            REMOTE,
            "SELECT ?x WHERE { VALUES ?x { 1 1 UNDEF } }",
            &budget,
            Some(&token),
        )
        .unwrap();
    assert_eq!(response.rows().len(), 3);
    assert_eq!(response.rows()[0], response.rows()[1]);
    assert_eq!(response.rows()[2], vec![None]);
    assert!(budget.used() > 64);
    // The nested Context is already gone, but the decoded response still owns
    // admitted payload alongside the caller. It cannot spend that capacity again.
    assert!(budget.charge(budget.remaining() + 1).is_err());
    drop(endpoint);
    drop(response);
    assert_eq!(budget.used(), 64);
    drop(caller);
    assert_eq!(budget.used(), 0);
}

#[test]
fn n7_nested_evaluation_preserves_its_original_actuator_and_memory_failure() {
    struct Leaf {
        actuator: CancellationToken,
        root: Arc<nrese_exec::Budget>,
        memory: bool,
        called: std::sync::atomic::AtomicBool,
    }
    impl ServiceClient for Leaf {
        fn select(
            &self,
            _: &str,
            _: &str,
            budget: &Arc<nrese_exec::Budget>,
            cancellation: Option<&CancellationToken>,
        ) -> Result<ServiceResults, QueryEvaluationError> {
            let token = cancellation.expect("the nested evaluation keeps the caller token");
            assert!(Arc::ptr_eq(&token.flag(), &self.actuator.flag()));
            assert!(!Arc::ptr_eq(budget, &self.root)); // Native's ordinary child part.
            assert!(self.root.used() >= budget.used());
            self.called
                .store(true, std::sync::atomic::Ordering::Release);
            if self.memory {
                // Refusal comes from this child's actual parent, not a fabricated
                // error or an independent budget with an equal-sized limit.
                Err(budget.charge(2 << 20).unwrap_err().into())
            } else {
                // The fixture retains the actuator; the leaf does not reconstruct
                // one from a stop trait. The native closing check must see this
                // even though the leaf returns a successful empty response.
                self.actuator.cancel();
                Ok(ServiceResults::new(Vec::new(), budget))
            }
        }
    }
    let nested = engine(&[]);
    let snapshot = nested.snapshot();
    let query = SparqlParser::new()
        .parse_query(&format!(
            "SELECT * WHERE {{ SERVICE SILENT <{REMOTE}> {{ BIND(1 AS ?x) }} }}"
        ))
        .unwrap();
    for memory in [true, false] {
        let budget = Arc::new(nrese_exec::Budget::new(1 << 20));
        let token = CancellationToken::new();
        let leaf = Arc::new(Leaf {
            actuator: token.clone(),
            root: Arc::clone(&budget),
            memory,
            called: std::sync::atomic::AtomicBool::new(false),
        });
        let mut options = QueryOptions::default();
        options.resources.memory = nrese_exec::Envelope::from_budget(Arc::clone(&budget));
        options.resources.cancellation = Some(token.clone());
        options.services = Some(Services(leaf.clone()));
        assert!(std::ptr::eq(&*options.resources.memory, &*budget));
        let result = evaluate_query(&snapshot, &query, &options);
        assert!(leaf.called.load(std::sync::atomic::Ordering::Acquire));
        if memory {
            assert!(
                matches!(&result, Err(QueryEvaluationError::MemoryLimit(e)) if e.limit == 1 << 20)
            );
            assert!(!token.is_cancelled());
        } else {
            assert!(matches!(&result, Err(QueryEvaluationError::Cancelled)));
            assert!(token.is_cancelled());
        }
        drop(result);
        assert_eq!(budget.used(), 0);
    }
}
