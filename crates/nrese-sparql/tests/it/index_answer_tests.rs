//! A query whose whole answer is the index's count of one triple pattern, `SELECT
//! (COUNT(*) AS ?n) WHERE { s p o }`, is answered from the index alone: no preparation, no
//! operators (EXPLAIN: one `index answer` step). Its answer is the general path's in every
//! delivery: ordinary and typed results, the direct JSON, TSV and CSV writer, EXPLAIN's row
//! count. A query that differs in any part, or whose count isn't the index's (the merged
//! default graph, a repeated variable), takes the general path with its own answer.
//!
//! The oracle is the general path itself (`as_written`, which the index answer never takes);
//! each case also states its answer, worked out by hand from [`engine`]'s statements.

use std::sync::Arc;

use nrese_engine::{EncodedTriple, Engine, EngineConfig, ReadModel};
use nrese_rdf::{GraphName, NamedNode, NamedOrBlankNode, Quad, Variable};
use nrese_sparql::ql::{Closure, QlRewriting};
use nrese_sparql::{
    QueryDatasetSpecification, QueryOptions, QueryResults, ReadView, ResultsFormat, TypedResults,
    UpdateOptions, apply_update, evaluate_query, evaluate_query_typed, explain_query,
    write_results,
};
use nrese_sparql_results::QueryResultsFormat;
use nrese_sparql_syntax::SparqlParser;

use crate::pre_bound_delivery_tests::{lines, serialised};

const EX: &str = "http://example.com/";
const INTEGER: &str = "<http://www.w3.org/2001/XMLSchema#integer>";

fn ex(local: &str) -> NamedNode {
    NamedNode::new_unchecked(format!("{EX}{local}"))
}

fn person(subject: &str, graph: GraphName) -> Quad {
    Quad::new(
        ex(subject),
        nrese_rdf::vocab::rdf::TYPE.into_owned(),
        ex("Person"),
        graph,
    )
}

fn graph(local: &str) -> GraphName {
    GraphName::NamedNode(ex(local))
}

/// In the default graph: `s1`, `s2`, `s3` and `s6` are asserted `e:Person`s, `s4` an
/// inferred one; `s5` was one and was removed, `s6` was removed and put back. `s1 e:knows`
/// `s1` and `s2`; `e:doc e:says <<( e:s1 e:knows e:s2 )>>`. In `g1`: `s1` (as in the default
/// graph) and `s7`; in `g2`: `s7`.
fn engine() -> Engine {
    let engine = Engine::new(EngineConfig::default()).unwrap();
    let mut tx = engine.transaction();
    for s in ["s1", "s2", "s3", "s5", "s6"] {
        tx.insert(person(s, GraphName::DefaultGraph).as_ref());
    }
    for (s, o) in [("s1", "s1"), ("s1", "s2")] {
        tx.insert(Quad::new(ex(s), ex("knows"), ex(o), GraphName::DefaultGraph).as_ref());
    }
    for (s, g) in [("s1", "g1"), ("s7", "g1"), ("s7", "g2")] {
        tx.insert(person(s, graph(g)).as_ref());
    }
    let inferred = EncodedTriple {
        subject: tx.intern(ex("s4").as_ref().into()),
        predicate: tx.intern(nrese_rdf::vocab::rdf::TYPE.into()),
        object: tx.intern(ex("Person").as_ref().into()),
    };
    tx.insert_inferred(inferred);
    tx.commit().unwrap();
    let mut tx = engine.transaction();
    for s in ["s5", "s6"] {
        tx.remove(person(s, GraphName::DefaultGraph).as_ref());
    }
    tx.commit().unwrap();
    let mut tx = engine.transaction();
    tx.insert(person("s6", GraphName::DefaultGraph).as_ref());
    let update = SparqlParser::new()
        .parse_update(&format!(
            "PREFIX e: <{EX}> INSERT DATA {{ e:doc e:says <<( e:s1 e:knows e:s2 )>> }}"
        ))
        .unwrap();
    apply_update(&mut tx, &update, &UpdateOptions::default()).unwrap();
    tx.commit().unwrap();
    engine
}

/// `s1`, `s9` and `s2` are `e:Person`s, `s9 owl:sameAs s1` inferred: under equality, three
/// people with late expansion (each member's statements are the class's), two classes with
/// canonical answers.
fn equality_engine() -> Engine {
    let engine = Engine::new(EngineConfig::default()).unwrap();
    let mut tx = engine.transaction();
    for s in ["s1", "s9", "s2"] {
        tx.insert(person(s, GraphName::DefaultGraph).as_ref());
    }
    let same_as = NamedNode::new_unchecked("http://www.w3.org/2002/07/owl#sameAs");
    let same_as = tx.intern(same_as.as_ref().into());
    let inferred = EncodedTriple {
        subject: tx.intern(ex("s9").as_ref().into()),
        predicate: same_as,
        object: tx.intern(ex("s1").as_ref().into()),
    };
    tx.insert_inferred(inferred);
    tx.commit().unwrap();
    engine.set_equality(Some(same_as));
    engine
}

fn count(n: u64) -> String {
    format!("\"{n}\"^^{INTEGER}")
}

/// What differs, delivery by delivery, between `text`'s answers on `view` under `options`
/// and `expected` (the projected variables, then the rows as [`lines`] gives them), and
/// whether it ran as an index answer as `indexed` says. `direct`: the view is written
/// directly (a snapshot; a transaction isn't).
fn wrong(
    view: &impl ReadView,
    text: &str,
    options: &QueryOptions,
    expected: &[String],
    indexed: bool,
    direct: bool,
) -> Vec<String> {
    let text = format!("PREFIX e: <{EX}> {text}");
    let query = SparqlParser::new()
        .parse_query(&text)
        .unwrap_or_else(|e| panic!("{e}: {text}"));
    let general = QueryOptions {
        as_written: true,
        ..options.clone()
    };
    let mut wrong = Vec::new();
    let answer = |options: &QueryOptions| {
        let QueryResults::Solutions(solutions) = evaluate_query(view, &query, options).unwrap()
        else {
            panic!("a SELECT: {text}")
        };
        let variables = solutions.variables().to_vec();
        let names = variables.iter().map(Variable::as_str).collect::<Vec<_>>();
        let mut rows = vec![names.join(" ")];
        rows.extend(lines(solutions.map(|s| {
            let s = s.unwrap();
            variables.iter().map(|v| s.get(v).cloned()).collect()
        })));
        rows
    };
    for (how, options) in [("general", &general), ("default", options)] {
        let rows = answer(options);
        if rows != expected {
            wrong.push(format!("{how}: {rows:?}"));
        }
    }
    match evaluate_query_typed(view, &query, options).unwrap() {
        TypedResults::Solutions(table) => {
            let typed = lines((0..table.len()).map(|r| table.row(r)));
            if typed != expected[1..] {
                wrong.push(format!("typed: {typed:?}"));
            }
        }
        _ => wrong.push("typed: not solutions".to_owned()),
    }
    for (format, reference) in [
        (ResultsFormat::Json, QueryResultsFormat::Json),
        (ResultsFormat::Tsv, QueryResultsFormat::Tsv),
        (ResultsFormat::Csv, QueryResultsFormat::Csv),
    ] {
        let mut written = Vec::new();
        match write_results(view, &query, options, format, None, &mut written) {
            Some(result) => result.unwrap(),
            None if direct => {
                wrong.push(format!("{format:?}: not written directly"));
                continue;
            }
            None => continue,
        }
        let serialiser = serialised(evaluate_query(view, &query, &general).unwrap(), reference);
        if written != serialiser {
            wrong.push(format!(
                "{format:?}: {:?}, the serialiser: {:?}",
                String::from_utf8_lossy(&written),
                String::from_utf8_lossy(&serialiser)
            ));
        }
    }
    let explained = explain_query(view, &query, options).unwrap();
    if explained.rows != expected.len() as u64 - 1 {
        wrong.push(format!("explain: {} rows", explained.rows));
    }
    let operators = explained
        .steps
        .iter()
        .map(|s| s.operator.as_str())
        .collect::<Vec<_>>();
    if (operators == ["index answer"]) != indexed {
        wrong.push(format!("route: {operators:?}"));
    }
    wrong
        .into_iter()
        .map(|w| format!("{text}\n  {w}"))
        .collect()
}

/// The index answer's own shape, and each query that differs from it in one part.
#[test]
fn a_count_of_one_pattern_is_the_index_s_in_every_delivery() {
    let engine = engine();
    let snapshot = engine.snapshot();
    let n = |k: u64| vec!["n".to_owned(), count(k)];
    let none = || vec!["n".to_owned()];
    let default = QueryOptions::default();
    let cases: Vec<(&str, Vec<String>, bool)> = vec![
        (
            "SELECT (COUNT(*) AS ?n) WHERE { ?s a e:Person }",
            n(5),
            true,
        ),
        ("SELECT (COUNT(*) AS ?n) WHERE { ?s ?p ?o }", n(8), true),
        ("SELECT (COUNT(*) AS ?n) WHERE { e:s1 ?p ?o }", n(3), true),
        // A ground triple term is a constant of the pattern; an open one is rewritten.
        (
            "SELECT (COUNT(*) AS ?n) WHERE { ?d e:says <<( e:s1 e:knows e:s2 )>> }",
            n(1),
            true,
        ),
        (
            "SELECT (COUNT(*) AS ?n) WHERE { ?d e:says <<( ?a e:knows ?b )>> }",
            n(1),
            false,
        ),
        // A repeated variable: the scan counts.
        (
            "SELECT (COUNT(*) AS ?n) WHERE { ?s e:knows ?s }",
            n(1),
            false,
        ),
        (
            "SELECT (COUNT(*) AS ?m) WHERE { ?s a e:Person }",
            vec!["m".to_owned(), count(5)],
            true,
        ),
        (
            "SELECT (COUNT(DISTINCT *) AS ?n) WHERE { ?s a e:Person }",
            n(5),
            false,
        ),
        (
            "SELECT (COUNT(?s) AS ?n) WHERE { ?s a e:Person }",
            n(5),
            false,
        ),
        (
            "SELECT (COUNT(*) + 0 AS ?n) WHERE { ?s a e:Person }",
            n(5),
            false,
        ),
        (
            "SELECT (COUNT(*) AS ?n) (2 AS ?two) WHERE { ?s a e:Person }",
            vec!["n two".to_owned(), format!("{} {}", count(5), count(2))],
            false,
        ),
        (
            "SELECT (COUNT(*) AS ?n) WHERE { ?s a e:Person } GROUP BY ?s",
            vec![
                "n".to_owned(),
                count(1),
                count(1),
                count(1),
                count(1),
                count(1),
            ],
            false,
        ),
        (
            "SELECT (COUNT(*) AS ?n) WHERE { ?s a e:Person } HAVING (COUNT(*) > 9)",
            none(),
            false,
        ),
        (
            "SELECT (COUNT(*) AS ?n) WHERE { ?s a e:Person . ?s e:knows ?o }",
            n(2),
            false,
        ),
        (
            "SELECT (COUNT(*) AS ?n) WHERE { ?s a e:Person BIND (1 AS ?b) }",
            n(5),
            false,
        ),
        (
            // Joined after the aggregation (SPARQL 1.1 §18.2.4.3), so the count stays.
            "SELECT (COUNT(*) AS ?n) WHERE { ?s a e:Person } VALUES ?s { e:s1 }",
            n(5),
            false,
        ),
        (
            "SELECT (COUNT(*) AS ?n) WHERE { ?s a e:Person } LIMIT 1",
            n(5),
            false,
        ),
        (
            "SELECT (COUNT(*) AS ?n) WHERE { ?s a e:Person } OFFSET 1",
            none(),
            false,
        ),
        (
            "SELECT (COUNT(*) AS ?n) WHERE { ?s a e:Person } ORDER BY ?n",
            n(5),
            false,
        ),
        (
            "SELECT DISTINCT (COUNT(*) AS ?n) WHERE { ?s a e:Person }",
            n(5),
            false,
        ),
        (
            "SELECT REDUCED (COUNT(*) AS ?n) WHERE { ?s a e:Person }",
            n(5),
            false,
        ),
        (
            "SELECT (COUNT(*) AS ?n) WHERE { GRAPH ?g { ?s a e:Person } }",
            n(3),
            false,
        ),
        (
            "SELECT (COUNT(*) AS ?n) FROM e:g1 WHERE { ?s a e:Person }",
            n(2),
            true,
        ),
    ];
    let mut failures = Vec::new();
    for (text, expected, indexed) in &cases {
        failures.extend(wrong(&snapshot, text, &default, expected, *indexed, true));
    }

    let person = "SELECT (COUNT(*) AS ?n) WHERE { ?s a e:Person }";
    let in_graphs = |default: Vec<GraphName>| {
        let mut dataset = QueryDatasetSpecification::new();
        dataset.set_default_graph(default);
        dataset.set_available_named_graphs(vec![NamedOrBlankNode::NamedNode(ex("g2"))]);
        QueryOptions {
            dataset: Some(dataset),
            ..QueryOptions::default()
        }
    };
    let options: Vec<(&str, QueryOptions, Vec<String>, bool)> = vec![
        (
            "asserted",
            QueryOptions {
                read_model: ReadModel::Asserted,
                ..QueryOptions::default()
            },
            n(4),
            true,
        ),
        (
            "inferred",
            QueryOptions {
                read_model: ReadModel::Inferred,
                ..QueryOptions::default()
            },
            n(1),
            true,
        ),
        // Every graph merged: a statement in several graphs counts once, so the scan counts.
        (
            "union",
            QueryOptions {
                union_default_graph: true,
                ..QueryOptions::default()
            },
            n(6),
            false,
        ),
        ("protocol graph", in_graphs(vec![graph("g1")]), n(2), true),
        ("no such graph", in_graphs(vec![graph("none")]), n(0), false),
        (
            "pre-bound",
            QueryOptions {
                pre_bound: Some(Arc::new(
                    [(Variable::new_unchecked("s"), ex("s1").into())].into(),
                )),
                ..QueryOptions::default()
            },
            n(1),
            false,
        ),
        (
            "ql",
            QueryOptions {
                ql: Some(Arc::new(QlRewriting::new(Closure { lists: true }))),
                ..QueryOptions::default()
            },
            n(5),
            false,
        ),
    ];
    for (name, options, expected, indexed) in &options {
        failures.extend(
            wrong(&snapshot, person, options, expected, *indexed, true)
                .into_iter()
                .map(|w| format!("[{name}] {w}")),
        );
    }

    // Unpublished state: a writer's pending view, and the transaction itself.
    let mut tx = engine.transaction();
    for s in ["s8", "s9"] {
        tx.insert(person_quad(s).as_ref());
    }
    tx.remove(person_quad("s1").as_ref());
    failures.extend(wrong(
        &tx.pending_snapshot(),
        person,
        &default,
        &n(6),
        true,
        true,
    ));
    failures.extend(wrong(&tx, person, &default, &n(6), true, false));
    drop(tx);
    failures.extend(wrong(
        &engine.snapshot(),
        person,
        &default,
        &n(5),
        true,
        true,
    ));

    // Under equality: the members' copies are counted (late expansion), or canonical
    // answers count the class once.
    let equality = equality_engine();
    let snapshot = equality.snapshot();
    failures.extend(wrong(&snapshot, person, &default, &n(3), true, true));
    let canonical = QueryOptions {
        equality_canonical: true,
        ..QueryOptions::default()
    };
    failures.extend(wrong(&snapshot, person, &canonical, &n(2), false, true));

    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// An index answer is the request's like any other: its row and its count's literal stay
/// charged to the request's envelope while the result lives and are released with it, and a
/// cancelled request gets no answer, in each delivery.
#[test]
fn an_index_answer_is_charged_while_it_lives_and_stops_when_cancelled() {
    use nrese_exec::SharedBudget;
    use nrese_sparql::{CancellationToken, QueryEvaluationError, Resources};

    let engine = engine();
    let snapshot = engine.snapshot();
    let query = SparqlParser::new()
        .parse_query(&format!(
            "PREFIX e: <{EX}> SELECT (COUNT(*) AS ?n) WHERE {{ ?s a e:Person }}"
        ))
        .unwrap();
    let shared = SharedBudget::new(1 << 20);
    let charged = QueryOptions {
        resources: Resources {
            memory: nrese_exec::Envelope::new(None, Some(shared.clone())),
            ..Default::default()
        },
        ..QueryOptions::default()
    };
    let result = evaluate_query(&snapshot, &query, &charged).unwrap();
    assert!(shared.used() > 0, "a live answer's row isn't charged");
    drop(result);
    let typed = evaluate_query_typed(&snapshot, &query, &charged).unwrap();
    assert!(shared.used() > 0, "a live typed answer's row isn't charged");
    drop(typed);
    assert_eq!(shared.used(), 0, "a dropped answer keeps its charge");

    let token = CancellationToken::new();
    token.cancel();
    let cancelled = QueryOptions {
        resources: Resources {
            cancellation: Some(token),
            ..Default::default()
        },
        ..QueryOptions::default()
    };
    assert!(matches!(
        evaluate_query(&snapshot, &query, &cancelled),
        Err(QueryEvaluationError::Cancelled)
    ));
    assert!(matches!(
        explain_query(&snapshot, &query, &cancelled),
        Err(QueryEvaluationError::Cancelled)
    ));
    let mut out = Vec::new();
    let written = write_results(
        &snapshot,
        &query,
        &cancelled,
        ResultsFormat::Json,
        None,
        &mut out,
    );
    assert!(
        matches!(
            written,
            Some(Err(nrese_sparql::WriteResultsError::Evaluation(
                QueryEvaluationError::Cancelled
            )))
        ),
        "{written:?}"
    );
    assert!(out.is_empty(), "a cancelled request wrote {out:?}");
}

fn person_quad(subject: &str) -> Quad {
    person(subject, GraphName::DefaultGraph)
}
