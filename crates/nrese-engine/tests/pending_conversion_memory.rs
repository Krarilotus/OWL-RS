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
