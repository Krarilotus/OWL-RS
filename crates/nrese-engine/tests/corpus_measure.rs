//! What a full-text search allocates for its corpus is charged first (the memory-accounting
//! owner's `measure_within`): a walk, a derivation after a commit, and a corpus too large
//! to keep, each held against the budget of the search and the corpora's. Measured with the
//! counting allocator, so this file is a test binary of its own.

use std::sync::Arc;

use nrese_engine::{Engine, EngineConfig, ReadModel, Snapshot, TextScope};
use nrese_exec::measure::{Measured, alone, measure_within};
use nrese_exec::{Budget, Envelope};
use nrese_rdf::{GraphName, Literal, NamedNode, Quad};

#[global_allocator]
static ALLOCATOR: Measured<std::alloc::System> = Measured(std::alloc::System);

fn label(i: u64) -> Quad {
    Quad::new(
        NamedNode::new_unchecked(format!("http://example.com/s{i}")),
        NamedNode::new_unchecked("http://www.w3.org/2000/01/rdf-schema#label"),
        Literal::new_simple_literal(format!("word{} item{i}", i % 17)),
        match i % 3 {
            0 => GraphName::DefaultGraph,
            g => NamedNode::new_unchecked(format!("http://example.com/g{g}")).into(),
        },
    )
}

fn add(engine: &Engine, range: std::ops::Range<u64>) {
    let mut tx = engine.transaction();
    for i in range {
        tx.insert(label(i).as_ref());
    }
    tx.commit().unwrap();
}

/// The bytes building the corpus of a search of `snapshot` held beyond what it charged to
/// `envelope`, of which the search's own budget and the corpora's are parts (the corpus,
/// not the scoring). The index is extended first: it is the dictionary's.
fn excess(snapshot: &Snapshot, envelope: &Envelope) -> usize {
    let (model, every) = (ReadModel::Materialised, TextScope::EVERY);
    snapshot
        .text_corpus_size(ReadModel::Asserted, &every, false, None)
        .unwrap();
    let request = Arc::new(envelope.part());
    let (size, excess) = measure_within(envelope, || {
        snapshot
            .text_corpus_size(model, &every, false, Some(&request))
            .unwrap()
    });
    assert!(size.0 > 0);
    excess
}

#[test]
fn corpora_are_charged_before_they_are_allocated() {
    if !alone() {
        return;
    }
    let envelope = Envelope::unlimited();
    let engine = Engine::new(EngineConfig::default()).unwrap();
    engine.set_text_corpus_budget(envelope.part());
    add(&engine, 0..20_000);
    // A walk, kept on the corpora's budget.
    assert_eq!(excess(&engine.snapshot(), &envelope), 0, "walk");
    // After a commit: derived from the kept one.
    add(&engine, 20_000..20_100);
    let derived = engine.stats().text_corpora.derived;
    assert_eq!(excess(&engine.snapshot(), &envelope), 0, "derived");
    // The measured one and the index's warm-up (another model's) both derived.
    assert_eq!(engine.stats().text_corpora.derived, derived + 2);
    // A corpus the store can't keep: the search's alone.
    let engine = Engine::new(EngineConfig::default()).unwrap();
    engine.set_text_corpus_budget(Budget::new(8));
    add(&engine, 0..20_000);
    assert_eq!(excess(&engine.snapshot(), &envelope), 0, "not kept");
    assert_eq!(engine.stats().text_corpora.kept, 0);
}

/// A walk reserves what its slices can hold, from the bits it walks: one one-word document
/// on two workers fits a 64 KiB budget (the review's case of 19 October: it asked 4 MiB,
/// full slices on every worker).
#[test]
fn a_small_walk_reserves_for_its_own_slices() {
    let engine = Engine::new(EngineConfig::default()).unwrap();
    let mut tx = engine.transaction();
    tx.insert(
        Quad::new(
            NamedNode::new_unchecked("http://example.com/s"),
            NamedNode::new_unchecked("http://example.com/p"),
            Literal::new_simple_literal("word"),
            GraphName::DefaultGraph,
        )
        .as_ref(),
    );
    tx.commit().unwrap();
    let snapshot = engine.snapshot();
    let request = Arc::new(Budget::new(64 << 10));
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(2)
        .build()
        .unwrap();
    let size = pool.install(|| {
        snapshot.text_corpus_size(
            ReadModel::Materialised,
            &TextScope::EVERY,
            false,
            Some(&request),
        )
    });
    assert_eq!(size.map(|(documents, _)| documents), Ok(1));
}

/// Finding a kept corpus copies nothing uncharged: the scope is compared as the caller
/// wrote it, copied (charged) only where it isn't sorted, and the key is cloned only for a
/// new entry (the review's case of 19 October: 131,072 bytes for an 8,192-graph scope).
#[test]
fn a_kept_corpus_is_found_without_uncharged_copies() {
    if !alone() {
        return;
    }
    let envelope = Envelope::unlimited();
    let engine = Engine::new(EngineConfig::default()).unwrap();
    engine.set_text_corpus_budget(envelope.part());
    let mut tx = engine.transaction();
    for g in 0..8192 {
        tx.insert(
            Quad::new(
                NamedNode::new_unchecked("http://example.com/s"),
                NamedNode::new_unchecked("http://example.com/p"),
                Literal::new_simple_literal(format!("word{g}")),
                NamedNode::new_unchecked(format!("http://example.com/g{g}")),
            )
            .as_ref(),
        );
    }
    tx.commit().unwrap();
    let snapshot = engine.snapshot();
    let mut graphs: Vec<_> = (0..8192)
        .map(|g| {
            let graph = NamedNode::new_unchecked(format!("http://example.com/g{g}"));
            snapshot.lookup(graph.as_ref().into()).unwrap()
        })
        .collect();
    graphs.sort_unstable();
    let sorted = TextScope::Merge(graphs.clone());
    graphs.reverse();
    let unsorted = TextScope::Merge(graphs);
    let model = ReadModel::Materialised;
    snapshot
        .text_corpus_size(model, &sorted, false, None)
        .unwrap();
    for scope in [&sorted, &unsorted] {
        let request = Arc::new(envelope.part());
        let (size, excess) = measure_within(&envelope, || {
            snapshot
                .text_corpus_size(model, scope, false, Some(&request))
                .unwrap()
        });
        assert_eq!(size.0, 8192);
        assert_eq!(excess, 0);
    }
}

/// Scoring holds nothing uncharged: postings (of a word, a prefix's words, a fuzzy word's),
/// a phrase's checks, the exclusions, the merge, the matches (held, then dropped) are all
/// charged to the search's budget first (the review's case of 19 October: 745,637 bytes for
/// a warm search of 8,192 documents, and nothing charged).
#[test]
fn scoring_is_charged_before_it_is_allocated() {
    if !alone() {
        return;
    }
    let envelope = Envelope::unlimited();
    let engine = Engine::new(EngineConfig::default()).unwrap();
    engine.set_text_corpus_budget(envelope.part());
    let mut tx = engine.transaction();
    for i in 0..8192 {
        tx.insert(
            Quad::new(
                NamedNode::new_unchecked(format!("http://example.com/s{i}")),
                NamedNode::new_unchecked("http://example.com/p"),
                Literal::new_simple_literal(format!("Alpha beta item{i}")),
                GraphName::DefaultGraph,
            )
            .as_ref(),
        );
    }
    tx.commit().unwrap();
    let snapshot = engine.snapshot();
    let query = nrese_engine::TextQuery {
        text: "\"alpha beta\" alph* betta~1 -item7".to_owned(),
        all_words: true,
        prefix: false,
        stem: None,
    };
    let (model, every) = (ReadModel::Materialised, TextScope::EVERY);
    snapshot
        .text_search_in(model, &every, &query, None)
        .unwrap();
    let request = Arc::new(envelope.part());
    let (found, excess) = measure_within(&envelope, || {
        let matches = snapshot
            .text_search_in(model, &every, &query, Some(&request))
            .unwrap();
        matches.len()
    });
    assert_eq!(found, 8191);
    assert_eq!(excess, 0);
}
