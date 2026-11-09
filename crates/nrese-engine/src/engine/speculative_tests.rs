//! Dictionary lifecycle V1's engine guards (`notes/2026-11-09-dict-v1-speculative-private-design.md`
//! §4): item 8, a private transaction's memoised pending snapshot, and the system-vocabulary
//! append (§1.1). Each states its input and what the design says it must give.

use std::sync::Arc;

use nrese_exec::Budget;
use nrese_rdf::{GraphName, Literal, NamedNode, NamedNodeRef, Quad, Term, TermRef};

use super::{Admission, Engine, EngineConfig, OwnState, PendingView, PendingWrite, Snapshot};
use crate::ReadModel;

fn iri(local: &str) -> NamedNode {
    NamedNode::new_unchecked(format!("http://example.com/{local}"))
}

fn quad(subject: &str, object: &str) -> Quad {
    let object: Term = Literal::new_simple_literal(object).into();
    Quad::new(iri(subject), iri("p"), object, GraphName::DefaultGraph)
}

/// An engine asserting `ex:a ex:p "one"`.
fn engine() -> Engine {
    let engine = Engine::new(EngineConfig {
        background_maintenance: false,
        ..EngineConfig::default()
    })
    .unwrap();
    let mut tx = engine.transaction();
    tx.insert(quad("a", "one").as_ref());
    tx.commit().unwrap();
    engine
}

fn own(snapshot: &Snapshot) -> OwnState<'_> {
    match snapshot.admission() {
        Admission::Own(own) => own,
        Admission::Shared(_) => panic!("a private snapshot admits into its own state"),
    }
}

/// What a probe stored in an own-state slot.
struct Probe(u32);

/// Item 8 (§4), the memo (§1.1).
///
/// - Input: a private transaction with one asserted statement; its pending snapshot asked for
///   twice with one budget; then after a write that changes nothing, a write that changes the
///   pending changes, a new term, and with another budget.
/// - Expected: the same state and budget give the same snapshot (one identity), so a value kept
///   in its own-state slot is found again rather than computed anew. A write that changes
///   nothing keeps it. A changing write, a new term (not in the earlier snapshot's frozen
///   vocabulary) and another budget each give a new snapshot, with the state as it is then.
#[test]
fn a_private_pending_snapshot_is_reused_while_its_state_and_budget_are() {
    let engine = engine();
    let base = engine.snapshot();
    let budget = Arc::new(Budget::unlimited());
    let mut tx = engine.speculative_over(&base, Arc::clone(&budget)).unwrap();
    assert!(tx.try_insert(quad("b", "two").as_ref()).unwrap());

    let first = tx.pending_snapshot_within(&budget).unwrap();
    own(&first).get_or_insert_with(|| Probe(1));
    let again = tx.pending_snapshot_within(&budget).unwrap();
    assert_eq!(
        again.identity(),
        first.identity(),
        "the same state and budget"
    );
    assert_eq!(
        own(&again).get::<Probe>().map(|probe| probe.0),
        Some(1),
        "the slot's value is reused"
    );

    assert!(!tx.try_insert(quad("b", "two").as_ref()).unwrap());
    let unchanged = PendingView::pending_snapshot_within(&tx, &budget).unwrap();
    assert_eq!(
        unchanged.identity(),
        first.identity(),
        "a write that changed nothing, through the trait"
    );

    assert!(PendingWrite::remove(&mut tx, quad("b", "two").as_ref()).unwrap());
    let changed = tx.pending_snapshot_within(&budget).unwrap();
    assert_ne!(changed.identity(), first.identity(), "after a change");
    assert!(own(&changed).get::<Probe>().is_none(), "a new slot");
    assert_eq!(changed.len_in(ReadModel::Asserted), 1, "the state now");

    let term: Term = Literal::new_simple_literal("new").into();
    let id = tx.try_intern(term.as_ref()).unwrap();
    let interned = tx.pending_snapshot_within(&budget).unwrap();
    assert_ne!(interned.identity(), changed.identity(), "after a new term");
    assert_eq!(
        interned.decode(id),
        Some(term),
        "its vocabulary has the term"
    );

    let other = Arc::new(Budget::unlimited());
    let elsewhere = tx.pending_snapshot_within(&other).unwrap();
    assert_ne!(elsewhere.identity(), interned.identity(), "another budget");
    let back = tx.pending_snapshot_within(&other).unwrap();
    assert_eq!(
        back.identity(),
        elsewhere.identity(),
        "kept for that budget"
    );
}

/// Item 6 (§4), the bounded-prefix precondition (`PrivateTransaction::extends`).
///
/// - Input: a published snapshot; then a system-vocabulary append (no commit); a private
///   transaction over the latest snapshot; then a commit of a new term; another engine's
///   snapshot; the transaction's own pending snapshot.
/// - Expected: the transaction extends the earlier snapshot and the latest at its opening;
///   the appended name keeps its shared id in it (its vocabulary starts past the append, not
///   at the last commit's length); it doesn't extend the snapshot committed after it opened,
///   another engine's, or a private one.
#[test]
#[allow(clippy::disallowed_methods)]
fn a_private_transaction_extends_only_its_engines_earlier_snapshots() {
    let engine = engine();
    let earlier = engine.snapshot();
    let name: TermRef<'_> = NamedNodeRef::new_unchecked("http://example.com/v1/x").into();
    let appended = engine.intern_system_vocabulary([name]).unwrap()[0];
    let opened = engine.snapshot();
    let tx = engine.speculative(Arc::new(Budget::unlimited()));
    assert!(
        tx.extends(&earlier) && tx.extends(&opened),
        "its engine's, earlier"
    );
    assert_eq!(
        tx.lookup(name),
        Some(appended),
        "the appended name's shared id"
    );
    let own = tx
        .try_intern(NamedNodeRef::new_unchecked("http://example.com/v1/own").into())
        .unwrap();
    assert_ne!(own, appended, "no own id equals an appended name's");

    let mut writer = engine.transaction();
    writer.insert(quad("later", "committed after").as_ref());
    writer.commit().unwrap();
    assert!(!tx.extends(&engine.snapshot()), "committed after it opened");
    assert!(!tx.extends(&self::engine().snapshot()), "another engine's");
    assert!(
        !tx.extends(&tx.pending_snapshot().unwrap()),
        "a private one"
    );
}

/// Item 7 (§4), at the engine: a refusal.
///
/// - Input: a private transaction whose budget has no room; a term and a statement of its own.
/// - Expected: both refused as `MemoryLimit`, through the inherent methods and the trait
///   alike; nothing pending, and the dictionary unchanged (no fallback to it).
#[test]
fn a_refused_private_write_falls_back_to_nothing() {
    let engine = engine();
    let before = terms(&engine);
    let mut tx = engine.speculative(Arc::new(Budget::new(0)));
    let term: Term = Literal::new_simple_literal("refused").into();
    assert!(matches!(
        tx.try_intern(term.as_ref()),
        Err(crate::EngineError::MemoryLimit(_))
    ));
    assert!(matches!(
        PendingWrite::try_insert(&mut tx, quad("r", "refused").as_ref()),
        Err(crate::EngineError::MemoryLimit(_))
    ));
    assert_eq!(PendingView::pending(&tx), (0, 0), "nothing pending");
    drop(tx);
    assert_eq!(terms(&engine), before, "no shared term");
}

/// §1.1, the system-vocabulary append.
///
/// - Input: an engine; two names appended as system vocabulary, one of them twice; then a
///   private transaction opened over the latest snapshot, looking them up.
/// - Expected: the dictionary grows by the two new names only, though no commit followed (a
///   snapshot's own length is its last commit's); a name appended twice keeps its id; a later
///   snapshot decodes both; and a private transaction opened afterwards finds them as shared
///   ids, without adding any of its own.
#[test]
#[allow(clippy::disallowed_methods)]
fn system_vocabulary_is_appended_to_the_shared_dictionary() {
    let engine = engine();
    let before = terms(&engine);
    let names = ["http://example.com/v1/one", "http://example.com/v1/two"];
    let refs: [TermRef<'_>; 2] = names.map(|name| NamedNodeRef::new_unchecked(name).into());
    let ids = engine
        .intern_system_vocabulary(refs.iter().copied())
        .unwrap();
    let again = engine.intern_system_vocabulary([refs[0]]).unwrap();
    assert_eq!(again, vec![ids[0]], "one id per name");
    assert_eq!(terms(&engine), before + 2, "the two names only");
    let after = engine.snapshot();
    for (&id, &term) in ids.iter().zip(&refs) {
        assert_eq!(after.decode(id), Some(term.into_owned()));
    }

    let tx = engine.speculative(Arc::new(Budget::unlimited()));
    for (&id, &term) in ids.iter().zip(&refs) {
        assert_eq!(
            tx.lookup(term),
            Some(id),
            "a shared id in the private vocabulary"
        );
    }
    assert_eq!(terms(&engine), before + 2, "the transaction added none");
}

/// The dictionary's live length: what a no-slot append changes before any commit.
fn terms(engine: &Engine) -> u64 {
    engine.stats().dictionary.terms
}

/// `PendingView::quads_for_pattern_in`'s scan returned through a signature that captures the
/// transaction's borrow only, as `ReadView::quads_for_pattern_in` does (V2's generic `ReadView`
/// delegates to it). Doesn't compile (E0700) if the trait's iterator captures the pattern's
/// borrow too.
fn scan_of<'a, T: PendingView>(
    tx: &'a T,
    pattern: &crate::QuadPattern,
) -> impl Iterator<Item = crate::EncodedQuad> + use<'a, T> {
    tx.quads_for_pattern_in(ReadModel::Asserted, pattern)
}

/// [`scan_of`] read after its pattern is dropped.
fn asserted_after_the_pattern<T: PendingView>(tx: &T) -> Vec<crate::EncodedQuad> {
    let scan = {
        let pattern = crate::QuadPattern::all();
        scan_of(tx, &pattern)
    };
    scan.collect()
}

/// The trait's scan, a pending change included, through both transaction kinds.
///
/// - Input: the engine of `ex:a ex:p "one"`; a write transaction and a private one, each
///   asserting `ex:b ex:p "two"`; each scanned through the trait after its pattern is dropped.
/// - Expected: both statements from each, decoded through its own transaction.
#[test]
fn a_pending_scan_outlives_its_pattern() {
    let engine = engine();
    let expected = {
        let mut both = vec![quad("a", "one"), quad("b", "two")];
        both.sort_by_key(ToString::to_string);
        both
    };
    let decoded = |quads: Vec<crate::EncodedQuad>,
                   decode: &dyn Fn(crate::EncodedQuad) -> Option<Quad>| {
        let mut out: Vec<Quad> = quads.into_iter().map(|q| decode(q).unwrap()).collect();
        out.sort_by_key(ToString::to_string);
        out
    };
    let mut private = engine.speculative(Arc::new(Budget::unlimited()));
    assert!(private.try_insert(quad("b", "two").as_ref()).unwrap());
    let scanned = asserted_after_the_pattern(&private);
    assert_eq!(
        decoded(scanned, &|q| private.decode_quad(q)),
        expected,
        "private"
    );
    drop(private);
    let mut writer = engine.transaction();
    assert!(writer.insert(quad("b", "two").as_ref()));
    let scanned = asserted_after_the_pattern(&writer);
    assert_eq!(
        decoded(scanned, &|q| writer.decode_quad(q)),
        expected,
        "write"
    );
}
