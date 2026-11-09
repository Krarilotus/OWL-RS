//! D16-039 retained-DESCRIBE witness from unowned-sweep-1109-final.
use nrese_engine::Engine;
use nrese_exec::{Envelope, SharedBudget};
use nrese_rdf::{GraphName, Literal, NamedNode, Quad};
use nrese_sparql::{QueryEvaluationError, QueryOptions, QueryResults, Resources, evaluate_query};
use nrese_sparql_syntax::SparqlParser;

fn data() -> Engine {
    let engine = Engine::new(Default::default()).unwrap();
    let mut tx = engine.transaction();
    for i in 0..128 {
        let quad = Quad::new(
            NamedNode::new_unchecked("urn:a"),
            NamedNode::new_unchecked(format!("urn:p{i}")),
            Literal::new_simple_literal("x".repeat(65536)),
            GraphName::DefaultGraph,
        );
        tx.insert(quad.as_ref());
    }
    tx.commit().unwrap();
    engine
}

// Spec: Resources::memory bounds retained decoded graph results (D16-039).
// The original witness retains 128 * 64 KiB under a 128 KiB envelope.
#[test]
fn d039_describe_cannot_retain_megabytes_outside_128k_envelope() {
    let engine = data();
    let snapshot = engine.snapshot();
    let query = SparqlParser::new().parse_query("DESCRIBE <urn:a>").unwrap();
    let shared = SharedBudget::new(128 << 10);
    let options = QueryOptions {
        resources: Resources {
            memory: Envelope::new(Some(128 << 10), Some(shared.clone())),
            ..Default::default()
        },
        ..Default::default()
    };
    let got = evaluate_query(&snapshot, &query, &options);
    assert!(
        matches!(got, Err(QueryEvaluationError::MemoryLimit(_))),
        "must refuse its retained decoded result"
    );
    assert_eq!(
        shared.used(),
        0,
        "refusal releases the result's reservation"
    );
}

// Spec: an accepted graph retains its reservation until delivery is dropped (D16-039).
#[test]
fn describe_retains_its_output_charge_until_drop() {
    let engine = data();
    let snapshot = engine.snapshot();
    let query = SparqlParser::new().parse_query("DESCRIBE <urn:a>").unwrap();
    let shared = SharedBudget::new(32 << 20);
    let options = QueryOptions {
        resources: Resources {
            memory: Envelope::new(None, Some(shared.clone())),
            ..Default::default()
        },
        ..Default::default()
    };
    let result = evaluate_query(&snapshot, &query, &options).unwrap();
    assert!(
        shared.used() >= 128 * 65536,
        "decoded payloads stay charged"
    );
    let QueryResults::Graph(rows) = result else {
        panic!("DESCRIBE returns a graph")
    };
    assert_eq!(rows.map(Result::unwrap).count(), 128);
    assert_eq!(
        shared.used(),
        0,
        "dropping delivery releases its reservation"
    );
}

/// `n` statements about `<urn:a>`, each with its own literal of `len` bytes.
fn distinct(n: usize, len: usize) -> Engine {
    let engine = Engine::new(Default::default()).unwrap();
    let mut tx = engine.transaction();
    for i in 0..n {
        let quad = Quad::new(
            NamedNode::new_unchecked("urn:a"),
            NamedNode::new_unchecked(format!("urn:p{i}")),
            Literal::new_simple_literal(format!("{i:08}{}", "x".repeat(len - 8))),
            GraphName::DefaultGraph,
        );
        tx.insert(quad.as_ref());
    }
    tx.commit().unwrap();
    engine
}

// Spec: what a query holds is charged while held (D16-039). DESCRIBE over 128 distinct
// 64 KiB literals holds each twice while it runs: in its output, and decoded once for reuse
// (the decode cache); the charge's peak covers both copies.
#[test]
fn describe_charges_the_terms_it_keeps_decoded() {
    let engine = distinct(128, 65536);
    let snapshot = engine.snapshot();
    let query = SparqlParser::new().parse_query("DESCRIBE <urn:a>").unwrap();
    let envelope = Envelope::new(None, None);
    let options = QueryOptions {
        resources: Resources {
            memory: envelope.clone(),
            ..Default::default()
        },
        ..Default::default()
    };
    let result = evaluate_query(&snapshot, &query, &options).unwrap();
    assert!(
        envelope.peak() >= 2 * 128 * 65536,
        "both copies of the literals are charged: peak {} bytes for {} bytes each",
        envelope.peak(),
        128 * 65536
    );
    drop(result);
    assert_eq!(envelope.used(), 0, "dropped, nothing stays charged");
}

// Spec (D16-039, sol-loop r1 S2): a decode cache gives way to what the answer must hold.
// DESCRIBE of one statement with a 1 MiB literal under a 1.5 MiB envelope holds the literal
// once in its output, which fits; a second copy kept for reuse would not, so it isn't kept.
#[test]
fn a_cached_term_never_crowds_out_the_answer() {
    let engine = distinct(1, 1 << 20);
    let snapshot = engine.snapshot();
    let query = SparqlParser::new().parse_query("DESCRIBE <urn:a>").unwrap();
    let options = QueryOptions {
        resources: Resources {
            memory: Envelope::new(Some(3 << 19), None),
            ..Default::default()
        },
        ..Default::default()
    };
    let QueryResults::Graph(rows) = evaluate_query(&snapshot, &query, &options).unwrap() else {
        panic!("DESCRIBE returns a graph")
    };
    assert_eq!(rows.map(Result::unwrap).count(), 1);
}

// Spec (D16-039, sol-loop r2 S2): a term kept early gives way once the answer needs its room.
// DESCRIBE of three statements with distinct 1 MiB literals under a 3.5 MiB envelope holds
// the three in its output, which fits; the first literal, kept for reuse while there was
// room, must not stay kept once the output needs that room.
#[test]
fn a_term_kept_early_gives_way_to_the_answer() {
    let engine = distinct(3, 1 << 20);
    let snapshot = engine.snapshot();
    let query = SparqlParser::new().parse_query("DESCRIBE <urn:a>").unwrap();
    let options = QueryOptions {
        resources: Resources {
            memory: Envelope::new(Some(7 << 19), None),
            ..Default::default()
        },
        ..Default::default()
    };
    let QueryResults::Graph(rows) = evaluate_query(&snapshot, &query, &options).unwrap() else {
        panic!("DESCRIBE returns a graph")
    };
    assert_eq!(rows.map(Result::unwrap).count(), 3);
}

// Guard (D16-039): a decode cache is never needed for an answer. A filter over 16,384
// distinct 1 KiB literals (16 MiB of text, decoded to be tested) under a 4 MiB envelope, far
// more than its rows of ids need, still answers, every row.
#[test]
fn guard_a_filter_whose_decoded_terms_dont_fit_still_answers() {
    let engine = distinct(16_384, 1024);
    let snapshot = engine.snapshot();
    let query = SparqlParser::new()
        .parse_query("SELECT ?p WHERE { <urn:a> ?p ?o FILTER(STRLEN(?o) > 1000) }")
        .unwrap();
    let options = QueryOptions {
        resources: Resources {
            memory: Envelope::new(Some(4 << 20), None),
            ..Default::default()
        },
        ..Default::default()
    };
    let QueryResults::Solutions(rows) = evaluate_query(&snapshot, &query, &options).unwrap() else {
        panic!("solutions")
    };
    assert_eq!(rows.map(Result::unwrap).count(), 16_384);
}
