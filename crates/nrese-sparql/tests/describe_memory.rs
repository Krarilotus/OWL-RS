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
