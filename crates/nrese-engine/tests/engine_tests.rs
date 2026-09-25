//! E3 gate: snapshot isolation, transaction overlay semantics, abort, readers under a
//! concurrent writer, and a differential model test over real RDF terms.

use std::collections::HashSet;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use nrese_engine::{Engine, EngineConfig, GraphSelector, QuadPattern};
use oxrdf::vocab::xsd;
use oxrdf::{BlankNode, GraphName, Literal, NamedNode, Quad, Term};

fn iri(n: u64) -> NamedNode {
    NamedNode::new_unchecked(format!("http://example.com/{n}"))
}

fn quad(s: u64, p: u64, o: u64) -> Quad {
    Quad::new(iri(s), iri(p), iri(o), GraphName::DefaultGraph)
}

fn inline_engine() -> Engine {
    Engine::new(EngineConfig {
        background_maintenance: false,
        ..EngineConfig::default()
    })
    .expect("engine")
}

fn all_quads(engine: &Engine) -> HashSet<Quad> {
    let snapshot = engine.snapshot();
    snapshot
        .quads_for_pattern(&QuadPattern::all())
        .map(|q| snapshot.decode_quad(q).expect("decodable"))
        .collect()
}

#[test]
fn snapshots_are_isolated_from_later_commits() {
    let engine = inline_engine();
    let before = engine.snapshot();
    let mut tx = engine.transaction();
    assert!(tx.insert(quad(1, 2, 3).as_ref()));
    let summary = tx.commit().expect("commit");
    assert_eq!((summary.revision, summary.inserted), (1, 1));

    assert!(before.is_empty());
    assert_eq!(before.revision(), 0);
    assert!(
        before.lookup(iri(1).as_ref().into()).is_none(),
        "term newer than snapshot"
    );
    let after = engine.snapshot();
    assert_eq!(after.len(), 1);
    assert!(after.lookup_quad(quad(1, 2, 3).as_ref()).is_some());
}

#[test]
fn transaction_reads_see_pending_changes() {
    let engine = inline_engine();
    let mut tx = engine.transaction();
    tx.insert(quad(1, 2, 3).as_ref());
    tx.insert(quad(1, 2, 4).as_ref());
    tx.commit().expect("commit");

    let mut tx = engine.transaction();
    assert!(tx.remove(quad(1, 2, 3).as_ref()));
    assert!(!tx.remove(quad(1, 2, 3).as_ref()), "already removed");
    assert!(tx.insert(quad(1, 2, 5).as_ref()));
    assert!(!tx.insert(quad(1, 2, 4).as_ref()), "already present");
    let subject = tx.lookup(iri(1).as_ref().into()).unwrap();
    let pattern = QuadPattern {
        subject: Some(subject),
        ..QuadPattern::all()
    };
    let objects: HashSet<Term> = tx
        .quads_for_pattern(&pattern)
        .map(|q| tx.decode(q.object).unwrap())
        .collect();
    assert_eq!(objects, HashSet::from([iri(4).into(), iri(5).into()]));
    assert_eq!(tx.len(), 2);

    // Re-inserting a deleted quad and deleting a fresh insert both cancel out.
    assert!(tx.insert(quad(1, 2, 3).as_ref()));
    assert!(tx.remove(quad(1, 2, 5).as_ref()));
    assert_eq!(tx.pending(), (0, 0));
    let summary = tx.commit().expect("commit");
    assert_eq!(summary.revision, 1, "no net change, no new revision");
}

#[test]
fn dropping_a_transaction_aborts_it() {
    let engine = inline_engine();
    {
        let mut tx = engine.transaction();
        tx.insert(quad(1, 2, 3).as_ref());
    }
    assert!(engine.snapshot().is_empty());
    assert_eq!(engine.stats().revision, 0);
}

#[test]
fn remove_matching_clears_one_graph() {
    let engine = inline_engine();
    let graph = iri(100);
    let mut tx = engine.transaction();
    tx.insert(quad(1, 2, 3).as_ref());
    for n in 0..5 {
        tx.insert(Quad::new(iri(n), iri(2), iri(3), graph.clone()).as_ref());
    }
    tx.commit().expect("commit");

    let mut tx = engine.transaction();
    let graph_id = tx.lookup(graph.as_ref().into()).unwrap();
    assert_eq!(tx.remove_matching(&QuadPattern::in_graph(graph_id)), 5);
    tx.commit().expect("commit");
    let snapshot = engine.snapshot();
    assert_eq!(snapshot.len(), 1);
    assert_eq!(snapshot.named_graphs().count(), 0);
}

#[test]
fn named_graphs_lists_non_empty_graphs() {
    let engine = inline_engine();
    let mut tx = engine.transaction();
    let graphs = [
        GraphName::from(iri(10)),
        BlankNode::new_unchecked("g").into(),
        iri(11).into(),
    ];
    for graph in &graphs {
        tx.insert(Quad::new(iri(1), iri(2), iri(3), graph.clone()).as_ref());
    }
    tx.insert(quad(1, 2, 3).as_ref());
    tx.commit().expect("commit");

    let snapshot = engine.snapshot();
    let listed: HashSet<_> = snapshot
        .named_graphs()
        .map(|g| snapshot.decode(g).unwrap())
        .collect();
    let expected: HashSet<Term> = HashSet::from([
        iri(10).into(),
        iri(11).into(),
        BlankNode::new_unchecked("g").into(),
    ]);
    assert_eq!(listed, expected);
    let named = QuadPattern {
        graph: GraphSelector::AnyNamed,
        ..QuadPattern::all()
    };
    assert_eq!(snapshot.quads_for_pattern(&named).count(), 3);
}

#[test]
fn readers_are_not_blocked_by_an_open_transaction() {
    let engine = inline_engine();
    let tx = engine.transaction();
    // Would deadlock if snapshots needed the writer slot.
    let snapshot = std::thread::scope(|scope| scope.spawn(|| engine.snapshot()).join().unwrap());
    assert!(snapshot.is_empty());
    drop(tx);
}

#[test]
fn concurrent_readers_only_see_whole_commits() {
    const BATCH: u64 = 7;
    let engine = Engine::new(EngineConfig::default()).expect("engine");
    let done = Arc::new(AtomicBool::new(false));
    std::thread::scope(|scope| {
        for _ in 0..3 {
            let engine = engine.clone();
            let done = Arc::clone(&done);
            scope.spawn(move || {
                let mut last_revision = 0;
                while !done.load(Ordering::Acquire) {
                    let snapshot = engine.snapshot();
                    let counted = snapshot.quads_for_pattern(&QuadPattern::all()).count() as u64;
                    assert_eq!(counted, snapshot.len());
                    assert_eq!(counted % BATCH, 0, "saw a partial commit");
                    assert_eq!(counted, snapshot.revision() * BATCH);
                    assert!(
                        snapshot.revision() >= last_revision,
                        "revisions go backwards"
                    );
                    last_revision = snapshot.revision();
                }
            });
        }
        for commit in 0..300 {
            let mut tx = engine.transaction();
            for n in 0..BATCH {
                tx.insert(quad(commit, 0, n).as_ref());
            }
            tx.commit().expect("commit");
        }
        done.store(true, Ordering::Release);
    });
    engine.compact();
    assert_eq!(engine.snapshot().len(), 300 * BATCH);
    let stats = engine.stats();
    assert!(
        stats.runs <= 8,
        "compaction keeps the run count logarithmic: {stats:?}"
    );
}

/// Random terms of every shape, including inline integers and non-canonical lexical forms.
fn random_term(state: &mut u64) -> Term {
    *state = state
        .wrapping_mul(6364136223846793005)
        .wrapping_add(1442695040888963407);
    let r = *state >> 33;
    match r % 6 {
        0 | 1 => iri(r % 7).into(),
        2 => BlankNode::new_unchecked(format!("b{}", r % 3)).into(),
        3 => Literal::new_typed_literal(format!("{}", r % 4), xsd::INTEGER).into(),
        4 => Literal::new_typed_literal(format!("0{}", r % 2), xsd::INTEGER).into(),
        _ => Literal::new_language_tagged_literal_unchecked(format!("t{}", r % 3), "en").into(),
    }
}

#[test]
fn random_transactions_match_a_model() {
    let engine = Engine::new(EngineConfig::default()).expect("engine");
    let mut model = HashSet::new();
    let mut state = 42u64;
    for _ in 0..200 {
        let mut tx = engine.transaction();
        let mut pending = model.clone();
        let ops = 1 + (random_term(&mut state).to_string().len() % 8);
        for _ in 0..ops {
            let subject: oxrdf::NamedOrBlankNode = match random_term(&mut state) {
                Term::Literal(_) => iri(0).into(),
                Term::NamedNode(n) => n.into(),
                Term::BlankNode(b) => b.into(),
            };
            let graph = if state.is_multiple_of(3) {
                GraphName::DefaultGraph
            } else {
                iri(20 + state % 2).into()
            };
            let q = Quad::new(subject, iri(state % 3), random_term(&mut state), graph);
            if state.is_multiple_of(4) {
                assert_eq!(tx.remove(q.as_ref()), pending.remove(&q));
            } else {
                assert_eq!(tx.insert(q.as_ref()), pending.insert(q));
            }
        }
        assert_eq!(tx.len(), pending.len() as u64);
        if state.is_multiple_of(5) {
            drop(tx); // abort
        } else {
            tx.commit().expect("commit");
            model = pending;
        }
        assert_eq!(all_quads(&engine), model);
    }
}
