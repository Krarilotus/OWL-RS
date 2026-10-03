//! Vector search over the dictionary's vector literals (`Snapshot::vector_search`).

use nrese_engine::vector::{DATATYPE, Metric, lexical};
use nrese_engine::{Engine, EngineConfig, TermId, VectorQuery, VectorStrategy};
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
    let (found, report) = before.vector_search(&query, &|_| true);
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
    let (found, _) = before.vector_search(&query, &odd);
    assert_eq!(angles(&engine, &found), [91, 89, 93]);
    // Euclidean: the same order on a circle.
    let euclidean = VectorQuery {
        metric: Metric::L2,
        ..query.clone()
    };
    let (found, _) = before.vector_search(&euclidean, &|_| true);
    assert_eq!(angles(&engine, &found), [90, 91, 89]);
    // Another dimension finds nothing.
    let (found, report) =
        before.vector_search(&VectorQuery::new(vec![1.0, 0.0, 0.0], 3), &|_| true);
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
    let (found, _) = before.vector_search(&query, &|_| true);
    assert_eq!(angles(&engine, &found), [90, 91, 89]);
    let (found, _) = engine.snapshot().vector_search(&query, &|_| true);
    assert!(
        found[0].1 < 1e-6,
        "the new literal is the nearest: {found:?}"
    );
}

/// A large space is searched through its graph and finds what the exact scan finds.
#[test]
fn large_spaces_are_searched_through_a_graph() {
    let engine = engine();
    // 72,000 points: the circle with 200 radii.
    let mut tx = engine.transaction();
    for i in 0..72_000usize {
        let angle = ((i % 360) as f32 + (i / 360) as f32 * 0.001).to_radians();
        let radius = 1.0 + (i / 360) as f32 * 0.01;
        let literal = Literal::new_typed_literal(
            lexical(&[radius * angle.cos(), radius * angle.sin(), (i % 7) as f32]),
            NamedNode::new_unchecked(DATATYPE),
        );
        tx.insert(QuadRef::new(
            NamedNode::new_unchecked(format!("http://example.com/item/{i}")).as_ref(),
            NamedNode::new_unchecked("http://example.com/embedding").as_ref(),
            literal.as_ref(),
            nrese_rdf::GraphNameRef::DefaultGraph,
        ));
    }
    tx.commit().expect("commit");
    let snapshot = engine.snapshot();
    let query = VectorQuery {
        metric: Metric::L2,
        ..VectorQuery::new(vec![0.5, 1.2, 3.0], 10)
    };
    // Past SYNC_BUILD the graph is built on a thread; a warm-up waits for it.
    snapshot.prepare_vector_graph(&query);
    let (approximate, report) = snapshot.vector_search(&query, &|_| true);
    assert!(
        report.graph && report.space == 72_000 && report.scanned == 0,
        "{report:?}"
    );
    let exact = VectorQuery {
        strategy: VectorStrategy::Exact,
        ..query.clone()
    };
    let (truth, report) = snapshot.vector_search(&exact, &|_| true);
    assert!(!report.graph);
    let shared = approximate
        .iter()
        .filter(|(id, _)| truth.iter().any(|(t, _)| t == id))
        .count();
    assert!(shared >= 9, "{shared} of 10");
    // A selective filter is answered exactly.
    let selective = VectorQuery {
        accepted: Some(10),
        ..query
    };
    let (_, report) = snapshot.vector_search(&selective, &|_| true);
    assert!(!report.graph);
}
