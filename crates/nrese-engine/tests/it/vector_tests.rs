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
        .vector_search(&query, &|_| true)
        .expect("an unlimited search");
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
        .vector_search(&query, &odd)
        .expect("an unlimited search");
    assert_eq!(angles(&engine, &found), [91, 89, 93]);
    // Euclidean: the same order on a circle.
    let euclidean = VectorQuery {
        metric: Metric::L2,
        ..query.clone()
    };
    let (found, _) = before
        .vector_search(&euclidean, &|_| true)
        .expect("an unlimited search");
    assert_eq!(angles(&engine, &found), [90, 91, 89]);
    // Another dimension finds nothing.
    let (found, report) = before
        .vector_search(&VectorQuery::new(vec![1.0, 0.0, 0.0], 3), &|_| true)
        .expect("an unlimited search");
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
        .vector_search(&query, &|_| true)
        .expect("an unlimited search");
    assert_eq!(angles(&engine, &found), [90, 91, 89]);
    let (found, _) = engine
        .snapshot()
        .vector_search(&query, &|_| true)
        .expect("an unlimited search");
    assert!(
        found[0].1 < 1e-6,
        "the new literal is the nearest: {found:?}"
    );
}

/// An answer depends on the vectors the reader sees alone: with deleted vectors and an
/// open transaction's in the dictionary, each nearer to a query than any visible one, an
/// approximate search finds the nearest visible ones, as comparing every one the filter
/// takes does (the review's case of 19 October: one graph of every vector the dictionary
/// held, whose links and descent ran through the hidden ones).
#[test]
fn hidden_vectors_change_no_answer() {
    use nrese_engine::{QuadPattern, VectorStrategy};
    let engine = engine();
    let mut state = 0x2545_f491_4f6c_dd1du64;
    let mut random = move || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        (state >> 40) as f32 / (1u64 << 24) as f32 * 2.0 - 1.0
    };
    let queries: Vec<Vec<f32>> = (0..20)
        .map(|_| (0..16).map(|_| random()).collect())
        .collect();
    let quad = |name: String, values: &[f32]| {
        let literal =
            Literal::new_typed_literal(lexical(values), NamedNode::new_unchecked(DATATYPE));
        Quad::new(
            NamedNode::new_unchecked(format!("http://example.com/{name}")),
            NamedNode::new_unchecked("http://example.com/embedding"),
            literal,
            nrese_rdf::GraphName::DefaultGraph,
        )
    };
    // Near a query: within a hundredth on each axis.
    let near = |q: &[f32], random: &mut dyn FnMut() -> f32| -> Vec<f32> {
        q.iter().map(|x| x + random() * 0.01).collect()
    };
    let visible: Vec<Quad> = (0..20_000)
        .map(|i| {
            let values: Vec<f32> = (0..16).map(|_| random()).collect();
            quad(format!("v{i}"), &values)
        })
        .collect();
    let deleted: Vec<Quad> = (0..20_000)
        .map(|i| quad(format!("d{i}"), &near(&queries[i % 20], &mut random)))
        .collect();
    let mut tx = engine.transaction();
    for quad in visible.iter().chain(&deleted) {
        tx.insert(QuadRef::from(quad));
    }
    tx.commit().expect("commit");
    let mut tx = engine.transaction();
    for quad in &deleted {
        tx.remove(QuadRef::from(quad));
    }
    tx.commit().expect("commit");
    let mut open = engine.transaction();
    for i in 0..4_000 {
        let values = near(&queries[i % 20], &mut random);
        open.insert(QuadRef::from(&quad(format!("o{i}"), &values)));
    }
    let snapshot = engine.snapshot();
    let seen = |id: TermId| {
        let pattern = QuadPattern {
            object: Some(id),
            ..QuadPattern::all()
        };
        snapshot.quads_for_pattern(&pattern).next().is_some()
    };
    for query in &queries {
        let approximate = VectorQuery {
            strategy: VectorStrategy::Approximate,
            ..VectorQuery::new(query.clone(), 10)
        };
        let exact = VectorQuery {
            strategy: VectorStrategy::Exact,
            ..approximate.clone()
        };
        let (found, _) = snapshot.vector_search(&approximate, &seen).unwrap();
        let (truth, _) = snapshot.vector_search(&exact, &seen).unwrap();
        assert_eq!(found.len(), 10);
        assert_eq!(found, truth);
    }
    drop(open);
}

fn embedding(i: usize, graph: nrese_rdf::GraphName) -> Quad {
    Quad::new(
        NamedNode::new_unchecked(format!("urn:item:{i}")),
        NamedNode::new_unchecked("urn:embedding"),
        Literal::new_typed_literal(format!("[{i}]"), NamedNode::new_unchecked(DATATYPE)),
        graph,
    )
}

/// A search's admission, refusal and report depend on the vectors its reader may read alone:
/// 10,000 vectors in a graph outside the scope, committed after the snapshot or visible to
/// a later one, change nothing under a hit limit or a byte budget (Sol's review of W2, r1
/// F1: the earlier snapshot's search was refused as `TooLarge { hits: 40 }`, counted over
/// every vector of the dictionary).
#[test]
fn hidden_vectors_change_no_admission_nor_report() {
    use nrese_engine::{GraphSelector, ReadModel, TextScope};
    let engine = engine();
    let mut tx = engine.transaction();
    tx.insert(QuadRef::from(&embedding(
        0,
        nrese_rdf::GraphName::DefaultGraph,
    )));
    tx.commit().expect("commit");
    let old = engine.snapshot();
    let scope = TextScope::Graphs(GraphSelector::Exact(TermId::DEFAULT_GRAPH));
    let query = VectorQuery {
        max_hits: Some(20),
        ..VectorQuery::new(vec![0.0], 10)
    };
    let search = |snapshot: &nrese_engine::Snapshot, budget: Option<u64>| {
        let budget =
            budget.map(|bytes| std::sync::Arc::new(nrese_exec::Budget::new(bytes as usize)));
        snapshot
            .vector_search_in(
                ReadModel::Asserted,
                &scope,
                &query,
                &|_| true,
                budget.as_ref(),
            )
            .map(|(hits, report)| (hits.to_vec(), report))
    };
    let before = search(&old, None).expect("one vector");
    assert_eq!(before.0.len(), 1);
    let mut tx = engine.transaction();
    for i in 1..10_001 {
        let private = NamedNode::new_unchecked("urn:private").into();
        tx.insert(QuadRef::from(&embedding(i, private)));
    }
    tx.commit().expect("commit");
    for snapshot in [&old, &engine.snapshot()] {
        for budget in [None, Some(64 << 10)] {
            assert_eq!(
                search(snapshot, budget),
                Ok(before.clone()),
                "budget {budget:?}"
            );
        }
    }
}

/// The hits a search returns keep their charge until they drop (Sol's review of W2, r1 F2:
/// the scan's reservation went as the search returned).
#[test]
fn returned_vector_hits_keep_their_reservation() {
    use nrese_engine::{GraphSelector, ReadModel, TextScope};
    let engine = engine();
    let mut tx = engine.transaction();
    tx.insert(QuadRef::from(&embedding(
        0,
        nrese_rdf::GraphName::DefaultGraph,
    )));
    tx.commit().expect("commit");
    let snapshot = engine.snapshot();
    let scope = TextScope::Graphs(GraphSelector::Exact(TermId::DEFAULT_GRAPH));
    let query = VectorQuery::new(vec![0.0], 1);
    let search = |budget| {
        snapshot
            .vector_search_in(ReadModel::Asserted, &scope, &query, &|_| true, budget)
            .expect("one vector")
    };
    // The corpus kept first, on the engine's budget: the request's holds the scan's alone.
    drop(search(None));
    let budget = std::sync::Arc::new(nrese_exec::Budget::new(1 << 20));
    let (hits, _) = search(Some(&budget));
    assert_eq!(hits.len(), 1);
    assert!(budget.used() >= std::mem::size_of::<(TermId, f32)>());
    drop(hits);
    assert_eq!(budget.used(), 0);
}
