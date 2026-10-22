//! Guard of a charged lookup's and decode's memory across a checkpoint (Sol's review of
//! b967b63c): their bound and the read it bounds meet one state of the dictionary, though a
//! snapshot shares the dictionary and a checkpoint publishes a compressed base under it.
//! Measured against the charging budget (`nrese_exec::measure`), so this file is a test
//! binary of its own.

use nrese_engine::{Engine, EngineConfig, VocabularyEncoding};
use nrese_exec::{Budget, Reservation, measure};
use nrese_rdf::{GraphName, Literal, NamedNode, Quad, Term, Triple};

#[global_allocator]
static ALLOCATOR: measure::Measured<std::alloc::System> = measure::Measured(std::alloc::System);

/// A 4 KiB IRI and a 4 KiB literal committed while the keys are plain, a snapshot taken, a
/// checkpoint then publishing their keys compressed: that snapshot's charged lookup of the
/// IRI and decode of the literal hold no more than they charged, and give what was committed.
/// (The lookup's bound once came from a state of the dictionary before the checkpoint and its
/// read from the one after: 12,303 bytes charged, 16,402 held.)
#[test]
fn a_read_charged_across_a_checkpoint_holds_no_more_than_its_charge() {
    if !measure::alone() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let config = EngineConfig {
        background_maintenance: false,
        vocabulary: VocabularyEncoding::Fsst,
        ..EngineConfig::default()
    };
    let engine = Engine::open(dir.path(), config).unwrap();
    let iri: Term = NamedNode::new_unchecked(format!("urn:{}", "x".repeat(4096))).into();
    let literal: Term = Literal::new_simple_literal("y".repeat(4096)).into();
    let mut tx = engine.transaction();
    let (iri_id, literal_id) = (tx.intern(iri.as_ref()), tx.intern(literal.as_ref()));
    let subject = NamedNode::new_unchecked("urn:s");
    let p = NamedNode::new_unchecked("urn:p");
    for object in [&iri, &literal] {
        let quad = Quad::new(
            subject.clone(),
            p.clone(),
            object.clone(),
            GraphName::DefaultGraph,
        );
        tx.insert(quad.as_ref());
    }
    tx.commit().unwrap();
    let snapshot = engine.snapshot();
    engine.checkpoint().unwrap();
    let budget = Budget::unlimited();
    let (found, excess) = measure::measure_within(&budget, || {
        snapshot.lookup_charged(iri.as_ref(), |bytes| budget.charge(bytes))
    });
    assert_eq!(found, Ok(Some(iri_id)));
    assert_eq!(excess, 0, "a lookup held {excess} bytes beyond its charge");
    budget.release(budget.used());
    let (decoded, excess) = measure::measure_within(&budget, || {
        snapshot.decode_charged(literal_id, |bytes| budget.charge(bytes))
    });
    assert_eq!(decoded, Ok(Some(literal)));
    assert_eq!(excess, 0, "a decode held {excess} bytes beyond its charge");
}

/// The complete operation uses the held snapshot's shared dictionary after a compressed
/// rebase, including recursively decoded objects. Both temporary realloc overlap and
/// retained output count at every allocation/free event, not just at function return.
#[test]
fn complete_decode_compressed_snapshot_preserves_values_and_existing_reservation() {
    if !measure::alone() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let engine = Engine::open(
        dir.path(),
        EngineConfig {
            background_maintenance: false,
            vocabulary: VocabularyEncoding::Fsst,
            ..EngineConfig::default()
        },
    )
    .unwrap();
    let atomic: Term =
        Literal::new_simple_literal("compressed long lexical value ".repeat(4096)).into();
    let mut triple = atomic.clone();
    for _ in 0..4 {
        triple = Triple::new(
            NamedNode::new_unchecked("urn:s"),
            NamedNode::new_unchecked("urn:p"),
            triple,
        )
        .into();
    }
    let mut tx = engine.transaction();
    let ids = [tx.intern(atomic.as_ref()), tx.intern(triple.as_ref())];
    tx.insert(
        Quad::new(
            NamedNode::new_unchecked("urn:s"),
            NamedNode::new_unchecked("urn:p"),
            triple.clone(),
            GraphName::DefaultGraph,
        )
        .as_ref(),
    );
    tx.commit().unwrap();
    let snapshot = engine.snapshot();
    engine.checkpoint().unwrap();
    for (id, expected) in ids.into_iter().zip([atomic, triple]) {
        assert_eq!(snapshot.decode(id).as_ref(), Some(&expected));
        let budget = Budget::unlimited();
        let ((term, prior, held), excess) = measure::measure_within(&budget, || {
            let mut held = Reservation::new(&budget);
            held.charge(37).unwrap();
            let prior = vec![0_u8; 37];
            let term = snapshot.decode_within(id, &mut held).unwrap().unwrap();
            (term, prior, held)
        });
        assert_eq!(term, expected);
        assert_eq!(excess, 0, "compressed scratch/retained overlap");
        assert_eq!(held.bytes(), 37 + term.heap_bytes());
        println!(
            "compressed_decode retained={} budget_peak={}",
            term.heap_bytes(),
            budget.peak()
        );
        drop((term, prior, held));
        assert_eq!(budget.used(), 0);

        let budget = Budget::new(37);
        let ((result, held), allocated) = measure::measure(|| {
            let mut held = Reservation::new(&budget);
            held.charge(37).unwrap();
            (snapshot.decode_within(id, &mut held), held)
        });
        assert!(result.is_err());
        assert_eq!(allocated, 0);
        assert_eq!(held.bytes(), 37);
        drop(held);
        assert_eq!(budget.used(), 0);
    }
}
