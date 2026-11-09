//! Guard of a pending version's memory (DUR-6): a transaction whose pending changes hold a
//! quad in a named graph, over a store whose asserted stack has the default-graph layout,
//! reads the base in the quad layout. That form of each base run is made once and shared by
//! every pending version holding it, counted in the store's index memory until the last of
//! them lets it go; what a pending version makes for itself is charged to its reader's budget
//! before it is made. Measured (`nrese_exec::measure`), so this file is a test binary of its
//! own.

use std::sync::{Arc, Barrier};

use nrese_engine::{Engine, EngineConfig, EngineError, Snapshot};
use nrese_exec::{Budget, measure};
use nrese_rdf::{GraphName, NamedNode, Quad};

#[global_allocator]
static ALLOCATOR: measure::Measured<std::alloc::System> = measure::Measured(std::alloc::System);

fn node(name: &str) -> NamedNode {
    NamedNode::new_unchecked(format!("urn:{name}"))
}

/// An engine without background maintenance (no compaction or checkpoint on a thread of its
/// own while a measurement runs), holding `size` statements in the default graph only: its
/// asserted stack has the default-graph layout.
fn store(size: usize) -> Engine {
    let engine = Engine::new(EngineConfig {
        background_maintenance: false,
        ..EngineConfig::default()
    })
    .unwrap();
    let mut tx = engine.transaction();
    for i in 0..size {
        tx.insert(
            Quad::new(
                node(&format!("s{i}")),
                node("p"),
                node(&format!("o{i}")),
                GraphName::DefaultGraph,
            )
            .as_ref(),
        );
    }
    tx.commit().unwrap();
    engine
}

/// The statement `n` in a named graph.
fn named(n: usize) -> Quad {
    Quad::new(node(&format!("named{n}")), node("p"), node("o"), node("g"))
}

fn unlimited() -> Arc<Budget> {
    Arc::new(Budget::unlimited())
}

/// The pending snapshot of a speculative transaction (no writer slot: many at once, as
/// sessions' reads are) over `engine` adding `named(n)`, charged to `budget`.
fn pending(engine: &Engine, n: usize, budget: &Arc<Budget>) -> Snapshot {
    let mut tx = engine.speculative();
    tx.insert(named(n).as_ref());
    tx.pending_snapshot_within(budget).unwrap()
}

/// N pending versions over one base, each adding a statement in a named graph and kept (as N
/// sessions keep their views), hold one quad-layout form of the base between them, not N:
/// the most the process holds at once for 8 of them stays under twice what it holds for one.
/// Before DUR-6 each made a copy of its own, about 7/4 of the base's index, so 8 held about 8
/// times as much.
#[test]
fn pending_versions_over_one_base_hold_one_form_of_it() {
    if !measure::alone() {
        return;
    }
    let engine = store(20_000);
    let budget = unlimited();
    let held = |n: usize| {
        let (kept, peak) = measure::measure(|| {
            (0..n)
                .map(|i| pending(&engine, i, &budget))
                .collect::<Vec<_>>()
        });
        drop(kept);
        peak
    };
    let one = held(1);
    let eight = held(8);
    assert!(
        eight < 2 * one,
        "8 pending versions held {eight} bytes at most, one held {one}: not one shared form"
    );
}

/// What a pending version makes for itself (its runs of the pending changes, their lists, its
/// version) is charged to its reader's budget before it is made and held by the snapshot: no
/// more is held than charged at any time. The base's quad-layout form is the store's (made
/// here before the measurement, by another reader still holding it). The transaction keeps its
/// last pending snapshot, with the charge, until its next write. A budget too small for it
/// refuses the read with the typed memory error, and nothing is kept. Before DUR-6 nothing
/// was charged.
#[test]
fn a_pending_version_is_charged_for_what_it_makes() {
    if !measure::alone() {
        return;
    }
    let engine = store(2_000);
    let other = pending(&engine, 0, &unlimited());
    let budget = unlimited();
    let mut tx = engine.speculative();
    tx.insert(named(1).as_ref());
    let (snapshot, excess) =
        measure::measure_within(&budget, || tx.pending_snapshot_within(&budget).unwrap());
    assert!(excess == 0, "held {excess} bytes beyond its charge");
    assert!(budget.used() > 0, "nothing charged");
    drop(snapshot);
    assert!(
        budget.used() > 0,
        "the transaction's kept snapshot lost its charge"
    );
    tx.insert(named(2).as_ref());
    assert_eq!(
        budget.used(),
        0,
        "the charge outlived the kept snapshot's write"
    );
    let tiny = Arc::new(Budget::new(16));
    let refused = tx.pending_snapshot_within(&tiny);
    assert!(
        matches!(refused, Err(EngineError::MemoryLimit(_))),
        "a 16-byte budget: {:?}",
        refused.map(|_| ())
    );
    assert_eq!(tiny.used(), 0, "a refusal kept a charge");
    drop(other);
}

/// The shared form counts in the store's index memory (`EngineStats::index_bytes`) from when
/// it is made until its last holder lets it go: four pending versions over one base raise it
/// by one form's bytes (more than the base's own, fewer than two forms'), the same while any
/// of them is held, and back once the last is dropped. Before DUR-6 it never moved.
#[test]
fn the_shared_form_counts_in_the_index_memory_while_held() {
    let engine = store(20_000);
    let before = engine.stats().index_bytes;
    let budget = unlimited();
    let mut kept: Vec<Snapshot> = (0..4).map(|i| pending(&engine, i, &budget)).collect();
    let held = engine.stats().index_bytes.saturating_sub(before);
    assert!(
        held > before && held < 3 * before,
        "4 pending versions over a base of {before} index bytes added {held}: not one form"
    );
    kept.truncate(1);
    assert_eq!(
        engine.stats().index_bytes.saturating_sub(before),
        held,
        "one holder left, the form counted as before"
    );
    drop(kept);
    assert_eq!(
        engine.stats().index_bytes,
        before,
        "the last holder gone, the form still counted"
    );
}

/// Two pending versions converting one base at the same moment end with one form between
/// them (a duplicate either made is freed when its maker takes the other's): the index memory
/// rises by one form's bytes, and goes back once both are dropped. Before DUR-6 neither form
/// was counted, and each kept its own.
#[test]
fn pending_versions_converting_one_base_at_once_share_one_form() {
    let engine = store(20_000);
    let before = engine.stats().index_bytes;
    let budget = unlimited();
    let start = Barrier::new(2);
    let both: Vec<Snapshot> = std::thread::scope(|scope| {
        let handles: Vec<_> = (0..2)
            .map(|i| {
                let (engine, budget, start) = (&engine, &budget, &start);
                scope.spawn(move || {
                    let mut tx = engine.speculative();
                    tx.insert(named(i).as_ref());
                    start.wait();
                    tx.pending_snapshot_within(budget).unwrap()
                })
            })
            .collect();
        handles.into_iter().map(|h| h.join().unwrap()).collect()
    });
    let held = engine.stats().index_bytes.saturating_sub(before);
    assert!(
        held > before && held < 2 * before + before / 2,
        "two pending versions made at once over a base of {before} index bytes added {held}"
    );
    drop(both);
    assert_eq!(
        engine.stats().index_bytes,
        before,
        "both dropped, a form still counted"
    );
}

/// An unchanged transaction reading under the same budget gets the same pending snapshot (its
/// asserted identity, its own per version made, is the same); a write in between, or another
/// budget, gets a new one (DICT2's memo, keyed by the change generation and the budget).
#[test]
fn an_unchanged_transaction_keeps_its_pending_snapshot_per_budget() {
    let engine = store(10);
    let (a, b) = (unlimited(), unlimited());
    let mut tx = engine.speculative();
    tx.insert(named(0).as_ref());
    let first = tx.pending_snapshot_within(&a).unwrap().asserted_identity();
    let again = tx.pending_snapshot_within(&a).unwrap().asserted_identity();
    assert_eq!(first, again, "unchanged, same budget: a new snapshot");
    let other = tx.pending_snapshot_within(&b).unwrap().asserted_identity();
    assert_ne!(first, other, "another budget: the same snapshot");
    tx.insert(named(1).as_ref());
    let after = tx.pending_snapshot_within(&a).unwrap().asserted_identity();
    assert_ne!(first, after, "after a write: the same snapshot");
}

/// The statement `s{i} p o{i}` of [`store`] as the transaction's ids, a default-graph triple.
fn triple(tx: &nrese_engine::Transaction<'_>, i: usize) -> nrese_engine::EncodedTriple {
    let id = |name: &str| tx.lookup(node(name).as_ref().into()).unwrap();
    nrese_engine::EncodedTriple {
        subject: id(&format!("s{i}")),
        predicate: id("p"),
        object: id(&format!("o{i}")),
    }
}

/// A transaction whose statements were retracted, inferred, then asserted again (sol-loop r1 on
/// DUR-6, S1) shadows inferred inserts that aren't asserted inserts: a pending snapshot charges
/// its scratch for them too, holding nothing beyond its charge. Before the fix the bound counted
/// only the asserted inserts.
#[test]
fn a_pending_version_is_charged_for_inferred_statements_asserted_again() {
    if !measure::alone() {
        return;
    }
    let engine = store(1_000);
    let other = pending(&engine, 0, &unlimited());
    let mut tx = engine.speculative();
    for i in 0..1_000 {
        let quad = Quad::new(
            node(&format!("s{i}")),
            node("p"),
            node(&format!("o{i}")),
            GraphName::DefaultGraph,
        );
        let inferred = triple(&tx, i);
        tx.remove(quad.as_ref());
        tx.insert_inferred(inferred);
        tx.insert(quad.as_ref());
    }
    tx.insert(named(1).as_ref());
    let budget = unlimited();
    let (snapshot, excess) =
        measure::measure_within(&budget, || tx.pending_snapshot_within(&budget).unwrap());
    assert!(excess == 0, "held {excess} bytes beyond its charge");
    drop((snapshot, tx, other));
}

/// A term the transaction interns after a pending snapshot is kept (sol-loop r1 on DUR-6, S2):
/// the next pending snapshot under the same budget knows it. Before the fix the kept snapshot
/// came back with the old dictionary length, and its lookup of the new term answered nothing.
#[test]
fn a_kept_pending_snapshot_is_not_returned_after_an_intern() {
    let engine = store(10);
    let budget = unlimited();
    let mut tx = engine.speculative();
    tx.insert(named(0).as_ref());
    drop(tx.pending_snapshot_within(&budget).unwrap());
    let fresh = node("fresh");
    let id = tx.intern(fresh.as_ref().into());
    let snapshot = tx.pending_snapshot_within(&budget).unwrap();
    assert_eq!(
        snapshot.lookup(fresh.as_ref().into()),
        Some(id),
        "the term interned after the kept snapshot"
    );
}

/// A kept pending snapshot no longer current (a term interned since) is let go before its
/// replacement is charged (sol-loop r2 on DUR-6, S4): a budget just enough for one pending
/// snapshot (the highest a first one used) makes the replacement too, once the caller has dropped
/// its own. Before the fix the stale snapshot kept its charge through the replacement's build,
/// which the budget then refused.
#[test]
fn a_stale_kept_pending_snapshot_is_let_go_before_its_replacement_is_charged() {
    let engine = store(10);
    let mut tx = engine.speculative();
    tx.insert(named(0).as_ref());
    let calibration = unlimited();
    drop(tx.pending_snapshot_within(&calibration).unwrap());
    let budget = Arc::new(Budget::new(calibration.peak()));
    drop(tx.pending_snapshot_within(&budget).unwrap());
    let fresh = node("fresh_after_memo");
    let id = tx.intern(fresh.as_ref().into());
    let snapshot = tx
        .pending_snapshot_within(&budget)
        .expect("a budget enough for one pending snapshot");
    assert_eq!(snapshot.lookup(fresh.as_ref().into()), Some(id));
}

/// A pending snapshot refused at any point of what it makes (sol-loop r1 on DUR-6, S3: the
/// inferred stack's run refused after the asserted one was made) holds nothing beyond its charge
/// at any moment, and keeps nothing: budgets from nothing up, each a fiftieth of what the made
/// snapshot keeps larger than the last, until one is enough (the scratch, charged first, needs
/// more than the snapshot keeps, so the steps pass through every stage). Before the fix a
/// refusal of the inferred run released the asserted run's charge while its arrays were still
/// there.
#[test]
fn a_refused_pending_snapshot_releases_nothing_before_it_frees_it() {
    if !measure::alone() {
        return;
    }
    let engine = store(2_000);
    let other = pending(&engine, 0, &unlimited());
    // The asserted run large enough that the fixed charge made first (its version, lists and
    // box, mostly not made yet when the inferred run is refused) can't cover it.
    let changes = |tx: &mut nrese_engine::Transaction<'_>| {
        for n in 1..=500 {
            tx.insert(named(n).as_ref());
        }
        for i in 0..500 {
            let inferred = nrese_engine::EncodedTriple {
                object: triple(tx, i + 500).object,
                ..triple(tx, i)
            };
            tx.insert_inferred(inferred);
        }
    };
    let whole = {
        let budget = unlimited();
        let mut tx = engine.speculative();
        changes(&mut tx);
        let snapshot = tx.pending_snapshot_within(&budget).unwrap();
        let used = budget.used();
        drop((snapshot, tx));
        used
    };
    assert!(whole > 0);
    let step = (whole / 50).max(1);
    let mut refused = 0;
    for limit in (0..).step_by(step) {
        assert!(refused < 10_000, "still refused at a budget of {limit}");
        let budget = Arc::new(Budget::new(limit));
        let mut tx = engine.speculative();
        changes(&mut tx);
        let (made, excess) =
            measure::measure_within(&budget, || tx.pending_snapshot_within(&budget));
        assert!(
            excess == 0,
            "a budget of {limit} held {excess} bytes beyond its charge"
        );
        let enough = made.is_ok();
        if !enough {
            refused += 1;
            assert_eq!(budget.used(), 0, "a refusal kept a charge");
        }
        drop((made, tx));
        if enough {
            break;
        }
    }
    assert!(refused > 0, "no budget refused");
    drop(other);
}

/// A pending version as a reader of a restricted view reads it (`pending_over_within`, review r1
/// on DUR-6a, N2): what it makes is charged to its reader's budget before it is made and held by
/// the snapshot, with its own scratch (the inferred statements gone: the pending deletes, the
/// pending asserts and `hidden`), over a view of a base with inferred statements, a named-graph
/// insert pending; the charge goes with the snapshot (nothing is kept: it is a new one each call).
/// A 16-byte budget refuses it with the typed memory error, and nothing stays charged. A guard of
/// the over path's own bounds, which the pending snapshot's tests don't reach.
#[test]
fn a_pending_version_over_a_view_is_charged_for_what_it_makes() {
    if !measure::alone() {
        return;
    }
    let engine = store(2_000);
    {
        let mut tx = engine.transaction();
        for i in 0..200 {
            let inferred = nrese_engine::EncodedTriple {
                object: triple(&tx, i + 200).object,
                ..triple(&tx, i)
            };
            tx.insert_inferred(inferred);
        }
        tx.commit().unwrap();
    }
    let other = pending(&engine, 0, &unlimited());
    let base = engine.snapshot();
    let view = base.with_inferred_subset(nrese_engine::InferredSubset::Without(Vec::new()));
    let mut tx = engine.speculative();
    tx.insert(named(1).as_ref());
    for i in 0..50 {
        let quad = Quad::new(
            node(&format!("s{i}")),
            node("p"),
            node(&format!("o{}", i + 200)),
            GraphName::DefaultGraph,
        );
        tx.insert(quad.as_ref());
    }
    let hidden: Vec<nrese_engine::EncodedQuad> = (50..100)
        .map(|i| {
            nrese_engine::EncodedTriple {
                object: triple(&tx, i + 200).object,
                ..triple(&tx, i)
            }
            .in_default_graph()
        })
        .collect();
    let budget = unlimited();
    let (snapshot, excess) = measure::measure_within(&budget, || {
        tx.pending_over_within(&view, &hidden, &budget).unwrap()
    });
    assert!(excess == 0, "held {excess} bytes beyond its charge");
    assert!(budget.used() > 0, "nothing charged");
    drop(snapshot);
    assert_eq!(budget.used(), 0, "the charge outlived the snapshot");
    let tiny = Arc::new(Budget::new(16));
    let refused = tx.pending_over_within(&view, &hidden, &tiny);
    assert!(
        matches!(refused, Err(EngineError::MemoryLimit(_))),
        "a 16-byte budget: {:?}",
        refused.map(|_| ())
    );
    assert_eq!(tiny.used(), 0, "a refusal kept a charge");
    drop((tx, view, base, other));
}
