//! Package 3.0c's guards for the own-state slots of unpublished versions
//! (`notes/2026-11-09-dict3-0c-own-state-design.md` §3, items 2 to 8; item 1 runs over every
//! row of 3.0's matrix in `shared_state_tests`). The API is new, so the parent can't fail
//! them: their red is the mutants the note names.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Barrier, mpsc};
use std::time::Duration;

use nrese_exec::{Budget, Reservation};
use nrese_rdf::{GraphName, Literal, NamedNode, Quad, Term};

use super::{Admission, BulkMode, Engine, EngineConfig, InferredSubset, OwnState, Snapshot};
use crate::durability::fault::IMAGE_READER;
use crate::{EncodedQuad, QuadPattern, ReadModel};

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

/// An engine asserting `ex:a ex:p "one"`.
fn engine() -> Engine {
    let engine = Engine::new(config()).unwrap();
    let mut tx = engine.transaction();
    assert!(tx.insert(quad("a", "one").as_ref()));
    tx.commit().unwrap();
    engine
}

/// The own-state slots of an unpublished snapshot.
fn own(snapshot: &Snapshot) -> OwnState<'_> {
    match snapshot.admission() {
        Admission::Own(own) => own,
        Admission::Shared(_) => panic!("an unpublished snapshot"),
    }
}

/// What each probe holds on its budget.
const HELD: usize = 1024;

/// A family's value: it counts its drops and holds a charge until it is dropped.
struct Probe {
    drops: Arc<AtomicUsize>,
    _held: Reservation,
}

impl Drop for Probe {
    fn drop(&mut self) {
        self.drops.fetch_add(1, Ordering::SeqCst);
    }
}

/// The probes one test makes: their drops and their charges.
struct Tally {
    drops: Arc<AtomicUsize>,
    budget: Arc<Budget>,
}

impl Tally {
    fn new() -> Self {
        Self {
            drops: Arc::default(),
            budget: Arc::new(Budget::unlimited()),
        }
    }

    fn probe(&self) -> Probe {
        let mut held = Reservation::new(Arc::clone(&self.budget));
        held.charge(HELD).unwrap();
        Probe {
            drops: Arc::clone(&self.drops),
            _held: held,
        }
    }

    fn drops(&self) -> usize {
        self.drops.load(Ordering::SeqCst)
    }

    fn charged(&self) -> usize {
        self.budget.used()
    }
}

/// Item 2: a race keeps the first value stored.
///
/// - Input: two readers of one pending version, each making a value, both held at a barrier
///   inside `make` until both have made theirs (both missed).
/// - Expected: both get the same `Arc`, the one the slot holds; the other value is dropped and
///   its charge released at once; the kept one's when the last holder goes.
#[test]
fn a_race_keeps_the_first_value_stored() {
    let engine = engine();
    let mut tx = engine.transaction();
    assert!(tx.insert(quad("b", "two").as_ref()));
    let pending = tx.pending_snapshot();
    let tally = Tally::new();
    let both_made = Barrier::new(2);
    let got: Vec<Arc<Probe>> = std::thread::scope(|scope| {
        let readers: Vec<_> = (0..2)
            .map(|_| {
                let reader = pending.clone();
                let (tally, both_made) = (&tally, &both_made);
                scope.spawn(move || {
                    own(&reader).get_or_insert_with(|| {
                        let made = tally.probe();
                        both_made.wait();
                        made
                    })
                })
            })
            .collect();
        readers.into_iter().map(|t| t.join().unwrap()).collect()
    });
    assert!(Arc::ptr_eq(&got[0], &got[1]), "one value for both");
    let stored = own(&pending).get::<Probe>().expect("a value stored");
    assert!(Arc::ptr_eq(&stored, &got[0]), "the one the slot holds");
    assert_eq!(tally.drops(), 1, "the other value dropped");
    assert_eq!(tally.charged(), HELD, "and its charge released");
    drop((got, stored, pending));
    assert_eq!(tally.drops(), 2, "the kept one, with the version");
    assert_eq!(tally.charged(), 0);
}

/// Item 3, this slice's share: the views that share a version share its value; a snapshot of
/// another version never sees it (the design note's §1 table).
///
/// - Input: a value made on a writer's pending snapshot; its equality, stored, canonical and
///   reading-within views and a clone; then a second `pending_snapshot`, a `pending_over`, the
///   charged `pending_snapshot_within` and `pending_over_within`, an inferred subset, an
///   addition and a private transaction's pending snapshot.
/// - Expected: the first group gets the same value; each of the second finds none and makes
///   its own.
#[test]
fn views_of_one_version_share_its_value_and_no_other_version_does() {
    let engine = engine();
    let published = engine.snapshot();
    let mut tx = engine.transaction();
    assert!(tx.insert(quad("b", "two").as_ref()));
    let pending = tx.pending_snapshot();
    let tally = Tally::new();
    let first = own(&pending).get_or_insert_with(|| tally.probe());
    let same_as = pending.lookup(iri("p").as_ref().into());
    let sharing = [
        ("equality", pending.with_equality(same_as)),
        ("stored", pending.stored()),
        ("canonical", pending.with_canonical_equality()),
        (
            "reading within",
            pending
                .reading_within(Arc::new(Budget::unlimited()))
                .unwrap(),
        ),
        ("a clone", pending.clone()),
    ];
    for (name, view) in &sharing {
        let value = own(view).get::<Probe>();
        assert!(
            value.is_some_and(|value| Arc::ptr_eq(&value, &first)),
            "{name}: its source's value"
        );
    }

    let asserted: EncodedQuad = published
        .quads_for_pattern_in(ReadModel::Asserted, &QuadPattern::all())
        .next()
        .unwrap();
    let mut private = engine
        .speculative_over(&published, Arc::new(Budget::unlimited()))
        .unwrap();
    assert!(private.try_insert(quad("c", "three").as_ref()).unwrap());
    let unlimited = Arc::new(Budget::unlimited());
    let others = [
        ("another pending snapshot", tx.pending_snapshot()),
        ("pending over", tx.pending_over(&published, &[])),
        // DUR-6a's charged builders make versions of their own too.
        (
            "pending snapshot within",
            tx.pending_snapshot_within(&unlimited).unwrap(),
        ),
        (
            "pending over within",
            tx.pending_over_within(&published, &[], &unlimited).unwrap(),
        ),
        (
            "a subset",
            pending.with_inferred_subset(InferredSubset::Without(Vec::new())),
        ),
        ("an addition", pending.with_inferred_added(&[asserted])),
        ("private", private.pending_snapshot().unwrap()),
    ];
    for (name, view) in &others {
        assert!(
            own(view).get::<Probe>().is_none(),
            "{name}: none of its own"
        );
        let made = own(view).get_or_insert_with(|| tally.probe());
        assert!(!Arc::ptr_eq(&made, &first), "{name}: a value of its own");
    }
}

/// Item 4: the charge is released by the version's last holder.
///
/// - Input: a value made on a pending snapshot, then a clone and a view sharing its version,
///   dropped one by one; a value on a pending snapshot whose transaction is rolled back
///   (dropped); a value on a `pending_over` view that outlives its transaction.
/// - Expected: the charge stays while any holder lives, and is zero once the last one goes.
#[test]
fn the_charge_is_released_by_the_versions_last_holder() {
    let engine = engine();
    let published = engine.snapshot();
    let tally = Tally::new();

    let mut tx = engine.transaction();
    assert!(tx.insert(quad("b", "two").as_ref()));
    let pending = tx.pending_snapshot();
    drop(own(&pending).get_or_insert_with(|| tally.probe()));
    let (clone, view) = (pending.clone(), pending.stored());
    drop(pending);
    assert_eq!(tally.charged(), HELD, "a clone holds it");
    drop(clone);
    assert_eq!(tally.charged(), HELD, "a view sharing the version holds it");
    drop(view);
    assert_eq!(tally.charged(), 0, "released by the last holder");
    drop(tx);

    let mut rolled_back = engine.transaction();
    assert!(rolled_back.insert(quad("c", "three").as_ref()));
    let pending = rolled_back.pending_snapshot();
    drop(own(&pending).get_or_insert_with(|| tally.probe()));
    drop(rolled_back);
    assert_eq!(
        tally.charged(),
        HELD,
        "rolled back, its pending snapshot kept"
    );
    drop(pending);
    assert_eq!(tally.charged(), 0, "rolled back, released");

    let mut tx = engine.transaction();
    assert!(tx.insert(quad("d", "four").as_ref()));
    let over = tx.pending_over(&published, &[]);
    drop(own(&over).get_or_insert_with(|| tally.probe()));
    drop(tx);
    assert_eq!(
        tally.charged(),
        HELD,
        "a pending_over view past its transaction"
    );
    drop(over);
    assert_eq!(tally.charged(), 0, "pending_over, released");
    assert_eq!(tally.drops(), 3);
}

/// Item 5: no version an unpublished snapshot held is installed. Installing takes a value
/// (`Versions::install`), so the compiler holds the rule; this guards what the bulk path
/// keeps of its checkpoint image.
///
/// - Input: a durable bulk load appending to a store that holds a statement (a version built,
///   so the checkpoint is written from an image: a replacement or a load into an empty store
///   with mapped checkpoints streams it instead), with mapped checkpoints and without; its
///   image is read by a reader that makes a value in its own slots.
/// - Expected: the image was read; by the time the load returns its value is dropped and its
///   charge released; the published snapshot is `Shared` and holds both statements.
#[test]
fn a_bulk_loads_image_keeps_nothing_past_the_load() {
    for map_checkpoints in [true, false] {
        let dir = tempfile::tempdir().unwrap();
        let mut config = config();
        config.durability.map_checkpoints = map_checkpoints;
        let engine = Engine::open(dir.path(), config).unwrap();
        let mut tx = engine.transaction();
        assert!(tx.insert(quad("a", "one").as_ref()));
        tx.commit().unwrap();
        let tally = Arc::new(Tally::new());
        let reads = Arc::new(AtomicUsize::new(0));
        let (reader_tally, reader_reads) = (Arc::clone(&tally), Arc::clone(&reads));
        IMAGE_READER.set(Some(Box::new(move |image: &Snapshot| {
            reader_reads.fetch_add(1, Ordering::SeqCst);
            drop(own(image).get_or_insert_with(|| reader_tally.probe()));
            assert_eq!(reader_tally.charged(), HELD, "the image's version holds it");
        })));
        let load = engine.bulk_load(BulkMode::Append);
        load.add(&[quad("e", "five")]);
        let finished = load.finish();
        IMAGE_READER.set(None);
        finished.unwrap();
        let mapped = format!("map_checkpoints = {map_checkpoints}");
        assert_eq!(reads.load(Ordering::SeqCst), 1, "{mapped}: the image read");
        assert_eq!(tally.drops(), 1, "{mapped}: its value dropped");
        assert_eq!(tally.charged(), 0, "{mapped}: its charge released");
        let published = engine.snapshot();
        assert!(
            matches!(published.admission(), Admission::Shared(_)),
            "{mapped}: published"
        );
        assert_eq!(
            published.len_in(ReadModel::Asserted),
            2,
            "{mapped}: the store's statement and the load's"
        );
        assert!(
            published.lookup(literal("five").as_ref()).is_some(),
            "{mapped}: the load's term"
        );
    }
}

/// Item 6: the slots' lock is a leaf, never held while a value is made.
///
/// - Input: a `make` that reads its snapshot, looks up its own type's slot, makes a value of
///   another type and, nested, one of its own type; on a thread with a deadline, so that a
///   lock held across `make` fails the test instead of hanging it.
/// - Expected: it returns; the nested value of its own type, stored first, is the one kept;
///   the other type's value is there.
#[test]
fn a_make_may_read_its_snapshot_and_its_slots() {
    struct Outer(u32);
    struct Read(usize);
    let (sent, got) = mpsc::channel();
    let reader = std::thread::spawn(move || {
        let engine = engine();
        let mut tx = engine.transaction();
        assert!(tx.insert(quad("b", "two").as_ref()));
        let pending = tx.pending_snapshot();
        let own = own(&pending);
        let outer = own.get_or_insert_with(|| {
            let read = own
                .snapshot()
                .quads_for_pattern_in(ReadModel::Asserted, &QuadPattern::all())
                .count();
            assert!(own.get::<Outer>().is_none(), "its own type, while made");
            drop(own.get_or_insert_with(|| Read(read)));
            drop(own.get_or_insert_with(|| Outer(1)));
            Outer(2)
        });
        let read = own.get::<Read>().map(|read| read.0);
        sent.send((outer.0, read)).unwrap();
    });
    match got.recv_timeout(Duration::from_secs(60)) {
        Ok((outer, read)) => {
            assert_eq!(outer, 1, "the nested value, stored first");
            assert_eq!(read, Some(2), "the other type's value");
        }
        Err(mpsc::RecvTimeoutError::Disconnected) => {
            std::panic::resume_unwind(reader.join().unwrap_err())
        }
        Err(mpsc::RecvTimeoutError::Timeout) => panic!("a make that reads its slots deadlocked"),
    }
}

/// Item 7: two value types on one version get separate values.
///
/// - Input: one pending snapshot; a value of each of two types, then each asked for again
///   with a different `make`.
/// - Expected: each type keeps its own first value.
#[test]
fn two_value_types_on_one_version_are_separate() {
    struct Spatial(u32);
    struct Results(u32);
    let engine = engine();
    let pending = engine
        .speculative(Arc::new(Budget::unlimited()))
        .pending_snapshot()
        .unwrap();
    let own = own(&pending);
    assert_eq!(own.get_or_insert_with(|| Spatial(1)).0, 1);
    assert!(own.get::<Results>().is_none(), "another type: none yet");
    assert_eq!(own.get_or_insert_with(|| Results(2)).0, 2);
    assert_eq!(
        own.get_or_insert_with(|| Spatial(3)).0,
        1,
        "the first type's"
    );
    assert_eq!(
        own.get_or_insert_with(|| Results(4)).0,
        2,
        "the second type's"
    );
}

/// Item 8: a commit never promotes a pending value.
///
/// - Input: a value made on a transaction's pending snapshot, kept across its commit.
/// - Expected: the engine's snapshot after the commit is `Shared` and of another version; the
///   kept pending snapshot still holds its value and charge until it is dropped.
#[test]
fn a_commit_never_promotes_a_pending_value() {
    let engine = engine();
    let tally = Tally::new();
    let mut tx = engine.transaction();
    assert!(tx.insert(quad("b", "two").as_ref()));
    let pending = tx.pending_snapshot();
    drop(own(&pending).get_or_insert_with(|| tally.probe()));
    tx.commit().unwrap();
    let published = engine.snapshot();
    assert!(matches!(published.admission(), Admission::Shared(_)));
    assert!(
        !published.same_version(&pending),
        "the commit's own version"
    );
    assert!(
        own(&pending).get::<Probe>().is_some(),
        "the kept pending snapshot's value"
    );
    assert_eq!(tally.charged(), HELD);
    drop(pending);
    assert_eq!(tally.charged(), 0);
    assert_eq!(tally.drops(), 1);
}
