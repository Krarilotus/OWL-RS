//! A transfer (`ADD`, and the copy step of `COPY` and `MOVE`) draws what it adds to the
//! transaction's pending changes from the request's memory envelope before it allocates, and
//! that charge lasts as long as the pending changes do (SYN-54 review r1, F1): an envelope
//! smaller than the copy refuses the update with the typed memory error, nothing is committed,
//! and the charge is gone once the transaction is.
//!
//! Sizes come from the data, not from the implementation: `N` distinct statements take at least
//! `N` encoded quads of 32 bytes in any set that holds them (`nrese_engine::EncodedQuad`).

use std::sync::Arc;

use nrese_engine::{EncodedQuad, Engine, EngineConfig, QuadPattern, ReadModel, Transaction};
use nrese_exec::Envelope;
use nrese_rdf::{Literal, NamedNode, Quad};
use nrese_sparql::{QueryEvaluationError, Resources, UpdateError, UpdateOptions, apply_update};
use nrese_sparql_syntax::SparqlParser;

const EX: &str = "http://example.com/";
const N: usize = 20_000;

fn ex(name: &str) -> NamedNode {
    NamedNode::new_unchecked(format!("{EX}{name}"))
}

/// `N` distinct statements in each of `<a>` and `<b>`, committed.
fn store() -> Engine {
    let engine = Engine::new(EngineConfig::default()).unwrap();
    let mut tx = engine.transaction();
    for graph in ["a", "b"] {
        for i in 0..N {
            let o = Literal::new_simple_literal(i.to_string());
            tx.insert(Quad::new(ex(&format!("s{i}")), ex("p"), o, ex(graph)).as_ref());
        }
    }
    tx.commit().unwrap();
    engine
}

/// A transaction whose pending changes draw from `envelope`, as the mutation pipeline opens
/// one for a request.
fn charged<'e>(engine: &'e Engine, envelope: &Envelope) -> Transaction<'e> {
    let mut tx = engine.transaction();
    tx.charge_pending_to(Arc::new(envelope.part())).unwrap();
    tx
}

fn within(envelope: &Envelope) -> UpdateOptions {
    UpdateOptions {
        resources: Resources {
            memory: envelope.clone(),
            ..Resources::default()
        },
        ..UpdateOptions::default()
    }
}

/// The asserted statements of the committed store, and its revision.
fn committed(engine: &Engine) -> (u64, u64) {
    let snapshot = engine.snapshot();
    let all = snapshot
        .quads_for_pattern_in(ReadModel::Asserted, &QuadPattern::all())
        .count() as u64;
    (all, snapshot.revision())
}

fn update(text: &str) -> nrese_sparql_syntax::Update {
    SparqlParser::new()
        .parse_update(&text.replace("<ex:", &format!("<{EX}")))
        .unwrap()
}

fn refused_for_memory(result: Result<(), UpdateError>) -> bool {
    matches!(
        result,
        Err(UpdateError::Evaluation(QueryEvaluationError::MemoryLimit(
            _
        )))
    )
}

/// An envelope of `N` encoded quads' bytes can't hold the `N` new pending statements of
/// `ADD <a> TO <c>`: refused, nothing committed, nothing left charged.
#[test]
fn a_transfer_larger_than_its_envelope_is_refused() {
    let engine = store();
    let before = committed(&engine);
    let envelope = Envelope::new(Some(N * size_of::<EncodedQuad>()), None);
    let mut tx = charged(&engine, &envelope);
    let result = apply_update(&mut tx, &update("ADD <ex:a> TO <ex:c>"), &within(&envelope));
    drop(tx);
    assert!(
        refused_for_memory(result),
        "an oversized transfer is refused"
    );
    assert_eq!(committed(&engine), before, "nothing is committed");
    assert_eq!(
        envelope.used(),
        0,
        "nothing stays charged once the transaction is gone"
    );
}

/// The pending statements of an earlier transfer stay charged through the request: two
/// transfers that each fit the envelope alone are refused together, at the second; a dropped
/// or committed transaction leaves nothing charged.
#[test]
fn a_transfers_charge_lasts_as_long_as_its_pending_statements() {
    let engine = store();
    // One transfer alone: what it holds while its transaction lives, and its peak.
    let alone = Envelope::new(None, None);
    let mut tx = charged(&engine, &alone);
    apply_update(&mut tx, &update("ADD <ex:a> TO <ex:c>"), &within(&alone)).unwrap();
    let held = alone.used();
    assert!(
        held >= N * size_of::<EncodedQuad>(),
        "the pending statements are charged while they live: {held} bytes for {N} quads"
    );
    drop(tx);
    assert_eq!(alone.used(), 0, "a dropped transaction releases its charge");
    let fits_one = alone.peak();

    let before = committed(&engine);
    let envelope = Envelope::new(Some(fits_one), None);
    let mut tx = charged(&engine, &envelope);
    let first = apply_update(&mut tx, &update("ADD <ex:a> TO <ex:c>"), &within(&envelope));
    assert!(first.is_ok(), "one transfer fits: {first:?}");
    let second = apply_update(&mut tx, &update("ADD <ex:b> TO <ex:d>"), &within(&envelope));
    assert!(
        refused_for_memory(second),
        "the second transfer is refused while the first's statements are pending"
    );
    drop(tx);
    assert_eq!(committed(&engine), before, "nothing is committed");
    assert_eq!(envelope.used(), 0, "nothing stays charged");

    // Committed, the pending statements are gone, and so is their charge.
    let envelope = Envelope::new(Some(fits_one), None);
    let mut tx = charged(&engine, &envelope);
    apply_update(&mut tx, &update("ADD <ex:a> TO <ex:c>"), &within(&envelope)).unwrap();
    tx.commit().unwrap();
    assert_eq!(envelope.used(), 0, "a commit releases the charge");
    assert_eq!(committed(&engine).0, before.0 + N as u64);
}
