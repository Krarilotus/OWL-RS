//! Every way a query names a term of an `owl:sameAs` class, in every equality answer mode,
//! optimised and as written, against the reference evaluator: strict answers (late and
//! early expansion) against it over the expanded closure, canonical answers against its
//! own quotient of the expanded closure (`nrese-sparql-reference`'s `quotient`), bags
//! compared exactly. Canonical answers refuse what they can't answer (research-designs §1,
//! "Canonical answers").

use std::sync::Arc;

use nrese_engine::{EncodedTriple, Engine, EngineConfig, Snapshot};
use nrese_rdf::{GraphName, NamedNode, Quad, Term};
use nrese_sparql::{
    CancellationToken, QueryEvaluationError, QueryOptions, QueryResults, ServiceClient,
    ServiceResults, Services, evaluate_query,
};
use nrese_sparql_syntax::SparqlParser;

const EX: &str = "http://example.com/";

fn ex(local: &str) -> NamedNode {
    NamedNode::new_unchecked(format!("{EX}{local}"))
}

/// `Rep ex:p ex:o`, `ex:s ex:q Rep`, `Alias owl:sameAs Rep` inferred (the closure over
/// representatives, `Rep` the smaller id). With `named`, also `Alias ex:in ex:z` in the
/// graphs `ex:g` and `ex:Alias`, and `ex:meta ex:quoted <<( Alias ex:p ex:o )>>` (named
/// graphs turn late expansion off, so the late mode is exercised without them).
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
        quads.push(Quad::new(ex("Alias"), ex("in"), ex("z"), ex("Alias")));
        let quoted = nrese_rdf::Triple::new(ex("Alias"), ex("p"), ex("o"));
        quads.push(Quad::new(
            ex("meta"),
            ex("quoted"),
            quoted,
            GraphName::DefaultGraph,
        ));
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

/// An endpoint holding `Alias ex:r ex:z` and `Rep ex:r ex:z`, which knows no classes.
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

/// The answer as sorted rows (solutions as sorted `var=term`, or triples), or the error.
fn answer(results: Result<QueryResults<'_>, QueryEvaluationError>) -> Result<Vec<String>, String> {
    let mut rows: Vec<String> = match results.map_err(|e| e.to_string())? {
        QueryResults::Solutions(solutions) => solutions
            .map(|solution| {
                let solution = solution.unwrap();
                let mut row: Vec<String> =
                    solution.iter().map(|(v, t)| format!("{v}={t}")).collect();
                row.sort();
                row.join(" ")
            })
            .collect(),
        QueryResults::Boolean(b) => vec![b.to_string()],
        QueryResults::Graph(triples) => triples.map(|t| t.unwrap().to_string()).collect(),
    };
    rows.sort();
    Ok(rows.into_iter().map(|r| r.replace(EX, "")).collect())
}

fn representative_of(snapshot: &Snapshot, term: &str) -> Term {
    let classes = snapshot.equality_classes().unwrap();
    let id = snapshot.lookup(ex(term).as_ref().into()).unwrap();
    snapshot.decode(classes.representative(id)).unwrap()
}

#[test]
fn every_way_of_naming_a_term_answers_by_its_class_in_every_mode() {
    // (case, query, the reference's query where it can't evaluate the case's, fixture with
    // named graphs, canonical answers refuse it)
    #[rustfmt::skip]
    let cases: &[(&str, &str, Option<&str>, bool, bool)] = &[
        ("inline, probe", "SELECT * { ex:Alias ex:p ex:o }", None, false, false),
        ("inline, object", "SELECT * { ex:s ex:q ex:Alias }", None, false, false),
        ("inline, join", "SELECT ?o { ex:Alias ex:p ?o . ex:s ex:q ex:Alias }", None, false, false),
        ("VALUES, scan", "SELECT ?x { VALUES ?x { ex:Alias } ?x ex:p ex:o }", None, false, false),
        ("VALUES, join", "SELECT ?x { VALUES ?x { ex:Alias } ?x ex:p ?o . ?s ex:q ?x }", None, false, false),
        ("=, scan", "SELECT ?x { ?x ex:p ex:o FILTER(?x = ex:Alias) }", None, false, false),
        ("=, join", "SELECT ?x { ?x ex:p ?o . ?s ex:q ?x FILTER(?x = ex:Alias) }", None, false, false),
        ("sameTerm", "SELECT ?x { ?x ex:p ex:o FILTER(sameTerm(?x, ex:Alias)) }", None, false, false),
        ("IN", "SELECT ?y { ex:s ex:q ?y FILTER(?y IN (ex:none, ex:Alias)) }", None, false, false),
        ("!=", "SELECT ?x { ?x ex:p ex:o FILTER(?x != ex:Alias) }", None, false, false),
        ("NOT IN", "SELECT ?x { ?x ex:p ex:o FILTER(?x NOT IN (ex:Alias)) }", None, false, false),
        ("BIND, join", "SELECT ?x { BIND(ex:Alias AS ?x) ?x ex:p ex:o }", None, false, false),
        ("BIND IRI(), join", "SELECT ?x { BIND(IRI(CONCAT(\"http://example.com/\", \"Alias\")) AS ?x) ?x ex:p ex:o }", None, false, false),
        ("= IRI()", "SELECT ?x { ?x ex:p ex:o FILTER(?x = IRI(\"http://example.com/Alias\")) }", None, false, false),
        ("STR", "SELECT ?s { BIND(STR(ex:Alias) AS ?s) }", None, false, false),
        ("STR of IRI()", "SELECT ?s { BIND(STR(IRI(\"http://example.com/Alias\")) AS ?s) }", None, false, false),
        ("STR =", "SELECT * { FILTER(STR(ex:Alias) = \"http://example.com/Rep\") }", None, false, false),
        ("STRENDS", "SELECT * { FILTER(STRENDS(STR(ex:Alias), \"Rep\")) }", None, false, false),
        ("subquery", "SELECT ?x { { SELECT ?x { VALUES ?x { ex:Alias } } } ?x ex:p ex:o }", None, false, false),
        ("OPTIONAL", "SELECT ?x ?o { VALUES ?x { ex:Alias } OPTIONAL { ?x ex:p ?o FILTER(?x = ex:Alias) } }", None, false, false),
        ("MINUS", "SELECT ?x { ?x ex:p ex:o MINUS { VALUES ?x { ex:Alias } } }", None, false, false),
        ("EXISTS", "SELECT ?x { ?x ex:p ex:o FILTER EXISTS { ex:s ex:q ?x FILTER(?x = ex:Alias) } }", None, false, false),
        ("NOT EXISTS", "SELECT ?x { ?x ex:p ex:o FILTER NOT EXISTS { ex:s ex:q ?x FILTER(?x = ex:Alias) } }", None, false, false),
        ("path, subject", "SELECT ?o { ex:Alias ex:p+ ?o }", None, false, false),
        ("path, object", "SELECT ?s { ?s ex:q+ ex:Alias }", None, false, false),
        ("path, zero length", "SELECT ?s { ex:Alias ex:p* ?s }", None, false, false),
        ("DISTINCT", "SELECT DISTINCT ?x { VALUES ?x { ex:Alias ex:Rep } }", None, false, false),
        ("COUNT DISTINCT", "SELECT (COUNT(DISTINCT ?x) AS ?n) { VALUES ?x { ex:Alias ex:Rep } }", None, false, false),
        ("COUNT, bag", "SELECT (COUNT(?x) AS ?n) { VALUES ?x { ex:Alias ex:Rep } }", None, false, false),
        ("ORDER BY STR", "SELECT ?x { VALUES ?x { ex:Alias ex:Rep } } ORDER BY STR(?x)", None, false, false),
        ("DESCRIBE", "DESCRIBE ex:Alias", None, false, false),
        ("CONSTRUCT", "CONSTRUCT { ex:Alias ex:out ?x } { ?x ex:p ex:o }", None, false, false),
        ("CONSTRUCT VALUES", "CONSTRUCT { ?x ex:out ex:Alias } { VALUES ?x { ex:Alias } }", None, false, false),
        ("SERVICE, joined", "SELECT ?x { ?x ex:p ex:o SERVICE <http://remote.example/sparql> { ?x ex:r ex:z } }",
            Some("SELECT ?x { ?x ex:p ex:o VALUES ?x { ex:Alias ex:Rep } }"), false, false),
        ("SERVICE, own filter", "SELECT ?x { SERVICE <http://remote.example/sparql> { ?x ex:r ex:z FILTER(?x = ex:Alias) } ?x ex:p ex:o }",
            Some("SELECT ?x { VALUES ?x { ex:Alias } ?x ex:p ex:o }"), false, false),
        ("GRAPH, inner =", "SELECT ?x { GRAPH ex:g { ?x ex:in ex:z FILTER(?x = ex:Alias) } }", None, true, false),
        ("GRAPH, outer =", "SELECT ?x { GRAPH ex:g { ?x ex:in ex:z } FILTER(?x = ex:Alias) }", None, true, false),
        ("GRAPH, outer !=", "SELECT ?x { GRAPH ex:g { ?x ex:in ex:z } FILTER(?x != ex:Alias) }", None, true, false),
        ("GRAPH, outer STR", "SELECT ?x { GRAPH ex:g { ?x ex:in ex:z } FILTER(STR(?x) = \"http://example.com/Rep\") }", None, true, false),
        ("GRAPH, outer sameTerm", "SELECT ?x { GRAPH ex:g { ?x ex:in ex:z } FILTER(sameTerm(?x, ex:Alias)) }", None, true, false),
        ("GRAPH, outer IRI()", "SELECT ?x { GRAPH ex:g { ?x ex:in ex:z } FILTER(?x = IRI(\"http://example.com/Alias\")) }", None, true, false),
        ("GRAPH, inner constant", "SELECT ?g { GRAPH ?g { ex:Rep ex:in ex:z } }", None, true, false),
        ("GRAPH named in a class", "SELECT ?x { GRAPH ex:Alias { ?x ex:in ex:z } }", None, true, false),
        ("GRAPH, then default", "SELECT ?x { GRAPH ex:g { ?x ex:in ex:z } ?x ex:p ex:o }", None, true, false),
        ("default, then GRAPH", "SELECT ?x { ?x ex:p ex:o GRAPH ex:g { ?x ex:in ex:z } }", None, true, false),
        ("GRAPH in EXISTS", "SELECT ?x { ?x ex:p ex:o FILTER EXISTS { GRAPH ex:g { ?x ex:in ex:z } } }", None, true, false),
        ("GRAPH in MINUS", "SELECT ?x { ?x ex:p ex:o MINUS { GRAPH ex:g { ?x ex:in ex:z } } }", None, true, false),
        ("GRAPH ?g, VALUES", "SELECT ?g { VALUES ?g { ex:Alias } GRAPH ?g { ?x ex:in ex:z } }", None, true, false),
        ("FROM a named graph", "SELECT ?x FROM ex:g { ?x ex:in ex:z }", None, true, false),
        ("union default graph", "SELECT ?x { ?x ex:in ?z . ?x ex:p ex:o }", None, true, false),
        ("triple term, value", "SELECT ?t { ex:meta ex:quoted ?t }", None, true, false),
        ("SUBJECT", "SELECT ?x { ex:meta ex:quoted ?t BIND(SUBJECT(?t) AS ?x) ?x ex:p ex:o }", None, true, true),
        ("triple pattern, open", "SELECT ?s { ex:meta ex:quoted <<( ?s ex:p ex:o )>> }", None, true, true),
    ];
    let remote = Engine::new(EngineConfig::default()).unwrap();
    let mut tx = remote.transaction();
    for s in ["Alias", "Rep"] {
        tx.insert(Quad::new(ex(s), ex("r"), ex("z"), GraphName::DefaultGraph).as_ref());
    }
    tx.commit().unwrap();
    let services = Services(Arc::new(Endpoint(remote)));
    let engines = [engine(false), engine(true)];
    let parse = |text: &str| {
        SparqlParser::new()
            .parse_query(&format!("PREFIX ex: <{EX}> {text}"))
            .unwrap()
    };
    let modes = [
        ("late", false, false),
        ("early", true, false),
        ("canonical", false, true),
    ];
    let mut wrong = Vec::new();
    for &(case, query, reference_query, named, refused) in cases {
        let snapshot = engines[usize::from(named)].snapshot();
        assert_eq!(representative_of(&snapshot, "Alias"), ex("Rep").into());
        for (mode, early, canonical) in modes {
            for as_written in [false, true] {
                let options = QueryOptions {
                    equality_early_expansion: early,
                    equality_canonical: canonical,
                    as_written,
                    union_default_graph: case == "union default graph",
                    services: Some(services.clone()),
                    ..QueryOptions::default()
                };
                let got = answer(evaluate_query(&snapshot, &parse(query), &options));
                let expected = match canonical && refused {
                    true => Err("refused".to_owned()),
                    false => answer(nrese_sparql_reference::evaluate_query(
                        &snapshot,
                        &parse(reference_query.unwrap_or(query)),
                        &options,
                    )),
                };
                let agrees = match (&got, &expected) {
                    (Err(error), Err(_)) => error.contains("canonical equality answers"),
                    (got, expected) => got == expected,
                };
                if !agrees {
                    wrong.push(format!(
                        "{mode:9} as_written={as_written:5} {case}: {got:?}, expected {expected:?}"
                    ));
                }
            }
        }
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
}
