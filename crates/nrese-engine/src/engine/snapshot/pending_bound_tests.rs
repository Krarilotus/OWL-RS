//! A pending snapshot's one decision (DUR-6 slice 2; F1 of the dictionary's charged-build
//! review): its deltas' bounds are charged before the build only where the budget's room
//! covers them; elsewhere every delta is charged exactly as it is built, the reference. A
//! budget the reference fits is answered, and not recorded as refused, though the bounds don't
//! fit it. Red on the bounds charged unconditionally (2f91a6d3's rule): that budget refused.
//! A delta at or above the parallel threshold beside one below it is built exactly throughout
//! (review r1, DUR6B-R1-01): red on fe517a03, whose bounded path held the small delta's slack
//! beside the large one's exact charges.

use std::sync::Arc;

use nrese_exec::Budget;
use nrese_rdf::{GraphName, NamedNode, Quad};

use crate::index::run::Run;
use crate::{EncodedTriple, Engine, EngineConfig, Transaction};

/// A delta at the parallel threshold: charged exactly as it is built.
const LARGE: usize = 16 * 1024;

fn node(name: &str) -> NamedNode {
    NamedNode::new_unchecked(format!("urn:{name}"))
}

fn triple(i: usize) -> Quad {
    Quad::new(
        node(&format!("s{i}")),
        node("p"),
        node(&format!("o{i}")),
        GraphName::DefaultGraph,
    )
}

/// `n` inferred statements from `from` on, over terms interned for them.
fn inferred(tx: &mut Transaction<'_>, from: usize, n: usize) {
    let predicate = tx.intern(node("q").as_ref().into());
    let object = tx.intern(node("o").as_ref().into());
    for i in from..from + n {
        let subject = tx.intern(node(&format!("s{i}")).as_ref().into());
        tx.insert_inferred(EncodedTriple {
            subject,
            predicate,
            object,
        });
    }
}

/// The pending changes of a row.
type Changes = fn(&mut Transaction<'_>);

/// A row: its name, its changes, and whether its bounds hold more than the reference (every
/// delta below the threshold) or it is built exactly throughout (a delta at or above it).
type Row = (&'static str, Changes, bool);

/// The most `budget` held at once while a transaction over `engine`, given `changes`, made
/// its pending snapshot (the reference forced where `reference`), and whether it made it. On
/// one worker: a build's peak doesn't depend on how its permutations meet in time.
fn peak_of(
    pool: &rayon::ThreadPool,
    engine: &Engine,
    changes: Changes,
    budget: &Arc<Budget>,
    reference: bool,
) -> (usize, bool) {
    pool.install(|| {
        super::reference::ONLY.set(reference);
        let mut tx = engine.transaction();
        changes(&mut tx);
        let made = tx.pending_snapshot_within(budget).is_ok();
        drop(tx);
        super::reference::ONLY.set(false);
        (budget.peak(), made)
    })
}

#[test]
fn a_budget_the_reference_fits_is_answered() {
    assert!(Run::charged_exactly(LARGE) && !Run::charged_exactly(LARGE - 1));
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(1)
        .build()
        .unwrap();
    let engine = Engine::new(EngineConfig {
        background_maintenance: false,
        ..EngineConfig::default()
    })
    .unwrap();
    let mut tx = engine.transaction();
    for i in 0..100 {
        tx.insert(triple(i).as_ref());
    }
    tx.commit().unwrap();
    let rows: [Row; 6] = [
        // F1's shape: two default-graph triples, one predicate shared.
        (
            "two triples",
            |tx| {
                for i in 1_000..1_002 {
                    tx.insert(triple(i).as_ref());
                }
            },
            true,
        ),
        // The base read in the quad layout.
        (
            "a named graph",
            |tx| {
                tx.insert(Quad::new(node("s"), node("p"), node("o"), node("g")).as_ref());
            },
            true,
        ),
        (
            "inserts and deletes",
            |tx| {
                for i in 0..40 {
                    tx.remove(triple(i).as_ref());
                    tx.insert(triple(2_000 + i).as_ref());
                }
            },
            true,
        ),
        (
            "asserted and inferred",
            |tx| {
                for i in 3_000..3_010 {
                    tx.insert(triple(i).as_ref());
                }
                inferred(tx, 0, 10);
            },
            true,
        ),
        // DUR6B-R1-01's construction, both ways round.
        (
            "one asserted beside a large inferred delta",
            |tx| {
                tx.insert(triple(4_000).as_ref());
                inferred(tx, 10_000, LARGE);
            },
            false,
        ),
        (
            "a large asserted delta beside one inferred",
            |tx| {
                for i in 0..LARGE {
                    tx.insert(triple(100_000 + i).as_ref());
                }
                inferred(tx, 0, 1);
            },
            false,
        ),
    ];
    let mut wrong = Vec::new();
    for (row, changes, separates) in rows {
        let unlimited = || Arc::new(Budget::unlimited());
        let (reference, _) = peak_of(&pool, &engine, changes, &unlimited(), true);
        let (bounded, _) = peak_of(&pool, &engine, changes, &unlimited(), false);
        if separates && bounded <= reference {
            // The row would exercise nothing: the bounds fit wherever the reference does.
            wrong.push(format!(
                "{row}: the bounds' peak {bounded} isn't above the reference's {reference}"
            ));
            continue;
        }
        let budget = Arc::new(Budget::new(reference));
        let (_, made) = peak_of(&pool, &engine, changes, &budget, false);
        if !made || budget.refused() {
            wrong.push(format!(
                "{row}: a budget of {reference} bytes, the reference's peak (unlimited, the \
                 candidate's {bounded}): answered {made}, recorded refused {}",
                budget.refused()
            ));
        }
        if budget.used() != 0 {
            wrong.push(format!("{row}: {} bytes still charged", budget.used()));
        }
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
}
