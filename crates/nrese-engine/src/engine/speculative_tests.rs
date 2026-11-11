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

/// Far above any answer's time: reaching it means the readers wait on each other (the lock
/// tests' deadline).
const DEADLINE: std::time::Duration = std::time::Duration::from_secs(60);

/// Pending inserts past `Run::from_delta`'s parallel-build threshold (16,384), so making the
/// pending snapshot runs Rayon work.
const PARALLEL_CHANGES: usize = 20_000;

/// A private transaction over `engine` with [`PARALLEL_CHANGES`] asserted statements of its
/// own, its memo cold, its terms charged to `budget`.
fn large_private<'e>(engine: &'e Engine, budget: &Arc<Budget>) -> super::PrivateTransaction<'e> {
    let mut tx = engine.speculative(Arc::clone(budget));
    for i in 0..PARALLEL_CHANGES {
        assert!(
            tx.try_insert(quad(&format!("big{i}"), "x").as_ref())
                .unwrap()
        );
    }
    tx
}

/// R1-C1 of V2's review r1 (`notes/review-dict-v2-r1/review.md`): the memo's lock and the
/// pool. Red on the parent, whose memo held its lock while the snapshot was built.
///
/// - Input: 8 rounds, each on a thread of its own: a fresh private transaction with 20,000
///   pending statements (past the parallel-build threshold), so its memo starts cold, and 64
///   readers of `pending_snapshot_within`, one budget, run as parallel work on a pool of four
///   (as SHACL's validation reads one view from its checks on Rayon). All bounded by one
///   [`DEADLINE`].
/// - Expected: every round answers, each with one snapshot (one identity) for all its readers.
#[test]
fn parallel_readers_of_a_cold_memo_finish_while_it_is_built_on_the_pool() {
    const ROUNDS: usize = 8;
    let engine = engine();
    let (answer, answered) = std::sync::mpsc::channel();
    for round in 0..ROUNDS {
        let (engine, answer) = (engine.clone(), answer.clone());
        std::thread::spawn(move || {
            let budget = Arc::new(Budget::unlimited());
            let tx = large_private(&engine, &budget);
            let pool = rayon::ThreadPoolBuilder::new()
                .num_threads(4)
                .build()
                .expect("a pool of four");
            let identities: Vec<_> = pool.install(|| {
                use rayon::prelude::*;
                (0..64)
                    .into_par_iter()
                    .map(|_| tx.pending_snapshot_within(&budget).unwrap().identity())
                    .collect()
            });
            let _ = answer.send((round, identities));
        });
    }
    let until = std::time::Instant::now() + DEADLINE;
    let mut answered_rounds = Vec::new();
    while answered_rounds.len() < ROUNDS {
        let left = until.saturating_duration_since(std::time::Instant::now());
        match answered.recv_timeout(left) {
            Ok(round) => answered_rounds.push(round),
            Err(_) => break,
        }
    }
    let timed_out = ROUNDS - answered_rounds.len();
    assert_eq!(
        timed_out, 0,
        "{timed_out} of {ROUNDS} rounds of readers of a cold memo still wait after {DEADLINE:?}: \
         the memo's lock held across the build"
    );
    for (round, identities) in answered_rounds {
        assert_eq!(identities.len(), 64);
        assert!(
            identities.iter().all(|id| *id == identities[0]),
            "round {round}: one snapshot for the state"
        );
    }
}

/// R1-C1 again, in the schedule that deadlocked the parent's memo (V2's lead, 06:44: readers
/// that only park on the lock steal nothing, so a plain `par_iter` of readers rarely meets it).
///
/// - Input: 12 trials, each a fresh private transaction with 20,000 pending statements, on a
///   pool of three, each trial on a thread of its own with its own [`TRIAL_DEADLINE`]. The
///   first read starts the cold build on one worker (past the parallel-build threshold, so
///   its build forks onto the pool). A second worker waits [`READER_DELAY`], then forks two
///   reads: the first waits for the build, the second stays queued in that worker's deque.
///   The third worker is idle, so it steals the build's work, and the builder, waiting for it,
///   can steal the queued read.
/// - Expected: every trial finishes, its three reads with one snapshot. (On the parent the
///   builder held the memo's lock and met it again in the stolen read: a deadlock.)
#[test]
fn a_builder_waiting_on_the_pool_never_meets_its_own_lock() {
    const TRIALS: usize = 12;
    let engine = engine();
    let mut timed_out = 0;
    for trial in 0..TRIALS {
        let (answer, answered) = std::sync::mpsc::channel();
        let engine = engine.clone();
        std::thread::spawn(move || {
            let budget = Arc::new(Budget::unlimited());
            let tx = large_private(&engine, &budget);
            let pool = rayon::ThreadPoolBuilder::new()
                .num_threads(3)
                .build()
                .expect("a pool of three");
            let read = || tx.pending_snapshot_within(&budget).unwrap().identity();
            let (built, (first, second)) = pool.install(|| {
                rayon::join(read, || {
                    std::thread::sleep(READER_DELAY);
                    rayon::join(read, read)
                })
            });
            let _ = answer.send(built == first && first == second);
        });
        match answered.recv_timeout(TRIAL_DEADLINE) {
            Ok(one) => assert!(one, "trial {trial}: one snapshot for the state"),
            Err(_) => timed_out += 1,
        }
    }
    assert_eq!(
        timed_out, 0,
        "{timed_out} of {TRIALS} trials still wait after {TRIAL_DEADLINE:?}: the builder met \
         the memo's lock in a read it stole"
    );
}

/// Far above one trial's time (a debug build of 20,000 pending changes takes well under a
/// second): reaching it means the trial's reads wait on each other.
const TRIAL_DEADLINE: std::time::Duration = std::time::Duration::from_secs(20);

/// How long the second worker waits before its reads, so the build has started and taken the
/// lock first: short against the build, long against a fork.
const READER_DELAY: std::time::Duration = std::time::Duration::from_millis(5);

/// DICT2's correction (06:38): a memo entry is keyed by the vocabulary end its snapshot froze.
/// `try_intern` takes `&self`, so a term can be added while a snapshot is built.
///
/// - Input: a private transaction with 20,000 pending statements; one thread interning 2,000
///   new terms of its own while another asks for the pending snapshot 50 times. Bounded by
///   [`DEADLINE`].
/// - Expected: after every ask, the memo's key end is the end its snapshot froze; once the
///   interning stops, the next ask is answered again with the same snapshot.
#[test]
fn a_memo_is_keyed_by_the_end_its_snapshot_froze() {
    let engine = engine();
    let (answer, answered) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let budget = Arc::new(Budget::unlimited());
        let tx = large_private(&engine, &budget);
        let mismatches = std::thread::scope(|scope| {
            scope.spawn(|| {
                for i in 0..2_000 {
                    let iri = format!("http://example.com/raced/{i}");
                    tx.try_intern(NamedNodeRef::new_unchecked(&iri).into())
                        .unwrap();
                }
            });
            let mut mismatches = Vec::new();
            for ask in 0..50 {
                drop(tx.pending_snapshot_within(&budget).unwrap());
                if let Some((key, frozen)) = tx.memo_ends()
                    && key != frozen
                {
                    mismatches.push((ask, key, frozen));
                }
            }
            mismatches
        });
        let settled = tx.pending_snapshot_within(&budget).unwrap().identity();
        let again = tx.pending_snapshot_within(&budget).unwrap().identity();
        let _ = answer.send((mismatches, settled == again));
    });
    let (mismatches, kept) = answered
        .recv_timeout(DEADLINE)
        .unwrap_or_else(|_| panic!("the asks still wait after {DEADLINE:?}"));
    assert!(
        mismatches.is_empty(),
        "(ask, key end, frozen end): {mismatches:?}"
    );
    assert!(kept, "a settled state's snapshot is kept");
}

/// The race the unlocked build allows: misses at once both build; one snapshot is installed
/// and kept, and every other one is dropped with its charge.
///
/// - Input: two private transactions alike (20,000 pending statements, each its own budget);
///   one read once; the other read by 8 threads released together by a barrier, each its
///   first read, so they miss at once. Bounded by [`DEADLINE`].
/// - Expected: the 8 readers get one snapshot (one identity), the one a ninth read gets too;
///   once their answers are dropped, the second budget holds what the first holds with its
///   one snapshot: no loser's charge stays.
#[test]
fn concurrent_misses_install_one_snapshot_and_release_the_others() {
    let engine = engine();
    let (answer, answered) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let once = Arc::new(Budget::unlimited());
        let single = large_private(&engine, &once);
        drop(single.pending_snapshot_within(&once).unwrap());
        let raced = Arc::new(Budget::unlimited());
        let tx = large_private(&engine, &raced);
        let barrier = std::sync::Barrier::new(8);
        let identities: Vec<_> = std::thread::scope(|scope| {
            let readers: Vec<_> = (0..8)
                .map(|_| {
                    scope.spawn(|| {
                        barrier.wait();
                        tx.pending_snapshot_within(&raced).unwrap().identity()
                    })
                })
                .collect();
            readers.into_iter().map(|r| r.join().unwrap()).collect()
        });
        let ninth = tx.pending_snapshot_within(&raced).unwrap().identity();
        let _ = answer.send((identities, ninth, once.used(), raced.used()));
    });
    let (identities, ninth, once, raced) = answered
        .recv_timeout(DEADLINE)
        .unwrap_or_else(|_| panic!("concurrent readers still wait after {DEADLINE:?}"));
    assert!(
        identities.iter().all(|id| *id == ninth),
        "one snapshot installed"
    );
    assert_eq!(raced, once, "no loser's snapshot charge stays");
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

/// The shared kind through `PendingWrite` charges as its own charged writes do: since #219,
/// engine-v2's update path calls `Transaction::try_insert` and `try_remove`, and a generic one
/// calls the trait (V2's lead, 9 Nov).
///
/// - Input: a write transaction whose pending changes are charged to an empty budget; through
///   the trait, an insert of a new statement, then the removal of the committed one.
/// - Expected: each refused for memory, as the inherent charged calls refuse, and nothing
///   pending.
#[test]
fn a_write_transaction_through_the_trait_charges_its_pending_growth() {
    let engine = engine();
    let mut tx = engine.transaction();
    tx.charge_pending_to(Arc::new(Budget::new(0))).unwrap();
    let inserted = PendingWrite::try_insert(&mut tx, quad("b", "two").as_ref());
    assert!(
        matches!(inserted, Err(crate::EngineError::MemoryLimit(_))),
        "{inserted:?}"
    );
    let removed = PendingWrite::remove(&mut tx, quad("a", "one").as_ref());
    assert!(
        matches!(removed, Err(crate::EngineError::MemoryLimit(_))),
        "{removed:?}"
    );
    assert_eq!(tx.pending(), (0, 0));
}

/// The shared kind's pending snapshots through `PendingView` are charged as its own
/// `pending_snapshot_within` charges them (DUR-6): the same shape as the writes'.
///
/// - Input: a write transaction with one pending insert; its pending snapshot asked for through
///   the trait, against an empty budget.
/// - Expected: refused for memory, as `Transaction::pending_snapshot_within` refuses it.
#[test]
fn a_write_transactions_pending_snapshot_through_the_trait_is_charged() {
    let engine = engine();
    let mut tx = engine.transaction();
    tx.insert(quad("b", "two").as_ref());
    let empty = Arc::new(Budget::new(0));
    let snapshot = PendingView::pending_snapshot_within(&tx, &empty);
    assert!(
        matches!(snapshot, Err(crate::EngineError::MemoryLimit(_))),
        "{:?}",
        snapshot.map(|_| ())
    );
}

/// Every private write is charged, its pending changes as its terms are (§1.1: "every write is
/// fallible"; the budget attached at its opening pays for the changes since D16-039).
///
/// - Input: a private transaction over the committed `ex:a ex:p "one"` with an empty budget;
///   through `PendingWrite`, the removal of that statement, then the removal of every statement
///   in the default graph.
/// - Expected: each refused for memory, and nothing pending.
#[test]
fn a_private_transactions_pending_growth_is_charged_to_its_budget() {
    let engine = engine();
    let mut tx = engine.speculative(Arc::new(Budget::new(0)));
    let removed = PendingWrite::remove(&mut tx, quad("a", "one").as_ref());
    assert!(
        matches!(removed, Err(crate::EngineError::MemoryLimit(_))),
        "{removed:?}"
    );
    let every = crate::QuadPattern::in_graph(crate::TermId::DEFAULT_GRAPH);
    let matched = PendingWrite::remove_matching(&mut tx, &every);
    assert!(
        matches!(matched, Err(crate::EngineError::MemoryLimit(_))),
        "{matched:?}"
    );
    assert_eq!(tx.pending(), (0, 0));
}

/// A private transaction's pending snapshots are charged to the reader's budget, as a write
/// transaction's are (DUR-6; V1-engine review r1, F2): what they make is charged before it is
/// made and held while they live.
///
/// - Input: a private transaction with a statement of its own; through `PendingView`, its
///   pending snapshot and its pending snapshot over the base, within an empty budget, then the
///   latter within an unlimited one.
/// - Expected: both refused for memory within the empty budget; within the other, charged
///   while the snapshot lives, nothing once it goes.
#[test]
fn a_private_transactions_pending_snapshots_are_charged_to_their_budget() {
    let engine = engine();
    let base = engine.snapshot();
    let mut tx = engine.speculative(Arc::new(Budget::unlimited()));
    tx.try_insert(quad("b", "own").as_ref()).unwrap();
    let empty = Arc::new(Budget::new(0));
    let snapshot = PendingView::pending_snapshot_within(&tx, &empty).map(|_| ());
    assert!(
        matches!(snapshot, Err(crate::EngineError::MemoryLimit(_))),
        "{snapshot:?}"
    );
    let over = PendingView::pending_over_within(&tx, &base, &[], &empty).map(|_| ());
    assert!(
        matches!(over, Err(crate::EngineError::MemoryLimit(_))),
        "{over:?}"
    );
    let reader = Arc::new(Budget::unlimited());
    let over = PendingView::pending_over_within(&tx, &base, &[], &reader).unwrap();
    assert!(reader.used() > 0, "charged while it lives");
    drop(over);
    assert_eq!(reader.used(), 0, "released with it");
}
