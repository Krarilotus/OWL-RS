//! The corpora of reader-scoped searches and their lineage are bounded by their budget alone
//! (master 7 Nov 17:28 and 17:34, IB1727-3bc5 and IB1728-d941): no count of corpora (see
//! `kept_corpora_are_bounded_and_shared_by_equal_graph_sets`), no count of lineage steps,
//! no count of the terms the steps hold. Within the budget a corpus is derived over as long a
//! lineage as reaches a kept ancestor; past it the budget refuses what would grow, before it
//! is allocated, and searches stay exact.
//!
//! Observable: `text_corpus_stats` (corpora walked or derived; bytes held against the
//! budget), and answers against a fresh engine with the same statements (a walk).

use std::sync::Arc;

use nrese_engine::{Engine, EngineConfig};
use nrese_rdf::{BlankNode, GraphName, NamedNode, Quad};

use crate::text_scope_tests::{base, built, every, label, texts};

/// A statement that touches no term a corpus holds: a blank-node subject (not an IRI) and an
/// IRI object (not a literal).
fn untouching(i: usize) -> Quad {
    Quad::new(
        BlankNode::new_unchecked(format!("b{i}")),
        NamedNode::new_unchecked("http://example.com/p"),
        NamedNode::new_unchecked("http://example.com/o"),
        GraphName::DefaultGraph,
    )
}

/// 4,100 commits between a kept corpus and the next search, more steps than the old count
/// (4,096) held: the corpus is derived over all of them (none touched a term it holds),
/// not walked, and answers as before.
#[test]
fn a_lineage_longer_than_any_step_count_still_derives() {
    let engine = Engine::new(EngineConfig::default()).unwrap();
    base(&engine);
    let before = engine.snapshot();
    let answer = every(&before, "alpha beta");
    assert_eq!(built(&before), (1, 0));
    for i in 0..4100 {
        let mut tx = engine.transaction();
        tx.insert(untouching(i).as_ref());
        tx.commit().unwrap();
    }
    let after = engine.snapshot();
    assert_eq!(every(&after, "alpha beta"), answer);
    assert_eq!(built(&after), (1, 1), "{:?}", after.text_corpus_stats());
}

/// A commit of 140,000 labels (280,000 terms, more than the old term count of 2^18 held)
/// after a small one: the version between them still derives its corpus over the small
/// step from the kept one, not walked, and answers as a fresh engine with its statements.
#[test]
fn a_large_later_commit_leaves_the_lineage_of_earlier_versions() {
    let engine = Engine::new(EngineConfig::default()).unwrap();
    // A corpus of 11 documents, derived over the small commit's 2 touched terms.
    engine.set_corpus_tuning(nrese_engine::CorpusTuning {
        documents_per_probe: 1,
        ..nrese_engine::CorpusTuning::DEFAULT
    });
    base(&engine);
    every(&engine.snapshot(), "alpha");
    let mut tx = engine.transaction();
    tx.insert(label("c", "gamma beta", None).as_ref());
    tx.commit().unwrap();
    let between = engine.snapshot();
    let mut tx = engine.transaction();
    for i in 0..140_000 {
        tx.insert(label(&format!("w{i}"), &format!("word{i}"), None).as_ref());
    }
    tx.commit().unwrap();
    let found = every(&between, "alpha beta");
    assert_eq!(built(&between), (1, 1), "{:?}", between.text_corpus_stats());
    let fresh = Engine::new(EngineConfig::default()).unwrap();
    base(&fresh);
    let mut tx = fresh.transaction();
    tx.insert(label("c", "gamma beta", None).as_ref());
    tx.commit().unwrap();
    let oracle = fresh.snapshot();
    assert_eq!(
        texts(&between, &found),
        texts(&oracle, &every(&oracle, "alpha beta"))
    );
}

/// Guard: with a budget of 64 KiB, 2,000 commits each record a step (about 120 bytes each,
/// 240 KB together if all were kept): what the store holds never passes the budget, so the
/// budget refused the steps that didn't fit before they were allocated; the search after
/// them answers as a fresh engine with the same statements.
#[test]
fn the_budget_refuses_lineage_growth_before_it_is_held() {
    let engine = Engine::new(EngineConfig::default()).unwrap();
    engine.set_text_corpus_budget(Arc::new(nrese_exec::Budget::new(64 << 10)));
    base(&engine);
    every(&engine.snapshot(), "alpha");
    let fresh = Engine::new(EngineConfig::default()).unwrap();
    base(&fresh);
    for i in 0..2000 {
        for engine in [&engine, &fresh] {
            let mut tx = engine.transaction();
            tx.insert(label(&format!("n{i}"), &format!("beta number{i}"), None).as_ref());
            tx.commit().unwrap();
        }
        let stats = engine.snapshot().text_corpus_stats();
        assert!(stats.bytes <= stats.budget, "commit {i}: {stats:?}");
    }
    let (snapshot, oracle) = (engine.snapshot(), fresh.snapshot());
    assert_eq!(
        texts(&snapshot, &every(&snapshot, "alpha beta")),
        texts(&oracle, &every(&oracle, "alpha beta"))
    );
    let stats = snapshot.text_corpus_stats();
    assert!(stats.bytes <= stats.budget, "{stats:?}");
}
