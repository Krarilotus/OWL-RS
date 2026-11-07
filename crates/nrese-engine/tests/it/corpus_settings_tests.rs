//! How reader-scoped searches build their corpora is set, not fixed (lead card §5a; master
//! 7 Nov 17:28 and 17:34): `CorpusTuning::documents_per_probe` decides between deriving a
//! corpus after commits and walking it again, `CorpusTuning::walk_slice_words` how a walk is
//! split. Neither changes an answer: oracle, a fresh engine with the same statements and the
//! defaults.
//!
//! These settings did not exist before this change, so their tests were written with it
//! (no red run on the parent; spec-tests: a new setting's test).

use nrese_engine::{CorpusTuning, Engine, EngineConfig};

use crate::text_scope_tests::{built, every, label, texts};

/// 2,000 labels, a kept corpus of them, then a commit of 600 labels (1,200 touched terms).
fn engine_with(tuning: CorpusTuning) -> (Engine, nrese_engine::Snapshot) {
    let engine = Engine::new(EngineConfig::default()).unwrap();
    engine.set_corpus_tuning(tuning);
    let mut tx = engine.transaction();
    for i in 0..2000 {
        tx.insert(label(&format!("s{i}"), &format!("alpha item{i}"), None).as_ref());
    }
    tx.commit().unwrap();
    every(&engine.snapshot(), "alpha");
    let mut tx = engine.transaction();
    for i in 0..600 {
        tx.insert(label(&format!("t{i}"), &format!("beta item{i}"), None).as_ref());
    }
    tx.commit().unwrap();
    let snapshot = engine.snapshot();
    (engine, snapshot)
}

/// A corpus of 2,000 documents after 1,200 touched terms: by default (10 documents a probe,
/// up to 200 terms) walked again; at one document a probe (up to 2,000 terms) derived.
/// Both answer alike.
#[test]
fn documents_per_probe_chooses_between_deriving_and_walking() {
    let (_default, walked) = engine_with(CorpusTuning::DEFAULT);
    let found = every(&walked, "alpha beta");
    assert_eq!(built(&walked), (2, 0), "{:?}", walked.text_corpus_stats());
    let (_probing, derived) = engine_with(CorpusTuning {
        documents_per_probe: 1,
        ..CorpusTuning::DEFAULT
    });
    let probed = every(&derived, "alpha beta");
    assert_eq!(built(&derived), (1, 1), "{:?}", derived.text_corpus_stats());
    assert_eq!(texts(&walked, &found), texts(&derived, &probed));
    assert_eq!(found.len(), 2600);
}

/// A walk in slices of one word (64 terms) finds what a walk in the default slices finds.
#[test]
fn the_walk_slice_changes_no_answer() {
    let (_default, wide) = engine_with(CorpusTuning::DEFAULT);
    let (_narrow, narrow) = engine_with(CorpusTuning {
        walk_slice_words: 1,
        ..CorpusTuning::DEFAULT
    });
    assert_eq!(
        texts(&wide, &every(&wide, "alpha beta")),
        texts(&narrow, &every(&narrow, "alpha beta"))
    );
}
