//! `COUNT(?v)` and `COUNT(DISTINCT ?v)` of one triple pattern from the index against the
//! reference evaluator: random data whose objects are mostly entities, committed in
//! several revisions with deletions, a third of it inferred, a little of it in a named
//! graph, every query under a random read model.

use nrese_engine::{EncodedTriple, Engine, EngineConfig, ReadModel};
use nrese_rdf::vocab::xsd;
use nrese_rdf::{GraphName, Literal, Quad, Term};
use nrese_sparql::{QueryOptions, QueryResults, evaluate_query, explain_query};
use nrese_sparql_syntax::SparqlParser;

use crate::native_differential_tests::{EX, Rng, ex};

fn object(rng: &mut Rng) -> Term {
    match rng.below(8) {
        0 => Literal::new_typed_literal(rng.below(4).to_string(), xsd::INTEGER).into(),
        1 => Literal::new_simple_literal(format!("s{}", rng.below(3))).into(),
        _ => ex(&format!("e{}", rng.below(8))).into(),
    }
}

fn dataset(rng: &mut Rng) -> Vec<Quad> {
    (0..20 + rng.below(80))
        .map(|_| {
            let graph = match rng.below(12) {
                0 => GraphName::NamedNode(ex("g")),
                _ => GraphName::DefaultGraph,
            };
            Quad::new(
                ex(&format!("e{}", rng.below(8))),
                ex(&format!("p{}", rng.below(4))),
                object(rng),
                graph,
            )
        })
        .collect()
}

/// The rows, each as its bindings, sorted.
fn count(results: QueryResults<'_>) -> Vec<String> {
    let QueryResults::Solutions(solutions) = results else {
        panic!("a SELECT")
    };
    let mut rows: Vec<String> = solutions
        .map(|s| {
            let s = s.unwrap();
            ["v0", "v1", "v2", "v3", "v4", "v5", "v6", "n", "m"]
                .iter()
                .filter_map(|v| s.get(*v).map(|t| format!("{v}={t}")))
                .collect::<Vec<_>>()
                .join(" ")
        })
        .collect();
    rows.sort();
    rows
}

/// `COUNT(?v)` and `COUNT(DISTINCT ?v)` of one pattern from the index (its count, its
/// groups counted as they are walked) against the reference evaluator: patterns with
/// constants in every position, over data committed in several revisions with deletions
/// (several runs, tombstones: the walk declines and the groups are listed), a third
/// inferred, every read model.
#[test]
fn single_pattern_counts_equal_the_reference() {
    let mut rng = Rng::seeded(20_261_015);
    let (mut checked, mut walked) = (0usize, 0usize);
    for case in 0..30 {
        let engine = Engine::new(EngineConfig::default()).unwrap();
        for _ in 0..1 + rng.below(3) {
            let mut tx = engine.transaction();
            for quad in dataset(&mut rng) {
                match rng.below(6) {
                    0 if quad.graph_name.is_default_graph() => {
                        let triple = EncodedTriple::new(
                            tx.intern(quad.subject.as_ref().into()),
                            tx.intern(quad.predicate.as_ref().into()),
                            tx.intern(quad.object.as_ref()),
                        );
                        tx.insert_inferred(triple);
                    }
                    1 => {
                        tx.remove(quad.as_ref());
                    }
                    _ => {
                        tx.insert(quad.as_ref());
                    }
                }
            }
            tx.commit().unwrap();
        }
        let snapshot = engine.snapshot();
        for _ in 0..30 {
            let options = QueryOptions {
                read_model: *rng.pick(&[
                    ReadModel::Materialised,
                    ReadModel::Asserted,
                    ReadModel::Inferred,
                ]),
                ..QueryOptions::default()
            };
            let term = |rng: &mut Rng, v: &str| match rng.below(3) {
                0 => format!("<{EX}e{}>", rng.below(9)),
                _ => v.to_owned(),
            };
            let (s, o) = (term(&mut rng, "?s"), term(&mut rng, "?o"));
            let p = match rng.below(2) {
                0 => "?p".to_owned(),
                _ => format!("<{EX}p{}>", rng.below(5)),
            };
            let vars: Vec<&str> = [s.as_str(), p.as_str(), o.as_str()]
                .into_iter()
                .filter(|t| t.starts_with('?'))
                .collect();
            let Some(&v) = vars.get(rng.below(vars.len().max(1) as u64) as usize) else {
                continue;
            };
            let distinct = if rng.below(2) == 0 { "DISTINCT " } else { "" };
            let text = format!("SELECT (COUNT({distinct}{v}) AS ?n) WHERE {{ {s} {p} {o} }}");
            let query = SparqlParser::new()
                .parse_query(&text)
                .unwrap_or_else(|e| panic!("{e}: {text}"));
            let native = count(evaluate_query(&snapshot, &query, &options).unwrap());
            let expected =
                count(nrese_sparql_reference::evaluate_query(&snapshot, &query, &options).unwrap());
            assert_eq!(
                native, expected,
                "case {case}, {:?}: {text}",
                options.read_model
            );
            let explained = explain_query(&snapshot, &query, &options).unwrap();
            walked += usize::from(
                explained
                    .steps
                    .iter()
                    .any(|s| s.operator == "distinct count"),
            );
            checked += 1;
        }
    }
    assert!(
        walked * 4 > checked,
        "{checked} checked, {walked} distinct counts"
    );
}
