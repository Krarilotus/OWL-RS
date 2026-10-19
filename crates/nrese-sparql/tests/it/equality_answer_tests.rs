//! Every way a query names a term of an `owl:sameAs` class, in every equality answer mode,
//! against the reference evaluator over the explicitly expanded closure. Strict modes
//! (late and early expansion) give its answers; canonical answers give them with each
//! class shown by its representative, each solution once (research-designs §1,
//! "Canonical answers").

use std::sync::Arc;

use nrese_engine::{EncodedTriple, Engine, EngineConfig, Snapshot};
use nrese_rdf::{GraphName, NamedNode, Quad, Term};
use nrese_sparql::{
    CancellationToken, QueryOptions, QueryResults, ServiceClient, ServiceResults, Services,
    evaluate_query,
};
use nrese_sparql_syntax::SparqlParser;

const EX: &str = "http://example.com/";
const REMOTE: &str = "http://remote.example/sparql";

fn ex(local: &str) -> NamedNode {
    NamedNode::new_unchecked(format!("{EX}{local}"))
}

/// `Rep ex:p ex:o`, `ex:s ex:q Rep`, `Alias owl:sameAs Rep` inferred (the closure over
/// representatives, `Rep` the smaller id); with `named`, also `GRAPH ex:g { Alias ex:in
/// ex:z }`.
fn engine(named: bool) -> Engine {
    let engine = Engine::new(EngineConfig::default()).unwrap();
    let mut tx = engine.transaction();
    let rep = tx.intern(ex("Rep").as_ref().into());
    let alias = tx.intern(ex("Alias").as_ref().into());
    let same_as = NamedNode::new_unchecked("http://www.w3.org/2002/07/owl#sameAs");
    let same_as = tx.intern(same_as.as_ref().into());
    let mut quads = vec![
        Quad::new(ex("Rep"), ex("p"), ex("o"), GraphName::DefaultGraph),
        Quad::new(ex("s"), ex("q"), ex("Rep"), GraphName::DefaultGraph),
    ];
    if named {
        quads.push(Quad::new(ex("Alias"), ex("in"), ex("z"), ex("g")));
    }
    for quad in &quads {
        tx.insert(quad.as_ref());
    }
    tx.insert_inferred(EncodedTriple {
        subject: alias,
        predicate: same_as,
        object: rep,
    });
    tx.commit().unwrap();
    engine.set_equality(Some(same_as));
    engine
}

/// An endpoint holding `ex:t ex:r Alias`.
struct Endpoint(Engine);

impl ServiceClient for Endpoint {
    fn select(
        &self,
        _endpoint: &str,
        query: &str,
        _cancellation: Option<&CancellationToken>,
    ) -> Result<ServiceResults, Box<dyn std::error::Error + Send + Sync>> {
        let parsed = SparqlParser::new().parse_query(query)?;
        let snapshot = self.0.snapshot();
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

/// The solutions of `results`, each as its bindings.
fn solutions(results: QueryResults<'_>) -> Vec<Vec<(String, Term)>> {
    let QueryResults::Solutions(solutions) = results else {
        panic!("solutions")
    };
    solutions
        .map(|solution| {
            let solution = solution.unwrap();
            solution
                .iter()
                .map(|(v, t)| (v.to_string(), t.clone()))
                .collect()
        })
        .collect()
}

/// `solutions` as sorted rows of sorted `var=term`, each term renamed by `rename`.
fn rows(solutions: &[Vec<(String, Term)>], rename: &dyn Fn(Term) -> Term) -> Vec<String> {
    let mut out: Vec<String> = solutions
        .iter()
        .map(|solution| {
            let mut row: Vec<String> = solution
                .iter()
                .map(|(v, t)| format!("{v}={}", rename(t.clone())).replace(EX, ""))
                .collect();
            row.sort();
            row.join(" ")
        })
        .collect();
    out.sort();
    out
}

/// A term as canonical answers show it: its class's representative.
fn representative(snapshot: &Snapshot, term: Term) -> Term {
    let classes = snapshot.equality_classes().unwrap();
    match snapshot.lookup(term.as_ref()) {
        Some(id) => snapshot.decode(classes.representative(id)).unwrap(),
        None => term,
    }
}

#[test]
fn every_way_of_naming_a_term_answers_by_its_class_in_every_mode() {
    let a = "<http://example.com/Alias>";
    // (case, query, the reference's query if it can't evaluate the case's, named graph)
    let cases: &[(&str, String, Option<String>, bool)] = &[
        // Inline constants: the control, normalised by the reads.
        (
            "inline, subject probe",
            format!("SELECT * {{ {a} ex:p ex:o }}"),
            None,
            false,
        ),
        (
            "inline, object probe",
            format!("SELECT * {{ ex:s ex:q {a} }}"),
            None,
            false,
        ),
        (
            "inline, join",
            format!("SELECT ?o {{ {a} ex:p ?o . ex:s ex:q {a} }}"),
            None,
            false,
        ),
        (
            "VALUES, subject",
            format!("SELECT ?x {{ VALUES ?x {{ {a} }} ?x ex:p ex:o }}"),
            None,
            false,
        ),
        (
            "VALUES, object",
            format!("SELECT ?y {{ VALUES ?y {{ {a} }} ex:s ex:q ?y }}"),
            None,
            false,
        ),
        (
            "VALUES, join",
            format!("SELECT ?x {{ VALUES ?x {{ {a} }} ?x ex:p ?o . ?s ex:q ?x }}"),
            None,
            false,
        ),
        (
            "FILTER =, subject scan",
            format!("SELECT ?x {{ ?x ex:p ex:o FILTER(?x = {a}) }}"),
            None,
            false,
        ),
        (
            "FILTER =, object scan",
            format!("SELECT ?y {{ ex:s ex:q ?y FILTER(?y = {a}) }}"),
            None,
            false,
        ),
        (
            "FILTER =, join",
            format!("SELECT ?x {{ ?x ex:p ?o . ?s ex:q ?x FILTER(?x = {a}) }}"),
            None,
            false,
        ),
        (
            "FILTER sameTerm",
            format!("SELECT ?x {{ ?x ex:p ex:o FILTER(sameTerm(?x, {a})) }}"),
            None,
            false,
        ),
        (
            "FILTER IN",
            format!("SELECT ?y {{ ex:s ex:q ?y FILTER(?y IN (ex:none, {a})) }}"),
            None,
            false,
        ),
        (
            "BIND, then join",
            format!("SELECT ?x {{ BIND({a} AS ?x) ?x ex:p ex:o }}"),
            None,
            false,
        ),
        (
            "BIND IRI(), then join",
            "SELECT ?x { BIND(IRI(\"http://example.com/Alias\") AS ?x) ?x ex:p ex:o }".into(),
            None,
            false,
        ),
        (
            "FILTER = IRI()",
            "SELECT ?x { ?x ex:p ex:o FILTER(?x = IRI(\"http://example.com/Alias\")) }".into(),
            None,
            false,
        ),
        (
            "subquery binding",
            format!("SELECT ?x {{ {{ SELECT ?x {{ BIND({a} AS ?x) }} }} ?x ex:p ex:o }}"),
            None,
            false,
        ),
        (
            "path, zero or more",
            format!("SELECT ?o {{ {a} ex:p* ?o }}"),
            None,
            false,
        ),
        (
            "SERVICE binding, join",
            format!("SELECT ?x {{ SERVICE <{REMOTE}> {{ ?t ex:r ?x }} ?x ex:p ex:o }}"),
            Some(format!(
                "SELECT ?x {{ VALUES (?t ?x) {{ (ex:t {a}) }} ?x ex:p ex:o }}"
            )),
            false,
        ),
        (
            "GRAPH, read as stored",
            format!("SELECT ?x {{ GRAPH ex:g {{ ?x ex:in ?z FILTER(?x = {a}) }} }}"),
            None,
            true,
        ),
        (
            "GRAPH binding, join",
            "SELECT ?x { GRAPH ex:g { ?x ex:in ?z } ?x ex:p ex:o }".into(),
            None,
            true,
        ),
    ];
    let remote = Engine::new(EngineConfig::default()).unwrap();
    let mut tx = remote.transaction();
    tx.insert(Quad::new(ex("t"), ex("r"), ex("Alias"), GraphName::DefaultGraph).as_ref());
    tx.commit().unwrap();
    let services = Services(Arc::new(Endpoint(remote)));
    let modes = [
        ("late", false, false),
        ("early", true, false),
        ("canonical", false, true),
    ];
    let engines = [engine(false), engine(true)];
    let parse = |text: &str| {
        SparqlParser::new()
            .parse_query(&format!("PREFIX ex: <{EX}> {text}"))
            .unwrap()
    };
    let mut wrong = Vec::new();
    for (case, query, reference_query, named) in cases {
        let snapshot = engines[usize::from(*named)].snapshot();
        let reference = solutions(
            nrese_sparql_reference::evaluate_query(
                &snapshot,
                &parse(reference_query.as_ref().unwrap_or(query)),
                &QueryOptions::default(),
            )
            .unwrap(),
        );
        assert!(!reference.is_empty(), "{case}: no rows");
        let strict = rows(&reference, &|t| t);
        let mut canonical = rows(&reference, &|t| representative(&snapshot, t));
        canonical.dedup();
        for (mode, early, canonical_answers) in modes {
            let options = QueryOptions {
                equality_early_expansion: early,
                equality_canonical: canonical_answers,
                services: Some(services.clone()),
                ..QueryOptions::default()
            };
            let got = rows(
                &solutions(evaluate_query(&snapshot, &parse(query), &options).unwrap()),
                &|t| t,
            );
            let expected = match canonical_answers {
                true => &canonical,
                false => &strict,
            };
            if got != *expected {
                wrong.push(format!("{mode:9} {case}: {got:?}, expected {expected:?}"));
            }
        }
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
}
