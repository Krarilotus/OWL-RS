//! Guard of an equality expansion's memory (equality by representatives, early expansion):
//! the copies a sorted read holds until they are in order are charged to its reader's
//! budget before they are held, and released once freed. Measured against that budget
//! (`nrese_exec::measure`), so this file is a test binary of its own.

use std::sync::Arc;

use nrese_engine::{EncodedTriple, Engine, EngineConfig, QuadPattern, ReadModel, Snapshot};
use nrese_exec::{Budget, Reservation, measure};
use nrese_rdf::{NamedNode, Quad};

#[global_allocator]
static ALLOCATOR: measure::Measured<std::alloc::System> = measure::Measured(std::alloc::System);

fn node(name: &str) -> NamedNode {
    NamedNode::new_unchecked(format!("urn:{name}"))
}

/// 20 statements about `rep` in the default graph, `rep` a class of `size` identities
/// (each other one `alias sameAs rep`), read with expansion. The classes are made here,
/// before any measurement: they are the version's, charged nowhere.
fn snapshot(size: usize) -> Snapshot {
    let config = EngineConfig {
        background_maintenance: false,
        ..EngineConfig::default()
    };
    let engine = Engine::new(config).unwrap();
    let mut tx = engine.transaction();
    let rep = tx.intern(node("rep").as_ref().into());
    let same_as = tx.intern(node("sameAs").as_ref().into());
    for i in 1..size {
        let alias = tx.intern(node(&format!("alias{i}")).as_ref().into());
        tx.insert_inferred(EncodedTriple {
            subject: alias,
            predicate: same_as,
            object: rep,
        });
    }
    for i in 0..20 {
        let object = node(&format!("o{i}"));
        tx.insert(
            Quad::new(
                node("rep"),
                node("p"),
                object,
                nrese_rdf::GraphName::DefaultGraph,
            )
            .as_ref(),
        );
    }
    tx.commit().unwrap();
    engine.set_equality(Some(same_as));
    let snapshot = engine.snapshot();
    snapshot.count_in(ReadModel::Materialised, &QuadPattern::all());
    snapshot
}

/// A read of every statement in subject order holds the copies of the representative's
/// statements for its later identities until the last of them is read: as many as the class
/// has identities, times 20. At each class size the read holds no more than it charged (its
/// scan's working memory reserved first, as a charged reader does, the pending copies by the
/// expansion), and a budget too small for the copies ends the read with its refusal kept.
#[test]
fn an_expansion_holds_no_more_than_it_charges() {
    if !measure::alone() {
        return;
    }
    for size in [1, 10, 100, 1_000] {
        let snapshot = snapshot(size);
        let budget = Arc::new(Budget::unlimited());
        let reader = snapshot.reading_within(Arc::clone(&budget));
        let (read, excess) = measure::measure_within(&budget, || {
            let mut scan = Reservation::new(&*budget);
            scan.charge(reader.scan_bytes(ReadModel::Materialised))
                .expect("an unlimited budget");
            let read = reader
                .quads_for_pattern_in(ReadModel::Materialised, &QuadPattern::all())
                .count();
            assert!(reader.read_refused().is_none());
            read
        });
        assert!(read >= 20 * size, "{size}: {read} statements read");
        assert_eq!(
            excess, 0,
            "{size} identities: {excess} bytes beyond the charge"
        );
        assert_eq!(budget.used(), 0, "{size} identities: charge left");
    }
    let snapshot = snapshot(1_000);
    let budget = Arc::new(Budget::new(1024));
    let reader = snapshot.reading_within(Arc::clone(&budget));
    let read = reader
        .quads_for_pattern_in(ReadModel::Materialised, &QuadPattern::all())
        .count();
    assert!(
        reader.read_refused().is_some(),
        "a read of {read} statements in 1,024 bytes"
    );
    assert_eq!(budget.used(), 0);
}
