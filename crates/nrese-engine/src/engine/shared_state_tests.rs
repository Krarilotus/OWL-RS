//! Package 3.0's guards for the shared-state capability
//! (`notes/2026-11-08-dict3-0-shared-state-design.md` §3; the design check `review-dict3-token-r0`
//! §5 items 1, 2, 5, 6, 8 and 10 in 3.0's share). The oracle is the capability itself:
//! `Snapshot::shared_state`, which every cache owner's admission will need. Its absence is what
//! keeps an unpublished snapshot's state out of shared caches.

use std::sync::Arc;

use nrese_exec::Budget;
use nrese_rdf::{GraphName, Literal, NamedNode, Quad, Term};

use super::{Engine, EngineConfig, InferredMask, InferredSubset, Snapshot};
use crate::{EncodedQuad, EncodedTriple};

fn iri(local: &str) -> NamedNode {
    NamedNode::new_unchecked(format!("http://example.com/{local}"))
}

fn literal(text: &str) -> Term {
    Literal::new_simple_literal(text).into()
}

fn quad(subject: &str, object: &str) -> Quad {
    Quad::new(
        iri(subject),
        iri("p"),
        literal(object),
        GraphName::DefaultGraph,
    )
}

fn config() -> EngineConfig {
    EngineConfig {
        background_maintenance: false,
        ..EngineConfig::default()
    }
}

/// An engine asserting `ex:a ex:p "one"` and `ex:b ex:p "two"`, inferring `ex:c ex:p "three"`
/// and `ex:c ex:p "four"`.
fn engine() -> Engine {
    let engine = Engine::new(config()).unwrap();
    let mut tx = engine.transaction();
    assert!(tx.insert(quad("a", "one").as_ref()));
    assert!(tx.insert(quad("b", "two").as_ref()));
    let c = tx.intern(iri("c").as_ref().into());
    let p = tx.intern(iri("p").as_ref().into());
    for object in ["three", "four"] {
        let object = tx.intern(literal(object).as_ref());
        assert!(tx.insert_inferred(EncodedTriple::new(c, p, object)));
    }
    tx.commit().unwrap();
    engine
}

fn budget() -> Arc<Budget> {
    Arc::new(Budget::unlimited())
}

/// The inferred statements of `snapshot`, as quads.
fn inferred(snapshot: &Snapshot) -> Vec<EncodedQuad> {
    snapshot
        .quads_for_pattern_in(super::ReadModel::Inferred, &crate::QuadPattern::all())
        .collect()
}

fn token(snapshot: &Snapshot) -> bool {
    snapshot.shared_state().is_some()
}

/// Review §5 item 1, 3.0's share: pending state of both kinds grants no token, with no private
/// id involved.
///
/// - Input: a published snapshot; writer pending snapshots (`pending_snapshot` and `pending_over`)
///   after inserting and deleting statements made only of existing ids, with no change, and after
///   inserting then deleting the same statement; `Engine::speculative()`'s; a private
///   transaction's, from both builders.
/// - Expected: the published one grants a token whose snapshot is itself; none of the others
///   grants one.
#[test]
fn pending_states_of_both_kinds_grant_no_token() {
    let engine = engine();
    let published = engine.snapshot();
    let state = published.shared_state().expect("a published snapshot");
    assert!(std::ptr::eq(state.snapshot(), &published));

    let existing: Vec<EncodedQuad> = published
        .quads_for_pattern_in(super::ReadModel::Asserted, &crate::QuadPattern::all())
        .collect();
    let a = existing[0];
    let moved = EncodedQuad::new(a.subject, a.predicate, existing[1].object, a.graph);
    let view = published.with_inferred_subset(InferredSubset::Without(Vec::new()));

    let mut changed = engine.transaction();
    assert!(changed.insert_encoded(moved), "existing ids only");
    assert!(changed.remove_encoded(existing[1]));
    assert!(!token(&changed.pending_snapshot()), "writer, changed");
    assert!(
        !token(&changed.pending_over(&view, &[])),
        "writer, over a view"
    );
    drop(changed);

    let unchanged = engine.transaction();
    assert!(!token(&unchanged.pending_snapshot()), "writer, no change");
    assert!(
        !token(&unchanged.pending_over(&view, &[])),
        "writer, no change, over"
    );
    drop(unchanged);

    let mut undone = engine.transaction();
    assert!(undone.insert_encoded(moved));
    assert!(undone.remove_encoded(moved));
    assert!(
        !token(&undone.pending_snapshot()),
        "writer, INSERT then DELETE"
    );
    drop(undone);

    assert!(
        !token(&engine.speculative().pending_snapshot()),
        "speculative"
    );

    let mut private = engine.speculative_over(&published, budget()).unwrap();
    assert!(private.try_insert(quad("d", "five").as_ref()).unwrap());
    assert!(!token(&private.pending_snapshot().unwrap()), "private");
    assert!(
        !token(&private.pending_over(&view, &[]).unwrap()),
        "private, over"
    );
}

/// `source` with `added` as statements its DL bounds derived (3.0b).
fn derived(source: &Snapshot, added: EncodedQuad) -> Snapshot {
    let triple = EncodedTriple::new(added.subject, added.predicate, added.object);
    source
        .with_inferred_derived_within(&[triple], &budget())
        .unwrap()
}

/// Review §5 item 2 and F2: views keep or clear eligibility by the design note's matrix, and
/// never launder an ineligible snapshot.
///
/// - Input: the published snapshot and a writer's pending one, each through every derived
///   view: inferred subsets (`Without`, `Only`), `with_inferred_only`, a mask, equality, stored,
///   canonical, a reader's budget, the two additive views, and a chain of them.
/// - Expected: from the published snapshot, the restrictions and reinterpretations keep a token,
///   and both additive views have none. From the pending snapshot, none has one.
#[test]
fn views_keep_or_clear_eligibility_by_the_matrix() {
    let engine = engine();
    let published = engine.snapshot();
    let mut tx = engine.transaction();
    assert!(tx.insert(quad("e", "six").as_ref()));
    let pending = tx.pending_snapshot();
    // Any predicate will do for equality's reinterpretation: only eligibility is read here.
    let predicate = published.lookup(iri("p").as_ref().into());
    let added = EncodedQuad::new(
        published.lookup(iri("a").as_ref().into()).unwrap(),
        predicate.unwrap(),
        published.lookup(literal("two").as_ref()).unwrap(),
        crate::TermId::DEFAULT_GRAPH,
    );

    for (source, eligible) in [(&published, true), (&pending, false)] {
        let all = inferred(source);
        let hidden = &all[..1];
        let mask = InferredMask::hiding(source, hidden, &budget()).unwrap();
        let restrictions: Vec<(&str, Snapshot)> = vec![
            (
                "subset without",
                source.with_inferred_subset(InferredSubset::Without(hidden.to_vec())),
            ),
            (
                "subset only",
                source.with_inferred_subset(InferredSubset::Only(all[1..].to_vec())),
            ),
            (
                "inferred only",
                source.with_inferred_only(&all[1..], &budget()).unwrap(),
            ),
            (
                "mask",
                source
                    .with_inferred_mask(&mask, &budget())
                    .unwrap()
                    .unwrap(),
            ),
            // 3.0b: the DL bounds' derivation keeps its source's publication, the upper
            // bound's read with its equality too.
            ("derived within", derived(source, added)),
            (
                "derived, with equality",
                derived(source, added).with_equality(predicate),
            ),
            ("equality", source.with_equality(predicate)),
            ("stored", source.stored()),
            ("canonical", source.with_canonical_equality()),
            ("reading within", source.reading_within(budget()).unwrap()),
            (
                "a chain",
                source
                    .with_inferred_subset(InferredSubset::Without(hidden.to_vec()))
                    .stored()
                    .with_equality(predicate)
                    .reading_within(budget())
                    .unwrap(),
            ),
        ];
        for (name, view) in &restrictions {
            assert_eq!(token(view), eligible, "{name}, from published: {eligible}");
        }
        let additions: Vec<(&str, Snapshot)> = vec![
            ("added", source.with_inferred_added(&[added])),
            (
                "added within",
                source
                    .with_inferred_added_within(
                        &[EncodedTriple::new(
                            added.subject,
                            added.predicate,
                            added.object,
                        )],
                        &budget(),
                    )
                    .unwrap(),
            ),
            (
                "added, then restricted",
                source
                    .with_inferred_added(&[added])
                    .with_inferred_subset(InferredSubset::Without(Vec::new()))
                    .stored(),
            ),
        ];
        for (name, view) in &additions {
            assert!(!token(view), "{name}, from published: {eligible}");
        }
    }
}

/// Review §5 item 5 and F1: the staging constructor grants no token, the engine's acquisition
/// does.
///
/// - Input: `Snapshot::new` over the installed version (the bulk path's checkpoint image is made
///   so), and the engine's own `Shared::snapshot` of the same version.
/// - Expected: the first has no token, the second has one.
#[test]
fn only_the_engines_acquisition_grants_a_token() {
    let engine = engine();
    let shared = &engine.inner.shared;
    let staged = Snapshot::new(
        shared.versions.load(),
        Arc::clone(&shared.dictionary),
        Arc::clone(&shared.statistics),
        None,
        Arc::clone(&shared.views.read()),
    );
    assert!(!token(&staged), "the staging constructor");
    assert!(token(&shared.snapshot()), "the engine's acquisition");
}

/// Review §5 item 6: a pending clone never gains a token, whatever becomes of its transaction.
///
/// - Input: pending snapshots kept across a successful commit, an empty-delta commit, a dropped
///   transaction, and a speculative transaction's refused commit.
/// - Expected: each kept clone has no token; a snapshot taken from the engine after the
///   successful commit has one.
#[test]
fn a_pending_clone_never_gains_a_token() {
    let engine = engine();

    let mut committed = engine.transaction();
    assert!(committed.insert(quad("f", "seven").as_ref()));
    let kept = committed.pending_snapshot();
    committed.commit().unwrap();
    assert!(!token(&kept), "after its commit published");
    let after = engine.snapshot();
    assert!(token(&after), "a fresh snapshot after the commit");
    assert!(after.lookup(literal("seven").as_ref()).is_some());

    let empty = engine.transaction();
    let kept = empty.pending_snapshot();
    empty.commit().unwrap();
    assert!(!token(&kept), "after an empty-delta commit");

    let mut dropped = engine.transaction();
    assert!(dropped.insert(quad("g", "eight").as_ref()));
    let kept = dropped.pending_snapshot();
    drop(dropped);
    assert!(!token(&kept), "after its transaction dropped");

    let mut refused = engine.speculative();
    assert!(refused.insert(quad("h", "nine").as_ref()));
    let kept = refused.pending_snapshot();
    assert!(
        refused.commit().is_err(),
        "a speculative transaction never commits"
    );
    assert!(!token(&kept), "after a refused commit");
}

/// Review §5 item 8, 3.0's share: eligibility comes with the version acquired, across a reopen
/// and between engines.
///
/// - Input: a durable engine's snapshot kept while the engine closes and the store reopens;
///   the reopened engine's snapshot; a second, independent engine's.
/// - Expected: the kept snapshot keeps its token and decodes its own term; the reopened and the
///   independent engines' snapshots each grant their own.
#[test]
fn eligibility_comes_with_the_version_acquired() {
    let dir = tempfile::tempdir().unwrap();
    let engine = Engine::open(dir.path(), config()).unwrap();
    let mut tx = engine.transaction();
    assert!(tx.insert(quad("a", "kept").as_ref()));
    tx.commit().unwrap();
    let kept = engine.snapshot();
    let id = kept.lookup(literal("kept").as_ref()).unwrap();
    drop(engine);

    let reopened = Engine::open(dir.path(), config()).unwrap();
    assert!(token(&kept), "kept across the reopen");
    assert_eq!(kept.decode(id), Some(literal("kept")), "with its own terms");
    assert!(token(&reopened.snapshot()), "the reopened engine's");
    let independent = Engine::new(config()).unwrap();
    assert!(token(&independent.snapshot()), "another engine's");
}

/// The master's 21:01 fix: an `Only` view keeps its source's publication only for statements
/// among the source's inferred ones, which debug builds check (the design note's §3).
///
/// - Input: the published snapshot, kept-only views of one of its inferred statements, and of
///   a statement it doesn't infer (an asserted one, then one it doesn't hold at all), through
///   both `InferredSubset::Only` and `with_inferred_only`.
/// - Expected: the real subset keeps the token; each list naming a statement the snapshot
///   doesn't infer has none. In every build (sol-loop r1 S1: the store's support views merge
///   such statements into their lists).
#[test]
fn a_kept_list_beyond_the_inferred_statements_loses_eligibility() {
    let engine = engine();
    let published = engine.snapshot();
    let real = inferred(&published)[..1].to_vec();
    let asserted: Vec<EncodedQuad> = published
        .quads_for_pattern_in(super::ReadModel::Asserted, &crate::QuadPattern::all())
        .take(1)
        .collect();
    let nowhere = vec![EncodedQuad::new(
        real[0].object,
        real[0].predicate,
        real[0].subject,
        real[0].graph,
    )];
    assert!(
        !published.contains(&nowhere[0]),
        "the fixture's statement is in nothing"
    );
    for (kept, eligible) in [(&real, true), (&asserted, false), (&nowhere, false)] {
        let subset = published.with_inferred_subset(InferredSubset::Only(kept.clone()));
        assert_eq!(token(&subset), eligible, "Only({kept:?})");
        let only = published.with_inferred_only(kept, &budget()).unwrap();
        assert_eq!(token(&only), eligible, "with_inferred_only({kept:?})");
    }
}

/// Sol-loop r1 S1 (8 Nov), the master's ruling (F2): a mask that shows a statement its stack
/// doesn't hold adds it, so a view through it is unpublished; one that only hides, or shows
/// again what it hid, is a restriction. An addition stays one through later changes.
///
/// - Input: the published snapshot's masks: hiding one inferred statement; that, then showing
///   it again; showing a statement the stack doesn't infer (an asserted one, as the store's
///   support views show facts asserted out of a reader's reach); that one changed again by
///   hiding another statement.
/// - Expected: views through the first two keep the token; through the last two, none.
#[test]
fn a_mask_that_shows_an_extra_statement_loses_eligibility() {
    let engine = engine();
    let published = engine.snapshot();
    let all = inferred(&published);
    let extra: Vec<EncodedQuad> = published
        .quads_for_pattern_in(super::ReadModel::Asserted, &crate::QuadPattern::all())
        .take(1)
        .collect();
    let view = |mask: &InferredMask| {
        published
            .with_inferred_mask(mask, &budget())
            .unwrap()
            .expect("the stack's layout")
    };
    let hiding = InferredMask::hiding(&published, &all[..1], &budget()).unwrap();
    assert!(token(&view(&hiding)), "hiding only");
    let shown_again = hiding
        .changed(&published, &[], &all[..1], &budget())
        .unwrap();
    assert!(token(&view(&shown_again)), "showing again what it hid");
    let adding = InferredMask::default()
        .changed(&published, &[], &extra, &budget())
        .unwrap();
    let through = view(&adding);
    assert!(
        through.contains_in(super::ReadModel::Inferred, &extra[0]),
        "the mask shows the extra statement as inferred"
    );
    assert!(
        !token(&through),
        "showing a statement the stack doesn't hold"
    );
    let still = adding
        .changed(&published, &all[1..2], &[], &budget())
        .unwrap();
    assert!(!token(&view(&still)), "an addition stays one");
}
