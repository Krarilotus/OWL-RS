//! What the engine names for itself is apart from every name a query or a store can hold
//! (Q-01). A pattern's blank node is an existential variable of its own (SPARQL 1.1
//! §18.3: "blank nodes in a basic graph pattern act as variables"), never equal to a
//! variable however that variable is spelt; a variable the executor makes for a triple
//! term is never one the query names; and an IRI is the resource it names (RDF 1.1
//! Concepts §3.2), wherever it sits in the IRI space, never a blank node put in for a
//! variable.
//!
//! Each case states its answer by those rules alone, and is checked natively as optimised
//! and as written, and where it reads one, with the result cache on.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use nrese_engine::{Engine, EngineConfig};
use nrese_rdf::{GraphName, NamedNode, Quad, Term, Triple, Variable};
use nrese_sparql::{
    QueryEvaluationError, QueryOptions, QueryResults, ResultCache, ServiceCall, ServiceClient,
    ServiceResults, Services, evaluate_query,
};
use nrese_sparql_syntax::SparqlParser;

const REIFIES: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#reifies";
const SENTINEL: &str = "urn:nrese:substituted-blank-node:";

/// Spec (SPARQL 1.1 §16.2): an unbound template variable emits no triple.
#[test]
fn e1_followup_construct_plain_blank_scope() {
    let engine = engine(&[
        (iri("urn:s").into(), "urn:p", iri("urn:o").into()),
        (iri("urn:x").into(), "urn:q", iri("urn:o").into()),
    ]);
    let snapshot = engine.snapshot();
    let query = SparqlParser::new()
        .parse_query("CONSTRUCT { ?s <urn:r> ?_m0 } WHERE { ?s <urn:p> ?o . _:b <urn:q> ?o }")
        .unwrap();
    for form in [
        nrese_sparql::ql::PrintForm::Values,
        nrese_sparql::ql::PrintForm::Paths,
    ] {
        let printed = nrese_sparql::ql::print(&snapshot, &query, form).unwrap();
        let nrese_sparql::ql::Printed::Query { text, .. } = printed else {
            panic!("expressible")
        };
        let query = SparqlParser::new().parse_query(&text).unwrap();
        let QueryResults::Graph(rows) =
            nrese_sparql_reference::evaluate_query(&snapshot, &query, &QueryOptions::default())
                .unwrap()
        else {
            panic!("CONSTRUCT")
        };
        assert_eq!(rows.map(Result::unwrap).count(), 0, "{text}");
    }
}

/// Spec: a ground triple pre-binding denotes the same RDF1.2 value at a SERVICE (IB0313).
#[test]
fn e1_followup_service_sends_a_ground_triple_value() {
    let triple: Term = Triple::new(iri("urn:s"), iri("urn:p"), iri("urn:o")).into();
    let expected = triple.to_string();
    let remote = engine(&[(iri("urn:one").into(), REIFIES, triple.clone())]);
    let endpoint = Arc::new(Endpoint(remote, Mutex::default()));
    let options = QueryOptions {
        pre_bound: Some(Arc::new(HashMap::from([(
            Variable::new_unchecked("this"),
            triple,
        )]))),
        services: Some(Services(endpoint.clone())),
        ..QueryOptions::default()
    };
    assert_rows(
        &engine(&[]),
        &format!(
            "SELECT ?r WHERE {{ SERVICE <http://remote.example/sparql> {{ ?r <{REIFIES}> $this }} }}"
        ),
        &options,
        &["<urn:one>"],
    );
    assert_rows(
        &engine(&[]),
        "SELECT ?r WHERE { SERVICE <http://remote.example/sparql> { BIND($this AS ?r) } }",
        &options,
        &[&expected],
    );
}

fn iri(text: &str) -> NamedNode {
    NamedNode::new_unchecked(text)
}

fn engine(quads: &[(Term, &str, Term)]) -> Engine {
    let engine = Engine::new(EngineConfig::default()).unwrap();
    let mut tx = engine.transaction();
    for (s, p, o) in quads {
        let subject = match s {
            Term::NamedNode(n) => nrese_rdf::NamedOrBlankNode::from(n.clone()),
            Term::BlankNode(b) => b.clone().into(),
            other => panic!("not a subject: {other}"),
        };
        tx.insert(Quad::new(subject, iri(p), o.clone(), GraphName::DefaultGraph).as_ref());
    }
    tx.commit().unwrap();
    engine
}

/// `<urn:a> <urn:p> <urn:b>` and `<urn:c> <urn:q> <urn:d>`: two subjects, apart.
fn two_subjects() -> Engine {
    engine(&[
        (iri("urn:a").into(), "urn:p", iri("urn:b").into()),
        (iri("urn:c").into(), "urn:q", iri("urn:d").into()),
    ])
}

/// The rows of `text` under `options`, each row its values in projection order.
fn rows_with(engine: &Engine, text: &str, options: &QueryOptions) -> Vec<String> {
    let query = SparqlParser::new()
        .parse_query(text)
        .unwrap_or_else(|e| panic!("{e}: {text}"));
    let snapshot = engine.snapshot();
    let QueryResults::Solutions(solutions) = evaluate_query(&snapshot, &query, options).unwrap()
    else {
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
    out
}

/// `text`'s rows natively, optimised and as written, each checked against `expected`.
fn assert_rows(engine: &Engine, text: &str, base: &QueryOptions, expected: &[&str]) {
    for as_written in [false, true] {
        let options = QueryOptions {
            as_written,
            ..base.clone()
        };
        assert_eq!(
            rows_with(engine, text, &options),
            expected,
            "as_written = {as_written}: {text}"
        );
    }
}

#[test]
fn a_pattern_blank_node_is_apart_from_a_variable_spelt_like_its_name() {
    assert_rows(
        &two_subjects(),
        "SELECT ?o ?z WHERE { _:x <urn:p> ?o . ?_bnode_x <urn:q> ?z }",
        &QueryOptions::default(),
        &["<urn:b> <urn:d>"],
    );
}

#[test]
fn a_path_blank_node_is_apart_from_a_variable_spelt_like_its_name() {
    assert_rows(
        &two_subjects(),
        "SELECT ?o ?z WHERE { _:x <urn:p>+ ?o . ?_bnode_x <urn:q> ?z }",
        &QueryOptions::default(),
        &["<urn:b> <urn:d>"],
    );
}

/// Two queries that differ in a blank node against a variable have their own answers,
/// in either order through one cache.
#[test]
fn the_result_cache_tells_a_blank_node_from_a_variable_spelt_like_its_name() {
    let engine = two_subjects();
    let blank = "SELECT ?o ?z WHERE { _:x <urn:p> ?o . ?_bnode_x <urn:q> ?z }";
    let variable = "SELECT ?o ?z WHERE { ?_bnode_x <urn:p> ?o . ?_bnode_x <urn:q> ?z }";
    for order in [[blank, variable], [variable, blank]] {
        let options = QueryOptions {
            result_cache: Some(Arc::new(ResultCache::new(1 << 20).admitting_all())),
            ..QueryOptions::default()
        };
        for text in order {
            let expected: &[&str] = if text == blank {
                &["<urn:b> <urn:d>"]
            } else {
                &[]
            };
            assert_eq!(rows_with(&engine, text, &options), expected, "{text}");
        }
    }
}

/// The audit's second case: `?_triple_term_0` is the query's, bound to `<urn:z>`; the
/// triple term in the reifier's pattern is another term.
#[test]
fn a_triple_term_pattern_is_apart_from_a_variable_spelt_like_its_lowering() {
    let triple = Triple::new(iri("urn:s"), iri("urn:p"), iri("urn:o"));
    let engine = engine(&[
        (iri("urn:r").into(), REIFIES, triple.into()),
        (iri("urn:a").into(), "urn:q", iri("urn:z").into()),
    ]);
    assert_rows(
        &engine,
        &format!(
            "SELECT ?s ?_triple_term_0 WHERE {{ ?r <{REIFIES}> <<( ?s <urn:p> ?o )>> . \
             <urn:a> <urn:q> ?_triple_term_0 }}"
        ),
        &QueryOptions::default(),
        &["<urn:s> <urn:z>"],
    );
}

#[test]
fn a_stored_iri_is_found_wherever_it_sits_in_the_iri_space() {
    let stored = format!("{SENTINEL}x");
    let engine = engine(&[(iri("urn:s").into(), "urn:p", iri(&stored).into())]);
    assert_rows(
        &engine,
        &format!("SELECT ?s WHERE {{ ?s <urn:p> <{stored}> }}"),
        &QueryOptions::default(),
        &["<urn:s>"],
    );
}

#[test]
fn an_iri_in_an_expression_is_itself_wherever_it_sits_in_the_iri_space() {
    assert_rows(
        &two_subjects(),
        &format!("SELECT ?v WHERE {{ BIND(STR(<{SENTINEL}user>) AS ?v) }}"),
        &QueryOptions::default(),
        &[&format!("\"{SENTINEL}user\"")],
    );
}

/// SHACL pre-binding of a stored blank node: it is that node, and no IRI is the same term
/// (SPARQL 1.1 §17.4.1.8 sameTerm), whatever the IRI spells.
#[test]
fn a_pre_bound_blank_node_is_apart_from_every_iri() {
    let engine = engine(&[(
        nrese_rdf::BlankNode::new_unchecked("b7").into(),
        "urn:p",
        iri("urn:o").into(),
    )]);
    // The stored node, as the store names it.
    let snapshot = engine.snapshot();
    let query = SparqlParser::new()
        .parse_query("SELECT ?x WHERE { ?x <urn:p> <urn:o> }")
        .unwrap();
    let QueryResults::Solutions(mut solutions) =
        evaluate_query(&snapshot, &query, &QueryOptions::default()).unwrap()
    else {
        panic!("solutions")
    };
    let x = Variable::new_unchecked("x");
    let node = solutions.next().unwrap().unwrap().get(&x).unwrap().clone();
    let Term::BlankNode(label) = &node else {
        panic!("a blank node: {node}")
    };
    let options = QueryOptions {
        pre_bound: Some(Arc::new(HashMap::from([(x, node.clone())]))),
        ..QueryOptions::default()
    };
    let text = format!(
        "SELECT ?o WHERE {{ ?x <urn:p> ?o FILTER(sameTerm(?x, <{SENTINEL}{}>)) }}",
        label.as_str()
    );
    assert_rows(&engine, &text, &options, &[]);
}

/// The reference evaluator, the native executor's oracle, keeps the same identity apart
/// on its own.
#[test]
fn the_reference_tells_a_blank_node_from_a_variable_spelt_like_its_name() {
    let engine = two_subjects();
    let snapshot = engine.snapshot();
    let query = SparqlParser::new()
        .parse_query("SELECT ?o ?z WHERE { _:x <urn:p> ?o . ?__blank_x <urn:q> ?z }")
        .unwrap();
    let QueryResults::Solutions(solutions) =
        nrese_sparql_reference::evaluate_query(&snapshot, &query, &QueryOptions::default())
            .unwrap()
    else {
        panic!("solutions")
    };
    let variables = solutions.variables().to_vec();
    let rows: Vec<String> = solutions
        .map(|s| {
            let s = s.unwrap();
            variables
                .iter()
                .map(|v| s.get(v).map_or("-".to_owned(), ToString::to_string))
                .collect::<Vec<_>>()
                .join(" ")
        })
        .collect();
    assert_eq!(rows, ["<urn:b> <urn:d>"]);
}

/// An endpoint that is an engine in the same process.
struct Endpoint(Engine, Mutex<Vec<String>>);

impl ServiceClient for Endpoint {
    fn select(
        &self,
        _endpoint: &str,
        query: &str,
        _call: ServiceCall<'_>,
    ) -> Result<ServiceResults, QueryEvaluationError> {
        self.1.lock().unwrap().push(query.to_owned());
        let parsed = SparqlParser::new()
            .parse_query(query)
            .map_err(|error| QueryEvaluationError::Service(error.into()))?;
        let snapshot = self.0.snapshot();
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

/// What a SERVICE pattern sends keeps the query's variables apart from what the executor
/// makes of a triple term pattern: the endpoint answers the pattern as written.
#[test]
fn a_service_pattern_keeps_a_triple_term_apart_from_a_variable_spelt_like_its_lowering() {
    let triple = Triple::new(iri("urn:s"), iri("urn:p"), iri("urn:o"));
    let remote = engine(&[
        (iri("urn:r").into(), REIFIES, triple.into()),
        (iri("urn:a").into(), "urn:q", iri("urn:z").into()),
    ]);
    let endpoint = Arc::new(Endpoint(remote, Mutex::default()));
    let options = QueryOptions {
        services: Some(Services(endpoint.clone())),
        ..QueryOptions::default()
    };
    let text = format!(
        "SELECT ?s ?_triple_term_0 WHERE {{ SERVICE <http://remote.example/sparql> {{ \
         ?r <{REIFIES}> <<( ?s <urn:p> ?o )>> . <urn:a> <urn:q> ?_triple_term_0 }} }}"
    );
    assert_rows(&engine(&[]), &text, &options, &["<urn:s> <urn:z>"]);
}

/// A blank node of a SERVICE pattern is one node in what the endpoint gets, where the
/// executor's reading of a triple term pattern beside it names the node too: `_:x` is
/// `<urn:s>` in both triples (SPARQL 1.1 §18.3, one blank node, one variable).
#[test]
fn a_service_pattern_keeps_a_blank_node_one_node_beside_a_triple_term() {
    let triple = Triple::new(iri("urn:s"), iri("urn:p"), iri("urn:o"));
    let remote = engine(&[
        (iri("urn:s").into(), "urn:p", iri("urn:o").into()),
        (iri("urn:r").into(), REIFIES, triple.into()),
    ]);
    let endpoint = Arc::new(Endpoint(remote, Mutex::default()));
    let options = QueryOptions {
        services: Some(Services(endpoint.clone())),
        ..QueryOptions::default()
    };
    let text = format!(
        "SELECT ?o WHERE {{ SERVICE <http://remote.example/sparql> {{ \
         _:x <urn:p> ?o . ?r <{REIFIES}> <<( _:x <urn:p> ?o )>> }} }}"
    );
    assert_rows(&engine(&[]), &text, &options, &["<urn:o>"]);
}

/// SERVICE options over an endpoint holding `quads`, with `variable` pre-bound to `value`.
fn remote_with(
    quads: &[(Term, &str, Term)],
    variable: &str,
    value: Term,
) -> (QueryOptions, Arc<Endpoint>) {
    let endpoint = Arc::new(Endpoint(engine(quads), Mutex::default()));
    let options = QueryOptions {
        services: Some(Services(endpoint.clone())),
        pre_bound: Some(Arc::new(HashMap::from([(
            Variable::new_unchecked(variable),
            value,
        )]))),
        ..QueryOptions::default()
    };
    (options, endpoint)
}

/// A literal pre-bound where only an IRI may stand matches nothing (no triple has a
/// literal predicate), at an endpoint too, whatever IRIs the endpoint holds.
#[test]
fn a_pre_bound_literal_in_an_iri_position_matches_nothing_at_an_endpoint() {
    let (options, _) = remote_with(
        &[(
            iri("urn:s").into(),
            &format!("{SENTINEL}nothing"),
            iri("urn:o").into(),
        )],
        "p",
        nrese_rdf::Literal::new_simple_literal("p").into(),
    );
    let text = "SELECT ?s WHERE { SERVICE <http://remote.example/sparql> { ?s ?p <urn:o> } }";
    assert_rows(&engine(&[]), text, &options, &[]);
}

/// A local blank node pre-bound into a SERVICE pattern matches nothing there: an
/// endpoint's terms are its own, and no IRI is that node, whatever it spells.
#[test]
fn a_pre_bound_blank_node_matches_nothing_at_an_endpoint() {
    let (options, _) = remote_with(
        &[(
            iri("urn:s").into(),
            "urn:p",
            iri(&format!("{SENTINEL}blank-b7")).into(),
        )],
        "o",
        nrese_rdf::BlankNode::new_unchecked("b7").into(),
    );
    let text = "SELECT ?s WHERE { SERVICE <http://remote.example/sparql> { ?s <urn:p> ?o } }";
    assert_rows(&engine(&[]), text, &options, &[]);
}

/// An endpoint pre-bound to a blank node or a literal is no IRI: the SERVICE fails, or
/// under SILENT gives one empty solution, as a variable endpoint without an IRI does; no
/// client is called with it.
#[test]
fn a_pre_bound_endpoint_that_is_no_iri_is_never_called() {
    let quads = [(iri("urn:s").into(), "urn:p", iri("urn:o").into())];
    for value in [
        Term::from(nrese_rdf::BlankNode::new_unchecked("b7")),
        nrese_rdf::Literal::new_simple_literal("x").into(),
    ] {
        let (options, endpoint) = remote_with(&quads, "e", value.clone());
        let failing = SparqlParser::new()
            .parse_query("SELECT ?s WHERE { SERVICE ?e { ?s <urn:p> <urn:o> } }")
            .unwrap();
        let local = engine(&[]);
        let snapshot = local.snapshot();
        match evaluate_query(&snapshot, &failing, &options) {
            Err(QueryEvaluationError::Service(_)) => {}
            Err(other) => panic!("{value}: {other}"),
            Ok(_) => panic!("{value}: answered"),
        }
        let silent = "SELECT ?s WHERE { SERVICE SILENT ?e { ?s <urn:p> <urn:o> } }";
        assert_rows(&local, silent, &options, &["-"]);
        assert_eq!(endpoint.1.lock().unwrap().len(), 0, "{value}: called");
    }
}
