//! Guard of an inferred addition's memory (`Snapshot::with_inferred_added_within`, the
//! `owl2-dl` mode's bounds views): everything the view makes is charged to its budget before
//! it is made, held until the view goes, and nothing is left charged after a refusal. Its
//! answers are those of `Snapshot::with_inferred_added`. Measured against the budget
//! (`nrese_exec::measure`), so this file is a test binary of its own.

use std::sync::Arc;

use nrese_engine::{EncodedTriple, Engine, EngineConfig, QuadPattern, ReadModel, Snapshot};
use nrese_exec::{Budget, measure};
use nrese_rdf::{NamedNode, Quad};

#[global_allocator]
static ALLOCATOR: measure::Measured<std::alloc::System> = measure::Measured(std::alloc::System);

fn node(name: &str) -> NamedNode {
    NamedNode::new_unchecked(format!("urn:{name}"))
}

/// An engine without background maintenance (every thread counts in a measurement), with
/// a few asserted statements, and `added` triples over terms interned before any measurement.
fn engine(added: usize) -> (Engine, Vec<EncodedTriple>) {
    let engine = Engine::new(EngineConfig {
        background_maintenance: false,
        ..EngineConfig::default()
    })
    .unwrap();
    let mut tx = engine.transaction();
    for i in 0..16 {
        tx.insert(
            Quad::new(
                node(&format!("a{i}")),
                node("p"),
                node("b"),
                nrese_rdf::GraphName::DefaultGraph,
            )
            .as_ref(),
        );
    }
    let p = tx.intern(node("q").as_ref().into());
    let o = tx.intern(node("c").as_ref().into());
    let triples: Vec<EncodedTriple> = (0..added)
        .map(|i| EncodedTriple {
            subject: tx.intern(node(&format!("s{i}")).as_ref().into()),
            predicate: p,
            object: o,
        })
        .collect();
    tx.commit().unwrap();
    (engine, triples)
}

fn all(snapshot: &Snapshot) -> Vec<String> {
    let mut quads: Vec<String> = snapshot
        .quads_for_pattern_in(ReadModel::Materialised, &QuadPattern::all())
        .map(|q| format!("{q:?}"))
        .collect();
    quads.sort_unstable();
    quads
}

/// Below and past the run builder's parallel threshold (16,384 statements): the view holds
/// nothing beyond its charge from before it is made until it goes, then nothing stays
/// charged; it reads what the uncharged addition reads.
#[test]
fn an_inferred_addition_is_charged_for_all_it_holds_until_it_goes() {
    if !measure::alone() {
        return;
    }
    let mut failed = Vec::new();
    for added in [100, 20_000] {
        let (engine, triples) = engine(added);
        let snapshot = engine.snapshot();
        let quads: Vec<_> = triples.iter().map(|t| t.in_default_graph()).collect();
        let expected = all(&snapshot.with_inferred_added(&quads));
        let budget = Arc::new(Budget::unlimited());
        // The view from before it is made until it goes; reading it allocates the reader's
        // own rows, so its answers are read from a second one, after the measurement.
        let (charged, excess) = measure::measure_within(&budget, || {
            let view = snapshot
                .with_inferred_added_within(&triples, &budget)
                .unwrap();
            let charged = budget.used();
            drop(view);
            charged
        });
        let left = budget.used();
        let got = all(&snapshot
            .with_inferred_added_within(&triples, &budget)
            .unwrap());
        if got != expected {
            failed.push(format!(
                "{added}: {} statements, expected {}",
                got.len(),
                expected.len()
            ));
        }
        if excess != 0 || charged == 0 || left != 0 {
            failed.push(format!(
                "{added}: {excess} bytes beyond the charge, {charged} charged, {left} left"
            ));
        }
    }
    assert!(failed.is_empty(), "{failed:#?}");
}

/// A budget that can't take the view refuses it and keeps nothing charged, at each size.
#[test]
fn a_refused_inferred_addition_keeps_nothing() {
    if !measure::alone() {
        return;
    }
    let mut failed = Vec::new();
    for added in [100, 20_000] {
        let (engine, triples) = engine(added);
        let snapshot = engine.snapshot();
        let budget = Arc::new(Budget::new(4096));
        let (refused, excess) = measure::measure_within(&budget, || {
            snapshot
                .with_inferred_added_within(&triples, &budget)
                .is_err()
        });
        if !refused || excess != 0 || budget.used() != 0 {
            failed.push(format!(
                "{added}: refused {refused}, {excess} bytes beyond, {} left",
                budget.used()
            ));
        }
    }
    assert!(failed.is_empty(), "{failed:#?}");
}
