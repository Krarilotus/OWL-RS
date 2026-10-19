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

/// The fixtures: [`PLAIN`], [`NAMED`] (named graphs turn late expansion off, so the late
/// mode is exercised without them), [`QUOTED`] and [`ALIASED`].
const PLAIN: usize = 0;
/// Also `Alias ex:in ex:z` in the graphs `ex:g` and `ex:Alias`.
const NAMED: usize = 1;
/// Also `ex:meta ex:quoted <<( Rep ex:p ex:o )>>`: a triple term over representatives.
const QUOTED: usize = 2;
/// Also `ex:meta ex:quoted <<( Alias ex:p ex:o )>>`: a triple term naming an alias.
const ALIASED: usize = 3;

/// `Rep ex:p ex:o`, `ex:s ex:q Rep`, `Alias owl:sameAs Rep` inferred (the closure over
/// representatives, `Rep` the smaller id), and what `fixture` adds.
fn engine(fixture: usize) -> Engine {
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
    match fixture {
        NAMED => {
            quads.push(Quad::new(ex("Alias"), ex("in"), ex("z"), ex("g")));
            quads.push(Quad::new(ex("Alias"), ex("in"), ex("z"), ex("Alias")));
        }
        QUOTED | ALIASED => {
            let part = if fixture == QUOTED { "Rep" } else { "Alias" };
            let quoted = nrese_rdf::Triple::new(ex(part), ex("p"), ex("o"));
            let quad = Quad::new(ex("meta"), ex("quoted"), quoted, GraphName::DefaultGraph);
            quads.push(quad);
        }
        _ => {}
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

/// The answer as rows (solutions as sorted `var=term`, or triples), sorted unless
/// `ordered` (the query's ORDER BY decides their order), or the error.
fn answer(
    results: Result<QueryResults<'_>, QueryEvaluationError>,
    ordered: bool,
) -> Result<Vec<String>, String> {
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
    if !ordered {
        rows.sort();
    }
    Ok(rows.into_iter().map(|r| r.replace(EX, "")).collect())
}

fn representative_of(snapshot: &Snapshot, term: &str) -> Term {
    let classes = snapshot.equality_classes().unwrap();
    let id = snapshot.lookup(ex(term).as_ref().into()).unwrap();
    snapshot.decode(classes.representative(id)).unwrap()
}

#[test]
fn every_way_of_naming_a_term_answers_by_its_class_in_every_mode() {
    // (case, query, the reference's query where it can't evaluate the case's, fixture,
    // canonical answers refuse it)
    #[rustfmt::skip]
    let cases: &[(&str, &str, Option<&str>, usize, bool)] = &[
        ("inline, probe", "SELECT * { ex:Alias ex:p ex:o }", None, PLAIN, false),
        ("inline, object", "SELECT * { ex:s ex:q ex:Alias }", None, PLAIN, false),
        ("inline, join", "SELECT ?o { ex:Alias ex:p ?o . ex:s ex:q ex:Alias }", None, PLAIN, false),
        ("VALUES, scan", "SELECT ?x { VALUES ?x { ex:Alias } ?x ex:p ex:o }", None, PLAIN, false),
        ("VALUES, join", "SELECT ?x { VALUES ?x { ex:Alias } ?x ex:p ?o . ?s ex:q ?x }", None, PLAIN, false),
        ("=, scan", "SELECT ?x { ?x ex:p ex:o FILTER(?x = ex:Alias) }", None, PLAIN, false),
        ("=, join", "SELECT ?x { ?x ex:p ?o . ?s ex:q ?x FILTER(?x = ex:Alias) }", None, PLAIN, false),
        ("sameTerm", "SELECT ?x { ?x ex:p ex:o FILTER(sameTerm(?x, ex:Alias)) }", None, PLAIN, false),
        ("IN", "SELECT ?y { ex:s ex:q ?y FILTER(?y IN (ex:none, ex:Alias)) }", None, PLAIN, false),
        ("!=", "SELECT ?x { ?x ex:p ex:o FILTER(?x != ex:Alias) }", None, PLAIN, false),
        ("NOT IN", "SELECT ?x { ?x ex:p ex:o FILTER(?x NOT IN (ex:Alias)) }", None, PLAIN, false),
        ("BIND, join", "SELECT ?x { BIND(ex:Alias AS ?x) ?x ex:p ex:o }", None, PLAIN, false),
        ("BIND IRI(), join", "SELECT ?x { BIND(IRI(CONCAT(\"http://example.com/\", \"Alias\")) AS ?x) ?x ex:p ex:o }", None, PLAIN, false),
        ("= IRI()", "SELECT ?x { ?x ex:p ex:o FILTER(?x = IRI(\"http://example.com/Alias\")) }", None, PLAIN, false),
        ("STR", "SELECT ?s { BIND(STR(ex:Alias) AS ?s) }", None, PLAIN, false),
        ("STR of IRI()", "SELECT ?s { BIND(STR(IRI(\"http://example.com/Alias\")) AS ?s) }", None, PLAIN, false),
        ("STR =", "SELECT * { FILTER(STR(ex:Alias) = \"http://example.com/Rep\") }", None, PLAIN, false),
        ("STRENDS", "SELECT * { FILTER(STRENDS(STR(ex:Alias), \"Rep\")) }", None, PLAIN, false),
        ("subquery", "SELECT ?x { { SELECT ?x { VALUES ?x { ex:Alias } } } ?x ex:p ex:o }", None, PLAIN, false),
        ("OPTIONAL", "SELECT ?x ?o { VALUES ?x { ex:Alias } OPTIONAL { ?x ex:p ?o FILTER(?x = ex:Alias) } }", None, PLAIN, false),
        ("MINUS", "SELECT ?x { ?x ex:p ex:o MINUS { VALUES ?x { ex:Alias } } }", None, PLAIN, false),
        ("EXISTS", "SELECT ?x { ?x ex:p ex:o FILTER EXISTS { ex:s ex:q ?x FILTER(?x = ex:Alias) } }", None, PLAIN, false),
        ("NOT EXISTS", "SELECT ?x { ?x ex:p ex:o FILTER NOT EXISTS { ex:s ex:q ?x FILTER(?x = ex:Alias) } }", None, PLAIN, false),
        ("path, subject", "SELECT ?o { ex:Alias ex:p+ ?o }", None, PLAIN, false),
        ("path, object", "SELECT ?s { ?s ex:q+ ex:Alias }", None, PLAIN, false),
        ("path, zero length", "SELECT ?s { ex:Alias ex:p* ?s }", None, PLAIN, false),
        ("DISTINCT", "SELECT DISTINCT ?x { VALUES ?x { ex:Alias ex:Rep } }", None, PLAIN, false),
        ("COUNT DISTINCT", "SELECT (COUNT(DISTINCT ?x) AS ?n) { VALUES ?x { ex:Alias ex:Rep } }", None, PLAIN, false),
        ("COUNT, bag", "SELECT (COUNT(?x) AS ?n) { VALUES ?x { ex:Alias ex:Rep } }", None, PLAIN, false),
        ("ORDER BY STR", "SELECT ?x { VALUES ?x { ex:Alias ex:Rep } } ORDER BY STR(?x)", None, PLAIN, false),
        ("DESCRIBE", "DESCRIBE ex:Alias", None, PLAIN, false),
        ("CONSTRUCT", "CONSTRUCT { ex:Alias ex:out ?x } { ?x ex:p ex:o }", None, PLAIN, false),
        ("CONSTRUCT VALUES", "CONSTRUCT { ?x ex:out ex:Alias } { VALUES ?x { ex:Alias } }", None, PLAIN, false),
        ("SERVICE, joined", "SELECT ?x { ?x ex:p ex:o SERVICE <http://remote.example/sparql> { ?x ex:r ex:z } }",
            Some("SELECT ?x { ?x ex:p ex:o VALUES ?x { ex:Alias ex:Rep } }"), PLAIN, false),
        ("SERVICE, own filter", "SELECT ?x { SERVICE <http://remote.example/sparql> { ?x ex:r ex:z FILTER(?x = ex:Alias) } ?x ex:p ex:o }",
            Some("SELECT ?x { VALUES ?x { ex:Alias } ?x ex:p ex:o }"), PLAIN, false),
        ("GRAPH, inner =", "SELECT ?x { GRAPH ex:g { ?x ex:in ex:z FILTER(?x = ex:Alias) } }", None, NAMED, false),
        ("GRAPH, outer =", "SELECT ?x { GRAPH ex:g { ?x ex:in ex:z } FILTER(?x = ex:Alias) }", None, NAMED, false),
        ("GRAPH, outer !=", "SELECT ?x { GRAPH ex:g { ?x ex:in ex:z } FILTER(?x != ex:Alias) }", None, NAMED, false),
        ("GRAPH, outer STR", "SELECT ?x { GRAPH ex:g { ?x ex:in ex:z } FILTER(STR(?x) = \"http://example.com/Rep\") }", None, NAMED, false),
        ("GRAPH, outer sameTerm", "SELECT ?x { GRAPH ex:g { ?x ex:in ex:z } FILTER(sameTerm(?x, ex:Alias)) }", None, NAMED, false),
        ("GRAPH, outer IRI()", "SELECT ?x { GRAPH ex:g { ?x ex:in ex:z } FILTER(?x = IRI(\"http://example.com/Alias\")) }", None, NAMED, false),
        ("GRAPH, inner constant", "SELECT ?g { GRAPH ?g { ex:Rep ex:in ex:z } }", None, NAMED, false),
        ("GRAPH <g>, inline probe", "SELECT ?z { GRAPH ex:g { ex:Alias ex:in ?z } }", None, NAMED, false),
        ("GRAPH <g>, VALUES", "SELECT ?x ?z { VALUES ?x { ex:Alias } GRAPH ex:g { ?x ex:in ?z } }", None, NAMED, false),
        ("GRAPH <g>, FILTER scan", "SELECT ?x ?z { GRAPH ex:g { ?x ex:in ?z FILTER(?x = ex:Alias) } }", None, NAMED, false),
        ("GRAPH ?g, inline probe", "SELECT ?g ?z { GRAPH ?g { ex:Alias ex:in ?z } }", None, NAMED, false),
        ("GRAPH ?g, VALUES", "SELECT ?g ?x { VALUES ?x { ex:Alias } GRAPH ?g { ?x ex:in ?z } }", None, NAMED, false),
        ("GRAPH ?g, FILTER scan", "SELECT ?g ?x { GRAPH ?g { ?x ex:in ?z FILTER(?x = ex:Alias) } }", None, NAMED, false),
        ("FROM NAMED, inline probe", "SELECT ?g ?z FROM ex:none FROM NAMED ex:g { GRAPH ?g { ex:Alias ex:in ?z } }", None, NAMED, false),
        ("FROM NAMED, VALUES", "SELECT ?g ?x FROM ex:none FROM NAMED ex:g { VALUES ?x { ex:Alias } GRAPH ?g { ?x ex:in ?z } }", None, NAMED, false),
        ("FROM NAMED, FILTER scan", "SELECT ?g ?x FROM ex:none FROM NAMED ex:g { GRAPH ?g { ?x ex:in ?z FILTER(?x = ex:Alias) } }", None, NAMED, false),
        ("GRAPH named in a class", "SELECT ?x { GRAPH ex:Alias { ?x ex:in ex:z } }", None, NAMED, false),
        ("GRAPH, then default", "SELECT ?x { GRAPH ex:g { ?x ex:in ex:z } ?x ex:p ex:o }", None, NAMED, false),
        ("default, then GRAPH", "SELECT ?x { ?x ex:p ex:o GRAPH ex:g { ?x ex:in ex:z } }", None, NAMED, false),
        ("GRAPH in EXISTS", "SELECT ?x { ?x ex:p ex:o FILTER EXISTS { GRAPH ex:g { ?x ex:in ex:z } } }", None, NAMED, false),
        ("GRAPH in MINUS", "SELECT ?x { ?x ex:p ex:o MINUS { GRAPH ex:g { ?x ex:in ex:z } } }", None, NAMED, false),
        ("GRAPH ?g, VALUES", "SELECT ?g { VALUES ?g { ex:Alias } GRAPH ?g { ?x ex:in ex:z } }", None, NAMED, false),
        ("FROM a named graph", "SELECT ?x FROM ex:g { ?x ex:in ex:z }", None, NAMED, false),
        ("union default graph", "SELECT ?x { ?x ex:in ?z . ?x ex:p ex:o }", None, NAMED, false),
        ("triple term, value", "SELECT ?t { ex:meta ex:quoted ?t }", None, QUOTED, false),
        ("SUBJECT, join", "SELECT ?x { ex:meta ex:quoted ?t BIND(SUBJECT(?t) AS ?x) ?x ex:p ex:o }", None, QUOTED, false),
        ("triple pattern, open", "SELECT ?s { ex:meta ex:quoted <<( ?s ex:p ex:o )>> }", None, QUOTED, false),
        ("quoted constant", "SELECT * { ex:meta ex:quoted <<( ex:Alias ex:p ex:o )>> }", None, QUOTED, false),
        ("quoted VALUES", "SELECT ?t { VALUES ?t { <<( ex:Alias ex:p ex:o )>> } ex:meta ex:quoted ?t }", None, QUOTED, false),
        ("= TRIPLE()", "SELECT ?t { ex:meta ex:quoted ?t FILTER(?t = TRIPLE(ex:Alias, ex:p, ex:o)) }", None, QUOTED, false),
        ("CONSTRUCT, quoted variable", "CONSTRUCT { ex:m ex:q <<( ?x ex:p ex:o )>> } WHERE { VALUES ?x { ex:Alias } }", None, QUOTED, false),
        ("CONSTRUCT, quoted constant", "CONSTRUCT { ex:m ex:q <<( ex:Alias ex:p ex:o )>> } WHERE { }", None, QUOTED, false),
        ("a stored triple term names an alias", "SELECT ?x { ?x ex:p ex:o }", None, ALIASED, true),
    ];
    let remote = Engine::new(EngineConfig::default()).unwrap();
    let mut tx = remote.transaction();
    for s in ["Alias", "Rep"] {
        tx.insert(Quad::new(ex(s), ex("r"), ex("z"), GraphName::DefaultGraph).as_ref());
    }
    tx.commit().unwrap();
    let services = Services(Arc::new(Endpoint(remote)));
    let engines = [PLAIN, NAMED, QUOTED, ALIASED].map(engine);
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
    for &(case, query, reference_query, fixture, refused) in cases {
        let snapshot = engines[fixture].snapshot();
        let ordered = query.contains("ORDER BY");
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
                let got = answer(evaluate_query(&snapshot, &parse(query), &options), ordered);
                let expected = match canonical && refused {
                    true => Err("refused".to_owned()),
                    false => answer(
                        nrese_sparql_reference::evaluate_query(
                            &snapshot,
                            &parse(reference_query.unwrap_or(query)),
                            &options,
                        ),
                        ordered,
                    ),
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

/// The engine's side of the same contract, for its other readers (spatial and text
/// indexes): a canonical snapshot reads every graph over representatives, so a pattern's
/// constant, the graph's name included, names its class in every read, whatever the
/// graph selector or the access path.
#[test]
fn canonical_reads_take_a_constant_by_its_class_in_every_graph() {
    use nrese_engine::{GraphSelector, QuadPattern, ReadModel, TermId};
    let engine = engine(NAMED);
    let snapshot = engine.snapshot().with_canonical_equality();
    let id = |local: &str| snapshot.lookup(ex(local).as_ref().into()).unwrap();
    let (alias, rep, input, z) = (id("Alias"), id("Rep"), id("in"), id("z"));
    let model = ReadModel::Materialised;
    for graph in [
        GraphSelector::Exact(id("g")),
        GraphSelector::Exact(alias),
        GraphSelector::AnyNamed,
        GraphSelector::Any,
    ] {
        let pattern = |subject: TermId| QuadPattern {
            subject: Some(subject),
            predicate: Some(input),
            object: None,
            graph,
        };
        let reads = |subject: TermId| {
            let pattern = pattern(subject);
            let mut cursor = snapshot.probe_cursor(model);
            let mut probed = Vec::new();
            cursor.for_each(&pattern, |quad| probed.push(quad));
            let mut scanned: Vec<_> = snapshot.quads_for_pattern_in(model, &pattern).collect();
            scanned.sort_unstable();
            probed.sort_unstable();
            (
                scanned,
                probed,
                snapshot.count_in(model, &pattern),
                snapshot.exists_in(model, &pattern),
                cursor.count(&pattern),
                cursor.exists(&pattern),
            )
        };
        let (by_alias, by_rep) = (reads(alias), reads(rep));
        assert!(!by_rep.0.is_empty(), "{graph:?}: the class's statements");
        assert!(
            by_rep.0.iter().all(|q| q.subject == rep),
            "{graph:?}: {:?}",
            by_rep.0
        );
        assert_eq!(by_alias, by_rep, "{graph:?}");
    }
    let quad =
        |subject: TermId, graph: TermId| nrese_engine::EncodedQuad::new(subject, input, z, graph);
    assert!(snapshot.contains_in(model, &quad(alias, id("g"))));
    assert!(snapshot.contains_in(model, &quad(alias, alias)));
    // The asserted model, and the snapshot read as stored, read the statements as stored.
    let stored = QuadPattern {
        graph: GraphSelector::AnyNamed,
        ..QuadPattern::all()
    };
    for (read, model) in [
        (&snapshot, ReadModel::Asserted),
        (&snapshot.stored(), ReadModel::Asserted),
        (&snapshot.stored(), ReadModel::Materialised),
    ] {
        let subjects: Vec<_> = read
            .quads_for_pattern_in(model, &stored)
            .map(|q| (q.subject, q.graph))
            .collect();
        assert_eq!(subjects.len(), 2, "{model:?}");
        assert!(
            subjects.iter().all(|&(s, _)| s == alias),
            "{model:?}: {subjects:?}"
        );
        assert!(
            subjects.iter().any(|&(_, g)| g == alias),
            "{model:?}: {subjects:?}"
        );
    }
}
