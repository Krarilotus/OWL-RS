//! A large vector space is searched through its graph and finds what the exact scan finds.
//!
//! The graph is built by parallel insertion on Rayon's global pool (the engine starts the build
//! with `rayon::spawn`), and which inserts overlap changes the graph from run to run: its recall
//! isn't a guarantee the product makes, so a test can't hold it (8 Nov: 9 of 10 in 31 of 660 runs,
//! 7 of 10 once in a loaded gate; `notes/2026-11-08-p2-a4-vector-flake.md` in the harness). The
//! test runs alone in a process of its own ([`measure::alone`]), whose global pool it builds with
//! one thread first, so the graph is built the same way every run (10 of 10 in 60 of 60 runs on
//! one thread). This file is a test binary of its own, as `alone` needs the measured allocator.

use nrese_engine::vector::{DATATYPE, Metric, lexical};
use nrese_engine::{Engine, EngineConfig, VectorQuery, VectorStrategy};
use nrese_exec::{Budget, measure};
use nrese_rdf::{Literal, NamedNode, QuadRef};

#[global_allocator]
static ALLOCATOR: measure::Measured<std::alloc::System> = measure::Measured(std::alloc::System);

/// A large space is searched through its graph and finds what the exact scan finds.
#[test]
fn large_spaces_are_searched_through_a_graph() {
    if !measure::alone() {
        return;
    }
    rayon::ThreadPoolBuilder::new()
        .num_threads(1)
        .build_global()
        .expect("this process's global pool, built before anything uses it");
    let unlimited = Budget::unlimited();
    let engine = Engine::new(EngineConfig {
        background_maintenance: false,
        ..EngineConfig::default()
    })
    .expect("engine");
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
    let (approximate, report) = snapshot
        .vector_search(&query, &|_| true, &unlimited)
        .unwrap();
    assert!(
        report.graph && report.space == 72_000 && report.scanned == 0,
        "{report:?}"
    );
    let exact = VectorQuery {
        strategy: VectorStrategy::Exact,
        ..query.clone()
    };
    let (truth, report) = snapshot
        .vector_search(&exact, &|_| true, &unlimited)
        .unwrap();
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
    let (_, report) = snapshot
        .vector_search(&selective, &|_| true, &unlimited)
        .unwrap();
    assert!(!report.graph);
}
