//! A pending snapshot's one decision (DUR-6 slice 2; F1 of the dictionary's charged-build
//! review): its deltas' bounds are charged before the build only where the budget's room
//! covers them; elsewhere every delta is charged exactly as it is built, the reference. A
//! budget the reference fits is answered, and not recorded as refused, though the bounds don't
//! fit it. Red on the bounds charged unconditionally (2f91a6d3's rule): that budget refused.

use std::sync::Arc;

use nrese_exec::Budget;
use nrese_rdf::{GraphName, NamedNode, Quad};

use crate::{EncodedTriple, Engine, EngineConfig, Transaction};

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

/// The pending changes of a row.
type Changes = fn(&mut Transaction<'_>);

/// The most `budget` held at once while a transaction over `engine`, given `changes`, made
/// its pending snapshot, and whether it made it.
fn peak_of(engine: &Engine, changes: Changes, budget: &Arc<Budget>) -> (usize, bool) {
    let mut tx = engine.transaction();
    changes(&mut tx);
    let made = tx.pending_snapshot_within(budget).is_ok();
    drop(tx);
    (budget.peak(), made)
}

#[test]
fn a_budget_the_reference_fits_is_answered() {
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
    let rows: [(&str, Changes); 4] = [
        // F1's shape: two default-graph triples, one predicate shared.
        ("two triples", |tx| {
            for i in 1_000..1_002 {
                tx.insert(triple(i).as_ref());
            }
        }),
        // The base read in the quad layout.
        ("a named graph", |tx| {
            tx.insert(Quad::new(node("s"), node("p"), node("o"), node("g")).as_ref());
        }),
        ("inserts and deletes", |tx| {
            for i in 0..40 {
                tx.remove(triple(i).as_ref());
                tx.insert(triple(2_000 + i).as_ref());
            }
        }),
        ("asserted and inferred", |tx| {
            for i in 3_000..3_010 {
                tx.insert(triple(i).as_ref());
            }
            for i in 0..10 {
                let inferred = EncodedTriple {
                    subject: tx.intern(node(&format!("s{i}")).as_ref().into()),
                    predicate: tx.intern(node("q").as_ref().into()),
                    object: tx.intern(node(&format!("o{}", i + 1)).as_ref().into()),
                };
                tx.insert_inferred(inferred);
            }
        }),
    ];
    let mut wrong = Vec::new();
    for (row, changes) in rows {
        super::reference::ONLY.set(true);
        let (reference, _) = peak_of(&engine, changes, &Arc::new(Budget::unlimited()));
        super::reference::ONLY.set(false);
        let (bounded, _) = peak_of(&engine, changes, &Arc::new(Budget::unlimited()));
        if bounded <= reference {
            // The row would exercise nothing: the bounds fit wherever the reference does.
            wrong.push(format!(
                "{row}: the bounds' peak {bounded} isn't above the reference's {reference}"
            ));
            continue;
        }
        let budget = Arc::new(Budget::new(reference));
        let (_, made) = peak_of(&engine, changes, &budget);
        if !made || budget.refused() {
            wrong.push(format!(
                "{row}: a budget of {reference} bytes, the reference's peak (the bounds' \
                 {bounded}): answered {made}, recorded refused {}",
                budget.refused()
            ));
        }
        if budget.used() != 0 {
            wrong.push(format!("{row}: {} bytes still charged", budget.used()));
        }
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
}
