//! What an update holds in proportion to its data is charged to the request's memory envelope
//! before it is allocated, and that charge lasts as long as what it pays for (D16-039): the
//! statements `CLEAR`/`DROP` and `DELETE` retract, held in the transaction's pending changes;
//! the quads a `DELETE`/`INSERT … WHERE` fills from its solutions, decoded; and the statements
//! `INSERT DATA` adds. An envelope smaller than that refuses the update with the typed memory
//! error, nothing is committed, and nothing stays charged once the transaction is gone.
//!
//! Sizes come from the data, not from the implementation: `N` distinct statements take at least
//! `N` encoded quads of 32 bytes in any set that holds them (`nrese_engine::EncodedQuad`), and
//! a quad decoded from a statement holds at least its literal's text.

use std::sync::Arc;

use nrese_engine::{EncodedQuad, Engine, EngineConfig, QuadPattern, ReadModel, Transaction};
use nrese_exec::Envelope;
use nrese_rdf::{Literal, NamedNode, Quad};
use nrese_sparql::{QueryEvaluationError, Resources, UpdateError, UpdateOptions, apply_update};
use nrese_sparql_syntax::SparqlParser;

const EX: &str = "http://example.com/";
/// Statements per graph in the many-statements store.
const N: usize = 20_000;
/// Statements and literal size in the large-literals store: 8 MiB of literal text.
const LARGE: usize = 128;
const LITERAL: usize = 64 << 10;

fn ex(name: &str) -> NamedNode {
    NamedNode::new_unchecked(format!("{EX}{name}"))
}

/// `N` distinct statements in each of `<a>` and `<b>`, committed.
fn many() -> Engine {
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

/// `LARGE` statements in `<a>`, each with its own literal of `LITERAL` bytes, committed.
fn large() -> Engine {
    let engine = Engine::new(EngineConfig::default()).unwrap();
    let mut tx = engine.transaction();
    for i in 0..LARGE {
        let o = Literal::new_simple_literal(format!("{i:08}{}", "x".repeat(LITERAL - 8)));
        tx.insert(Quad::new(ex(&format!("s{i}")), ex("p"), o, ex("a")).as_ref());
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

fn refused_for_memory(result: &Result<(), UpdateError>) -> bool {
    matches!(
        result,
        Err(UpdateError::Evaluation(QueryEvaluationError::MemoryLimit(
            _
        )))
    )
}

/// `text` on `engine` under an envelope of `limit` bytes is refused for memory, nothing is
/// committed, and nothing stays charged once its transaction is dropped.
fn assert_refused(engine: &Engine, text: &str, limit: usize) {
    let before = committed(engine);
    let envelope = Envelope::new(Some(limit), None);
    let mut tx = charged(engine, &envelope);
    let result = apply_update(&mut tx, &update(text), &within(&envelope));
    drop(tx);
    assert!(
        refused_for_memory(&result),
        "`{text}` under {limit} bytes is refused for memory: {result:?}"
    );
    assert_eq!(committed(engine), before, "`{text}`: nothing is committed");
    assert_eq!(
        envelope.used(),
        0,
        "`{text}`: nothing stays charged once the transaction is gone"
    );
}

/// An envelope of `N` encoded quads' bytes can't hold the `N` pending retractions of clearing
/// or dropping a graph of `N` statements, by name or with every graph (`ALL`, `NAMED`).
#[test]
fn a_clear_larger_than_its_envelope_is_refused() {
    let engine = many();
    let limit = N * size_of::<EncodedQuad>();
    for text in [
        "CLEAR GRAPH <ex:a>",
        "DROP GRAPH <ex:a>",
        "CLEAR NAMED",
        "DROP ALL",
    ] {
        assert_refused(&engine, text, limit);
    }
}

/// The same for the `N` pending retractions of a `DELETE WHERE` over that graph.
#[test]
fn a_delete_larger_than_its_envelope_is_refused() {
    let engine = many();
    let limit = N * size_of::<EncodedQuad>();
    assert_refused(&engine, "DELETE WHERE { GRAPH <ex:a> { ?s ?p ?o } }", limit);
}

/// The pending retractions of a `CLEAR` stay charged through the request: two that each fit
/// the envelope alone are refused together, at the second; a dropped or committed transaction
/// leaves nothing charged.
#[test]
fn a_clears_charge_lasts_as_long_as_its_pending_retractions() {
    let engine = many();
    // One clear alone: what it holds while its transaction lives, and its peak.
    let alone = Envelope::new(None, None);
    let mut tx = charged(&engine, &alone);
    apply_update(&mut tx, &update("CLEAR GRAPH <ex:a>"), &within(&alone)).unwrap();
    let held = alone.used();
    assert!(
        held >= N * size_of::<EncodedQuad>(),
        "the pending retractions are charged while they live: {held} bytes for {N} quads"
    );
    drop(tx);
    assert_eq!(alone.used(), 0, "a dropped transaction releases its charge");
    let fits_one = alone.peak();

    let before = committed(&engine);
    let envelope = Envelope::new(Some(fits_one), None);
    let mut tx = charged(&engine, &envelope);
    let first = apply_update(&mut tx, &update("CLEAR GRAPH <ex:a>"), &within(&envelope));
    assert!(first.is_ok(), "one clear fits: {first:?}");
    let second = apply_update(&mut tx, &update("CLEAR GRAPH <ex:b>"), &within(&envelope));
    assert!(
        refused_for_memory(&second),
        "the second clear is refused while the first's retractions are pending: {second:?}"
    );
    drop(tx);
    assert_eq!(committed(&engine), before, "nothing is committed");
    assert_eq!(envelope.used(), 0, "nothing stays charged");

    // Committed, the pending retractions are gone, and so is their charge.
    let envelope = Envelope::new(Some(fits_one), None);
    let mut tx = charged(&engine, &envelope);
    apply_update(&mut tx, &update("CLEAR GRAPH <ex:a>"), &within(&envelope)).unwrap();
    tx.commit().unwrap();
    assert_eq!(envelope.used(), 0, "a commit releases the charge");
    assert_eq!(committed(&engine).0, before.0 - N as u64);
}

/// The quads a `DELETE … WHERE` or `INSERT … WHERE` fills from `LARGE` solutions hold each
/// solution's `LITERAL`-byte literal, decoded: 8 MiB can't be held in 128 KiB. The solutions
/// themselves are `LARGE` rows of ids, and the pending changes `LARGE` encoded quads, which fit.
#[test]
fn decoded_update_quads_larger_than_their_envelope_are_refused() {
    let engine = large();
    let limit = 128 << 10;
    assert!(LARGE * LITERAL >= 64 * limit);
    for text in [
        "DELETE { GRAPH <ex:a> { ?s ?p ?o } } WHERE { GRAPH <ex:a> { ?s ?p ?o } }",
        "INSERT { GRAPH <ex:c> { ?s ?p ?o } } WHERE { GRAPH <ex:a> { ?s ?p ?o } }",
    ] {
        assert_refused(&engine, text, limit);
    }
}

/// Accepted, the decoded quads are charged while they are held: the envelope's peak covers
/// their literal text, and once applied and dropped nothing of them stays charged. The update
/// does what it says.
#[test]
fn decoded_update_quads_are_charged_while_held() {
    let engine = large();
    let envelope = Envelope::new(None, None);
    let mut tx = charged(&engine, &envelope);
    let text = "DELETE { GRAPH <ex:a> { ?s ?p ?o } } WHERE { GRAPH <ex:a> { ?s ?p ?o } }";
    apply_update(&mut tx, &update(text), &within(&envelope)).unwrap();
    assert!(
        envelope.peak() >= LARGE * LITERAL,
        "the decoded quads' literals are charged while held: peak {} bytes for {} bytes",
        envelope.peak(),
        LARGE * LITERAL
    );
    assert!(
        envelope.used() < LARGE * LITERAL,
        "applied, the decoded quads and their charge are gone: {} bytes still charged",
        envelope.used()
    );
    tx.commit().unwrap();
    assert_eq!(envelope.used(), 0, "a commit releases the charge");
    assert_eq!(committed(&engine).0, 0, "every statement is deleted");
}

/// Guard (and the A/B instruction-count workload): on a transaction charged to an ample
/// envelope, each update form this module charges does what it says, statement counts after
/// each from the data, and a commit leaves nothing charged.
#[test]
fn guard_charged_updates_do_what_they_say() {
    let engine = many();
    let envelope = Envelope::new(None, None);
    let mut tx = charged(&engine, &envelope);
    let n = N as u64;
    let data: String = (0..N)
        .map(|i| format!("<ex:t{i}> <ex:p> \"{i}\" . "))
        .collect();
    let steps = [
        (
            format!("INSERT DATA {{ GRAPH <ex:c> {{ {data} }} }}"),
            3 * n,
        ),
        (
            "DELETE { GRAPH <ex:a> { ?s ?p ?o } } INSERT { GRAPH <ex:d> { ?s ?p ?o } } \
             WHERE { GRAPH <ex:a> { ?s ?p ?o } }"
                .to_owned(),
            3 * n,
        ),
        ("CLEAR GRAPH <ex:b>".to_owned(), 2 * n),
        (
            "DELETE DATA { GRAPH <ex:c> { <ex:t0> <ex:p> \"0\" } }".to_owned(),
            2 * n - 1,
        ),
        ("DROP ALL".to_owned(), 0),
    ];
    for (text, after) in steps {
        apply_update(&mut tx, &update(&text), &within(&envelope)).unwrap();
        assert_eq!(tx.len_in(ReadModel::Asserted), after, "after `{text:.60}`");
    }
    tx.commit().unwrap();
    assert_eq!(envelope.used(), 0, "a commit releases the charge");
    assert_eq!(committed(&engine).0, 0, "everything is dropped");
}

/// A `WHERE` after an earlier operation of the request reads a snapshot of the pending state,
/// which holds the `N` pending statements again: it is charged to the request's envelope (DUR-6's
/// charged builder), and refused as the request's memory error where it doesn't fit. Its second
/// operation matches nothing, so the snapshot is all it holds.
#[test]
fn a_where_clauses_pending_snapshot_is_charged_to_the_request() {
    let engine = Engine::new(EngineConfig::default()).unwrap();
    let data: String = (0..N)
        .map(|i| format!("<ex:s{i}> <ex:p> \"{i}\" . "))
        .collect();
    let insert = format!("INSERT DATA {{ GRAPH <ex:c> {{ {data} }} }}");
    let both =
        format!("{insert} ; DELETE {{ ?s ?p ?o }} WHERE {{ GRAPH <ex:c> {{ ?s <ex:none> ?o }} }}");
    // Each alone, with memory to spare: what the first holds at its peak, and whether the
    // second holds more.
    let peak = |text: &str| {
        let envelope = Envelope::new(None, None);
        let mut tx = charged(&engine, &envelope);
        apply_update(&mut tx, &update(text), &within(&envelope)).unwrap();
        envelope.peak()
    };
    let (first, request) = (peak(&insert), peak(&both));
    assert!(
        request >= first + N * size_of::<EncodedQuad>(),
        "the snapshot of {N} pending statements is charged: peak {request} bytes, \
         {first} for the first operation alone"
    );
    // The first operation's peak fits; the snapshot doesn't.
    assert_refused(&engine, &both, first);
}

/// `INSERT DATA` of `N` new statements needs room for `N` pending inserts: an envelope of `N`
/// encoded quads' bytes can't hold it. The update text is parsed before it is applied, outside
/// the envelope, as the store does.
#[test]
fn an_insert_data_larger_than_its_envelope_is_refused() {
    let engine = Engine::new(EngineConfig::default()).unwrap();
    let data: String = (0..N)
        .map(|i| format!("<ex:s{i}> <ex:p> \"{i}\" . "))
        .collect();
    assert_refused(
        &engine,
        &format!("INSERT DATA {{ GRAPH <ex:a> {{ {data} }} }}"),
        N * size_of::<EncodedQuad>(),
    );
}
