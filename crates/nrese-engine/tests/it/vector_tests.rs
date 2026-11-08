//! Vector search over the dictionary's vector literals (`Snapshot::vector_search`).

use nrese_engine::vector::{DATATYPE, Metric, lexical};
use nrese_engine::{Engine, EngineConfig, TermId, VectorQuery};
use nrese_rdf::{Literal, NamedNode, Quad, QuadRef};

fn engine() -> Engine {
    Engine::new(EngineConfig {
        background_maintenance: false,
        ..EngineConfig::default()
    })
    .expect("engine")
}

/// `n` statements `<item/i> <embedding> "[...]"^^nrv:vector`, the vectors on a circle
/// (angle i degrees), so the nearest to an angle are known.
fn store_circle(engine: &Engine, n: usize, from: usize) {
    let mut tx = engine.transaction();
    for i in from..from + n {
        let angle = (i as f32).to_radians();
        let literal = Literal::new_typed_literal(
            lexical(&[angle.cos(), angle.sin()]),
            NamedNode::new_unchecked(DATATYPE),
        );
        let quad = Quad::new(
            NamedNode::new_unchecked(format!("http://example.com/item/{i}")),
            NamedNode::new_unchecked("http://example.com/embedding"),
            literal,
            nrese_rdf::GraphName::DefaultGraph,
        );
        tx.insert(QuadRef::from(&quad));
    }
    tx.commit().expect("commit");
}

/// The angles of the vector literals found.
fn angles(engine: &Engine, found: &[(TermId, f32)]) -> Vec<i64> {
    let snapshot = engine.snapshot();
    found
        .iter()
        .map(|(id, _)| {
            let Some(nrese_rdf::Term::Literal(literal)) = snapshot.decode(*id) else {
                panic!("a vector literal");
            };
            let values = nrese_engine::vector::parse(literal.value()).unwrap();
            (values[1].atan2(values[0]).to_degrees().round() as i64).rem_euclid(360)
        })
        .collect()
}

#[test]
fn the_nearest_vector_literals_are_found() {
    let engine = engine();
    store_circle(&engine, 360, 0);
    let before = engine.snapshot();
    let angle = 90.4f32.to_radians();
    let query = VectorQuery::new(vec![angle.cos(), angle.sin()], 3);
    let (found, report) = before
        .vector_search(&query, &|_| true, crate::unlimited())
        .unwrap();
    assert_eq!(angles(&engine, &found), [90, 91, 89]);
    assert_eq!(report.space, 360);
    assert!(!report.graph, "a small space is scanned");
    assert!(found.windows(2).all(|w| w[0].1 <= w[1].1));
    // A filter: odd angles only.
    let odd = |id: TermId| {
        let snapshot = engine.snapshot();
        let Some(nrese_rdf::Term::Literal(literal)) = snapshot.decode(id) else {
            return false;
        };
        let values = nrese_engine::vector::parse(literal.value()).unwrap();
        (values[1].atan2(values[0]).to_degrees().round() as i64).rem_euclid(2) == 1
    };
    let (found, _) = before
        .vector_search(&query, &odd, crate::unlimited())
        .unwrap();
    assert_eq!(angles(&engine, &found), [91, 89, 93]);
    // Euclidean: the same order on a circle.
    let euclidean = VectorQuery {
        metric: Metric::L2,
        ..query.clone()
    };
    let (found, _) = before
        .vector_search(&euclidean, &|_| true, crate::unlimited())
        .unwrap();
    assert_eq!(angles(&engine, &found), [90, 91, 89]);
    // Another dimension finds nothing.
    let (found, report) = before
        .vector_search(
            &VectorQuery::new(vec![1.0, 0.0, 0.0], 3),
            &|_| true,
            crate::unlimited(),
        )
        .unwrap();
    assert!(found.is_empty() && report.space == 0);
    // A snapshot doesn't see literals interned after it.
    let mut tx = engine.transaction();
    let near = Literal::new_typed_literal(
        lexical(&[angle.cos(), angle.sin()]),
        NamedNode::new_unchecked(DATATYPE),
    );
    tx.insert(QuadRef::new(
        NamedNode::new_unchecked("http://example.com/new").as_ref(),
        NamedNode::new_unchecked("http://example.com/embedding").as_ref(),
        near.as_ref(),
        nrese_rdf::GraphNameRef::DefaultGraph,
    ));
    tx.commit().expect("commit");
    let (found, _) = before
        .vector_search(&query, &|_| true, crate::unlimited())
        .unwrap();
    assert_eq!(angles(&engine, &found), [90, 91, 89]);
    let (found, _) = engine
        .snapshot()
        .vector_search(&query, &|_| true, crate::unlimited())
        .unwrap();
    assert!(
        found[0].1 < 1e-6,
        "the new literal is the nearest: {found:?}"
    );
}

// The large space searched through its graph is `tests/vector_graph_recall.rs`: alone in a process
// whose Rayon pool has one thread, so its graph is built the same way every run.
