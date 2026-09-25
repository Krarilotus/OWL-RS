//! Q1 evidence: queries and updates give the same results on nrese-engine as on Oxigraph's
//! own storage, which runs the same `spareval` evaluator. A difference therefore points at
//! the adapter (term identity, graph handling, update semantics), not the evaluator.
//!
//! Blank-node labels are normalised, since fresh labels legitimately differ between stores.
//!
//! The shared dataset uses canonical lexical forms only: Oxigraph canonicalises typed
//! literals on storage (`"030"^^xsd:integer` becomes `"30"`), which violates RDF term
//! identity. NRESE preserves lexical forms (ADR-0002); `non_canonical_literals_keep_their_identity`
//! pins that behaviour separately.

use nrese_engine::{Engine, EngineConfig};
use nrese_sparql::{QueryOptions, QueryResults, UpdateOptions, apply_update, evaluate_query};
use oxigraph::sparql::{QueryResults as OxResults, SparqlEvaluator};
use oxigraph::store::Store;
use oxrdf::vocab::xsd;
use oxrdf::{BlankNode, GraphName, Literal, NamedNode, Quad, Term};
use spargebra::SparqlParser;

const EX: &str = "http://example.com/";

fn ex(local: &str) -> NamedNode {
    NamedNode::new_unchecked(format!("{EX}{local}"))
}

fn dataset() -> Vec<Quad> {
    let g1: GraphName = ex("g1").into();
    let g2: GraphName = ex("g2").into();
    let dg = GraphName::DefaultGraph;
    let int = |v: &str| Literal::new_typed_literal(v, xsd::INTEGER);
    let typed = |v: &str, datatype| Term::from(Literal::new_typed_literal(v, datatype));
    let q = |s: NamedNode, p: &str, o: Term, g: &GraphName| Quad::new(s, ex(p), o, g.clone());
    let mut quads = vec![
        q(ex("alice"), "knows", ex("bob").into(), &dg),
        q(ex("bob"), "knows", ex("carol").into(), &dg),
        q(ex("carol"), "knows", ex("dave").into(), &dg),
        q(ex("alice"), "age", int("30").into(), &dg),
        q(ex("bob"), "age", int("31").into(), &dg),
        q(ex("carol"), "age", int("25").into(), &dg),
        q(
            ex("dave"),
            "age",
            Literal::new_typed_literal("25.5", xsd::DECIMAL).into(),
            &dg,
        ),
        q(
            ex("alice"),
            "name",
            Literal::new_language_tagged_literal_unchecked("Alice", "en").into(),
            &dg,
        ),
        q(
            ex("alice"),
            "name",
            Literal::new_language_tagged_literal_unchecked("Alicia", "es").into(),
            &dg,
        ),
        q(
            ex("bob"),
            "name",
            Literal::new_simple_literal("Bob").into(),
            &dg,
        ),
        q(
            ex("alice"),
            "active",
            Literal::new_typed_literal("true", xsd::BOOLEAN).into(),
            &dg,
        ),
        q(
            ex("bob"),
            "active",
            Literal::new_typed_literal("false", xsd::BOOLEAN).into(),
            &dg,
        ),
        q(ex("alice"), "memberOf", ex("org1").into(), &g1),
        q(ex("bob"), "memberOf", ex("org1").into(), &g1),
        q(ex("carol"), "memberOf", ex("org2").into(), &g2),
        q(
            ex("org1"),
            "label",
            Literal::new_simple_literal("Org One").into(),
            &g2,
        ),
        q(ex("alice"), "knows", ex("bob").into(), &g2), // same triple in two graphs
        // Inline value kinds (E1): canonical dates, dateTimes and decimals.
        q(ex("alice"), "born", typed("1990-05-01", xsd::DATE), &dg),
        q(ex("bob"), "born", typed("1991-12-31Z", xsd::DATE), &dg),
        q(
            ex("carol"),
            "born",
            typed("1985-02-28+05:30", xsd::DATE),
            &dg,
        ),
        q(
            ex("alice"),
            "seen",
            typed("2026-09-25T14:03:07.5+02:00", xsd::DATE_TIME),
            &dg,
        ),
        q(
            ex("bob"),
            "seen",
            typed("2026-09-25T09:00:00Z", xsd::DATE_TIME),
            &dg,
        ),
        q(
            ex("carol"),
            "seen",
            typed("2026-09-24T23:59:59", xsd::DATE_TIME),
            &dg,
        ),
        q(ex("alice"), "score", typed("-0.125", xsd::DECIMAL), &dg),
        q(ex("bob"), "score", typed("7.25", xsd::DECIMAL), &dg), // Oxigraph rewrites "7.0" to "7"
    ];
    quads.push(Quad::new(
        ex("alice"),
        ex("address"),
        BlankNode::new_unchecked("addr1"),
        dg.clone(),
    ));
    quads.push(Quad::new(
        BlankNode::new_unchecked("addr1"),
        ex("city"),
        Literal::new_simple_literal("Berlin"),
        dg,
    ));
    quads
}

const QUERIES: &[&str] = &[
    "PREFIX ex: <http://example.com/> PREFIX xsd: <http://www.w3.org/2001/XMLSchema#> SELECT ?s WHERE { ?s ex:born \"1990-05-01\"^^xsd:date }",
    "PREFIX ex: <http://example.com/> PREFIX xsd: <http://www.w3.org/2001/XMLSchema#> SELECT ?s ?d WHERE { ?s ex:born ?d FILTER(?d < \"1991-01-01\"^^xsd:date) }",
    "PREFIX ex: <http://example.com/> SELECT ?s ?t WHERE { ?s ex:seen ?t } ORDER BY ?t",
    "PREFIX ex: <http://example.com/> PREFIX xsd: <http://www.w3.org/2001/XMLSchema#> SELECT ?s WHERE { ?s ex:seen ?t FILTER(?t >= \"2026-09-25T00:00:00Z\"^^xsd:dateTime) }",
    "PREFIX ex: <http://example.com/> SELECT ?s (YEAR(?d) AS ?y) (MONTH(?d) AS ?m) WHERE { ?s ex:born ?d }",
    "PREFIX ex: <http://example.com/> SELECT ?s (?v * 2 AS ?double) WHERE { ?s ex:score ?v } ORDER BY ?v",
    "PREFIX ex: <http://example.com/> SELECT ?s WHERE { ?s ex:score 7.25 }",
    "SELECT * WHERE { ?s ?p ?o }",
    "SELECT * WHERE { GRAPH ?g { ?s ?p ?o } }",
    "SELECT ?g WHERE { GRAPH ?g { } }",
    "SELECT DISTINCT ?g WHERE { GRAPH ?g { ?s ?p ?o } }",
    "PREFIX ex: <http://example.com/> SELECT ?a ?c WHERE { ?a ex:knows ?b . ?b ex:knows ?c }",
    "PREFIX ex: <http://example.com/> SELECT ?x WHERE { ex:alice ex:knows+ ?x }",
    "PREFIX ex: <http://example.com/> SELECT ?x WHERE { ?x ex:knows* ex:dave }",
    "PREFIX ex: <http://example.com/> SELECT ?s ?age WHERE { ?s ex:age ?age FILTER(?age = 30) }",
    "PREFIX ex: <http://example.com/> SELECT ?s WHERE { ?s ex:age 30 }",
    "PREFIX ex: <http://example.com/> SELECT ?s WHERE { ?s ex:age ?a FILTER(?a > 25) }",
    "PREFIX ex: <http://example.com/> SELECT ?s WHERE { BIND(25 AS ?v) ?s ex:age ?v }",
    "PREFIX ex: <http://example.com/> SELECT ?s WHERE { VALUES ?v { 25 30 25.5 } ?s ex:age ?v }",
    "PREFIX ex: <http://example.com/> SELECT ?s WHERE { ?s ex:active true }",
    "PREFIX ex: <http://example.com/> SELECT ?s ?n WHERE { ?s ex:knows ?o OPTIONAL { ?s ex:name ?n FILTER(lang(?n) = 'en') } }",
    "PREFIX ex: <http://example.com/> SELECT ?s WHERE { ?s ex:age ?a MINUS { ?s ex:memberOf ?o } }",
    "PREFIX ex: <http://example.com/> SELECT ?s WHERE { ?s ex:age ?a FILTER NOT EXISTS { GRAPH ?g { ?s ex:memberOf ?o } } }",
    "PREFIX ex: <http://example.com/> SELECT ?o (COUNT(?s) AS ?n) WHERE { GRAPH ?g { ?s ex:memberOf ?o } } GROUP BY ?o",
    "PREFIX ex: <http://example.com/> SELECT (SUM(?a) AS ?total) (AVG(?a) AS ?avg) WHERE { ?s ex:age ?a }",
    "PREFIX ex: <http://example.com/> SELECT ?s ?a WHERE { ?s ex:age ?a } ORDER BY DESC(?a) ?s LIMIT 3",
    "PREFIX ex: <http://example.com/> SELECT * FROM ex:g1 WHERE { ?s ?p ?o }",
    "PREFIX ex: <http://example.com/> SELECT * FROM NAMED ex:g2 WHERE { GRAPH ?g { ?s ?p ?o } }",
    "PREFIX ex: <http://example.com/> SELECT * WHERE { GRAPH ex:missing { ?s ?p ?o } }",
    "PREFIX ex: <http://example.com/> SELECT ?city WHERE { ex:alice ex:address [ ex:city ?city ] }",
    "PREFIX ex: <http://example.com/> SELECT ?s WHERE { ?s ex:name ?n FILTER(STR(?n) = 'Bob') }",
    "PREFIX ex: <http://example.com/> SELECT ?s WHERE { ?s ex:unknownPredicate ?o }",
    "PREFIX ex: <http://example.com/> SELECT ?x WHERE { { SELECT ?x WHERE { ?x ex:knows ?y } ORDER BY ?x ?y LIMIT 2 } ?x ex:age ?a }",
    "PREFIX ex: <http://example.com/> ASK { ex:alice ex:knows ex:bob }",
    "PREFIX ex: <http://example.com/> ASK { GRAPH ex:g1 { ex:carol ?p ?o } }",
    "PREFIX ex: <http://example.com/> CONSTRUCT { ?b ex:knownBy ?a } WHERE { ?a ex:knows ?b }",
    "PREFIX ex: <http://example.com/> DESCRIBE ex:alice",
];

/// Update scripts, applied in sequence; contents are compared after each.
const UPDATES: &[&str] = &[
    "PREFIX ex: <http://example.com/> INSERT DATA { ex:erin ex:knows ex:alice . GRAPH ex:g3 { ex:erin ex:age 41 } }",
    "PREFIX ex: <http://example.com/> DELETE DATA { ex:alice ex:knows ex:bob }",
    "PREFIX ex: <http://example.com/> DELETE { ?s ex:age ?a } INSERT { ?s ex:age ?b } WHERE { ?s ex:age ?a FILTER(isNumeric(?a)) BIND(?a + 1 AS ?b) }",
    "PREFIX ex: <http://example.com/> INSERT { GRAPH ex:g4 { ?s ex:copied true } } WHERE { GRAPH ex:g1 { ?s ?p ?o } }",
    "PREFIX ex: <http://example.com/> INSERT DATA { ex:x ex:p ex:y } ; DELETE WHERE { ex:x ex:p ?o } ; INSERT DATA { ex:z ex:p ex:y }",
    "PREFIX ex: <http://example.com/> INSERT { ?s ex:tag _:t . _:t ex:value ?s } WHERE { ?s ex:knows ex:carol }",
    "PREFIX ex: <http://example.com/> DELETE { GRAPH ?g { ?s ?p ?o } } USING NAMED ex:g2 WHERE { GRAPH ?g { ?s ?p ?o } }",
    "PREFIX ex: <http://example.com/> WITH ex:g1 DELETE { ?s ex:memberOf ?o } WHERE { ?s ex:memberOf ?o }",
    "PREFIX ex: <http://example.com/> CLEAR GRAPH ex:g3",
    "CLEAR SILENT GRAPH <http://example.com/nothing>",
    "PREFIX ex: <http://example.com/> DROP GRAPH ex:g4",
    "CLEAR NAMED",
    "INSERT DATA { GRAPH <http://example.com/g5> { <http://example.com/a> <http://example.com/b> _:x } }",
    "DROP ALL",
];

fn term_key(term: &Term) -> String {
    match term {
        Term::BlankNode(_) => "_:b".to_owned(),
        other => other.to_string(),
    }
}

#[derive(Debug, PartialEq)]
enum Normalized {
    Solutions(Vec<String>),
    Boolean(bool),
    Graph(Vec<String>),
}

fn normalize(results: QueryResults<'_>, ordered: bool) -> Normalized {
    match results {
        QueryResults::Solutions(solutions) => {
            let mut rows: Vec<String> = solutions
                .map(|solution| {
                    let solution = solution.expect("solution");
                    let mut row: Vec<String> = solution
                        .iter()
                        .map(|(variable, term)| format!("{variable}={}", term_key(term)))
                        .collect();
                    row.sort();
                    row.join(" ")
                })
                .collect();
            if !ordered {
                rows.sort();
            }
            Normalized::Solutions(rows)
        }
        QueryResults::Boolean(value) => Normalized::Boolean(value),
        QueryResults::Graph(triples) => {
            let mut rows: Vec<String> = triples
                .map(|triple| {
                    let triple = triple.expect("triple");
                    format!(
                        "{} {} {}",
                        term_key(&triple.subject.into()),
                        triple.predicate,
                        term_key(&triple.object)
                    )
                })
                .collect();
            rows.sort();
            Normalized::Graph(rows)
        }
    }
}

fn load_both() -> (Engine, Store) {
    let engine = Engine::new(EngineConfig {
        background_maintenance: false,
        ..EngineConfig::default()
    })
    .unwrap();
    let store = Store::new().unwrap();
    let mut tx = engine.transaction();
    for quad in dataset() {
        tx.insert(quad.as_ref());
        store.insert(&quad).unwrap();
    }
    tx.commit().unwrap();
    (engine, store)
}

fn ours(engine: &Engine, query: &str) -> Normalized {
    let parsed = SparqlParser::new().parse_query(query).unwrap();
    let snapshot = engine.snapshot();
    let results = evaluate_query(&snapshot, &parsed, &QueryOptions::default()).unwrap();
    normalize(results, query.contains("ORDER BY"))
}

fn oracle(store: &Store, query: &str) -> Normalized {
    let results: OxResults<'_> = SparqlEvaluator::new()
        .parse_query(query)
        .unwrap()
        .on_store(store)
        .execute()
        .unwrap();
    normalize(results, query.contains("ORDER BY"))
}

const ALL_QUADS: &str = "SELECT * WHERE { { ?s ?p ?o } UNION { GRAPH ?g { ?s ?p ?o } } }";

#[test]
fn queries_match_the_oracle() {
    let (engine, store) = load_both();
    for query in QUERIES {
        assert_eq!(
            ours(&engine, query),
            oracle(&store, query),
            "query: {query}"
        );
    }
}

#[test]
fn update_sequences_match_the_oracle() {
    let (engine, store) = load_both();
    for update in UPDATES {
        let parsed = SparqlParser::new().parse_update(update).unwrap();
        let mut tx = engine.transaction();
        apply_update(&mut tx, &parsed, &UpdateOptions::default()).unwrap();
        tx.commit().unwrap();
        SparqlEvaluator::new()
            .parse_update(update)
            .unwrap()
            .on_store(&store)
            .execute()
            .unwrap();
        assert_eq!(
            ours(&engine, ALL_QUADS),
            oracle(&store, ALL_QUADS),
            "after: {update}"
        );
        let mismatches: Vec<_> = QUERIES
            .iter()
            .filter(|query| **query != EMPTY_GRAPH_PATTERN)
            .filter(|query| ours(&engine, query) != oracle(&store, query))
            .collect();
        assert!(mismatches.is_empty(), "after {update}: {mismatches:#?}");
    }
}

/// Lists graphs by existence. Oxigraph keeps a graph registered after its last quad is
/// deleted; NRESE, like QLever and RDF4J/GraphDB, treats a graph as existing iff it holds a
/// quad (ADR-0002). Compared before updates only, and pinned by the test below.
const EMPTY_GRAPH_PATTERN: &str = "SELECT ?g WHERE { GRAPH ?g { } }";

#[test]
fn graphs_exist_while_they_hold_quads() {
    let (engine, _) = load_both();
    let graphs = |engine: &Engine| match ours(engine, EMPTY_GRAPH_PATTERN) {
        Normalized::Solutions(rows) => rows,
        other => panic!("{other:?}"),
    };
    assert_eq!(graphs(&engine).len(), 2);
    for update in [
        "DELETE WHERE { GRAPH <http://example.com/g2> { ?s ?p ?o } }",
        "CREATE GRAPH <http://example.com/g9>",
    ] {
        let parsed = SparqlParser::new().parse_update(update).unwrap();
        let mut tx = engine.transaction();
        apply_update(&mut tx, &parsed, &UpdateOptions::default()).unwrap();
        tx.commit().unwrap();
    }
    assert_eq!(graphs(&engine), vec!["?g=<http://example.com/g1>"]);
}

#[test]
fn a_failed_update_leaves_nothing_to_commit() {
    let (engine, _) = load_both();
    let before = ours(&engine, ALL_QUADS);
    let update = SparqlParser::new()
        .parse_update("INSERT DATA { <http://example.com/new> <http://example.com/p> 1 } ; DROP GRAPH <http://example.com/missing>")
        .unwrap();
    let mut tx = engine.transaction();
    let error = apply_update(&mut tx, &update, &UpdateOptions::default()).unwrap_err();
    assert!(error.to_string().contains("does not exist"), "{error}");
    drop(tx); // the caller aborts on error
    assert_eq!(ours(&engine, ALL_QUADS), before);
}

#[test]
fn cancelled_updates_stop_before_the_next_operation() {
    let (engine, _) = load_both();
    let token = nrese_sparql::CancellationToken::new();
    token.cancel();
    let options = UpdateOptions {
        cancellation: Some(token),
        ..UpdateOptions::default()
    };
    let update = SparqlParser::new()
        .parse_update("INSERT DATA { <http://example.com/new> <http://example.com/p> 1 }")
        .unwrap();
    let mut tx = engine.transaction();
    assert!(matches!(
        apply_update(&mut tx, &update, &options),
        Err(nrese_sparql::UpdateError::Cancelled)
    ));
}

#[test]
fn protocol_dataset_overrides_from_clauses() {
    let (engine, _) = load_both();
    let query = SparqlParser::new()
        .parse_query("SELECT * FROM <http://example.com/g1> WHERE { ?s ?p ?o }")
        .unwrap();
    let mut dataset = nrese_sparql::QueryDatasetSpecification::new();
    dataset.set_default_graph(vec![ex("g2").into()]);
    let options = QueryOptions {
        dataset: Some(dataset),
        ..QueryOptions::default()
    };
    let snapshot = engine.snapshot();
    let Normalized::Solutions(rows) =
        normalize(evaluate_query(&snapshot, &query, &options).unwrap(), false)
    else {
        panic!("solutions")
    };
    assert_eq!(rows.len(), 3, "g2 has three quads: {rows:?}");
}

#[test]
fn non_canonical_dates_and_decimals_keep_their_identity() {
    let engine = Engine::new(EngineConfig::default()).unwrap();
    let mut tx = engine.transaction();
    for (subject, lexical, datatype) in [
        ("a", "2026-09-25Z", xsd::DATE),      // canonical: inline
        ("b", "2026-09-25+00:00", xsd::DATE), // same value, dictionary term
        ("c", "1.5", xsd::DECIMAL),           // canonical: inline
        ("d", "1.50", xsd::DECIMAL),          // same value, dictionary term
    ] {
        let literal = Literal::new_typed_literal(lexical, datatype);
        tx.insert(Quad::new(ex(subject), ex("v"), literal, GraphName::DefaultGraph).as_ref());
    }
    tx.commit().unwrap();
    let solutions = |query: &str| match ours(&engine, query) {
        Normalized::Solutions(rows) => rows,
        other => panic!("{other:?}"),
    };
    assert_eq!(
        solutions("SELECT ?o WHERE { ?s ?p ?o }"),
        vec![
            "?o=\"1.5\"^^<http://www.w3.org/2001/XMLSchema#decimal>",
            "?o=\"1.50\"^^<http://www.w3.org/2001/XMLSchema#decimal>",
            "?o=\"2026-09-25+00:00\"^^<http://www.w3.org/2001/XMLSchema#date>",
            "?o=\"2026-09-25Z\"^^<http://www.w3.org/2001/XMLSchema#date>",
        ]
    );
    let date = "\"2026-09-25Z\"^^<http://www.w3.org/2001/XMLSchema#date>";
    assert_eq!(
        solutions(&format!(
            "SELECT ?s WHERE {{ ?s <http://example.com/v> {date} }}"
        )),
        vec!["?s=<http://example.com/a>"]
    );
    assert_eq!(
        solutions(&format!(
            "SELECT ?s WHERE {{ ?s <http://example.com/v> ?o FILTER(?o = {date}) }}"
        ))
        .len(),
        2
    );
    assert_eq!(
        solutions("SELECT ?s WHERE { ?s <http://example.com/v> ?o FILTER(?o = 1.5) }").len(),
        2
    );
}

#[test]
fn non_canonical_literals_keep_their_identity() {
    let engine = Engine::new(EngineConfig::default()).unwrap();
    let mut tx = engine.transaction();
    for (subject, lexical) in [("a", "30"), ("b", "030"), ("c", "+30")] {
        let literal = Literal::new_typed_literal(lexical, xsd::INTEGER);
        tx.insert(Quad::new(ex(subject), ex("age"), literal, GraphName::DefaultGraph).as_ref());
    }
    tx.commit().unwrap();
    let solutions = |query: &str| match ours(&engine, query) {
        Normalized::Solutions(rows) => rows,
        other => panic!("{other:?}"),
    };
    // Stored lexical forms come back unchanged.
    assert_eq!(
        solutions("SELECT ?o WHERE { ?s ?p ?o }"),
        vec![
            "?o=\"+30\"^^<http://www.w3.org/2001/XMLSchema#integer>",
            "?o=\"030\"^^<http://www.w3.org/2001/XMLSchema#integer>",
            "?o=\"30\"^^<http://www.w3.org/2001/XMLSchema#integer>",
        ]
    );
    // Graph pattern matching is by RDF term (sameTerm) ...
    assert_eq!(
        solutions("SELECT ?s WHERE { ?s <http://example.com/age> 30 }"),
        vec!["?s=<http://example.com/a>"]
    );
    // ... while `=` compares values, so all three match.
    assert_eq!(
        solutions("SELECT ?s WHERE { ?s <http://example.com/age> ?a FILTER(?a = 30) }").len(),
        3
    );
    // A computed value joins with the stored canonical term only.
    assert_eq!(
        solutions("SELECT ?s WHERE { BIND(30 AS ?v) ?s <http://example.com/age> ?v }"),
        vec!["?s=<http://example.com/a>"]
    );
}
