//! Guard of a charged lookup's and decode's memory across a checkpoint (Sol's review of
//! b967b63c): their bound and the read it bounds meet one state of the dictionary, though a
//! snapshot shares the dictionary and a checkpoint publishes a compressed base under it.
//! Measured against the charging budget (`nrese_exec::measure`), so this file is a test
//! binary of its own.

use nrese_engine::{Engine, EngineConfig, VocabularyEncoding, set_vocabulary_encoding};
use nrese_exec::{Budget, measure};
use nrese_rdf::{GraphName, Literal, NamedNode, Quad, Term};

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
    set_vocabulary_encoding(VocabularyEncoding::Fsst);
    let dir = tempfile::tempdir().unwrap();
    let config = EngineConfig {
        background_maintenance: false,
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
