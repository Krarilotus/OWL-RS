//! What a vector search holds against what it charges (SRCH review r1, R1-C1), under this
//! test binary's counting allocator ([`nrese_exec::measure`]): the search widens its `k`
//! towards the whole space while its hits fail the query's checks, and every hit list and
//! heap that widening grows is charged to the query's budget before it is allocated.

use crate::{QueryEvaluationError, QueryOptions, QueryResults, evaluate_query};
use nrese_engine::vector::{CHUNK, DATATYPE, lexical};
use nrese_engine::{Engine, EngineConfig};
use nrese_exec::measure::{alone, measure_within};
use nrese_exec::{Envelope, SharedBudget};
use nrese_rdf::{GraphName, Literal, NamedNode, Quad};
use nrese_sparql_syntax::SparqlParser;

fn node(iri: &str) -> NamedNode {
    NamedNode::new_unchecked(iri)
}

/// Bytes of a hit as a scan holds it: a term id and a distance, padded.
const HIT_BYTES: usize = 16;

/// The reviewer's witness store: `n` distinct one-dimensional vector literals in a graph
/// the query doesn't read, and one ordinary statement in the graph it does. Without
/// background maintenance: every thread counts in a measurement.
fn store(n: usize) -> Engine {
    let engine = Engine::new(EngineConfig {
        background_maintenance: false,
        ..EngineConfig::default()
    })
    .unwrap();
    let mut tx = engine.transaction();
    for i in 0..n {
        let vector = Literal::new_typed_literal(lexical(&[i as f32 + 1.0]), node(DATATYPE));
        tx.insert(
            Quad::new(
                node(&format!("urn:item/{i}")),
                node("urn:embedding"),
                vector,
                GraphName::from(node("urn:hidden")),
            )
            .as_ref(),
        );
    }
    tx.insert(
        Quad::new(
            node("urn:a"),
            node("urn:p"),
            node("urn:b"),
            GraphName::from(node("urn:visible")),
        )
        .as_ref(),
    );
    tx.commit().unwrap();
    engine
}

/// The ten nearest to `[0]` among the vectors the visible graph uses: none.
const WITNESS: &str = "PREFIX nrv: <urn:nrese:vector:> \
    SELECT ?v FROM <urn:visible> WHERE { \
      SERVICE nrv:search { ?v nrv:near \"[0]\"^^nrv:vector ; nrv:k 10 ; nrv:exact true } }";

/// The witness's rows under `memory`, or its error.
fn witness(engine: &Engine, memory: Envelope) -> Result<usize, QueryEvaluationError> {
    let query = SparqlParser::new().parse_query(WITNESS).unwrap();
    let mut options = QueryOptions::default();
    options.resources.memory = memory;
    let snapshot = engine.snapshot();
    let QueryResults::Solutions(solutions) = evaluate_query(&snapshot, &query, &options)? else {
        panic!("solutions")
    };
    let mut rows = 0;
    for solution in solutions {
        solution?;
        rows += 1;
    }
    Ok(rows)
}

/// The witness at 2 and at 16 chunks of hidden vectors: no rows either way, and the bytes it
/// holds beyond what it charged are the same at both sizes (the plan's and the result's
/// bookkeeping, fixed per query). At 16 chunks a search that held its widened round
/// uncharged would hold at least 14 chunks of hits more than at 2. The vector index is
/// built by a first run outside the measurement: it is the store's derived state, not the
/// query's.
#[test]
fn a_widened_vector_search_holds_no_more_than_it_charges() {
    if !alone() {
        return;
    }
    let excess = |n: usize| -> usize {
        let engine = store(n);
        assert_eq!(witness(&engine, Envelope::unlimited()).unwrap(), 0);
        let envelope = Envelope::unlimited();
        let (rows, excess) = measure_within(&envelope, || witness(&engine, envelope.clone()));
        assert_eq!(rows.unwrap(), 0, "{n} hidden vectors");
        excess
    };
    let (small, large) = (excess(2 * CHUNK), excess(16 * CHUNK));
    eprintln!("uncharged at most: {small} bytes at 2 chunks, {large} at 16");
    assert!(
        large <= small + HIT_BYTES * CHUNK / 4,
        "uncharged bytes grow with the hidden vectors: {small} at 2 chunks, {large} at 16"
    );
}

/// The witness at 16 chunks under a request envelope too small for its widened round (the
/// last round holds every one of the vectors' hits: half of that is below what any complete
/// search of them holds at once) is refused with the memory error, and leaves nothing
/// charged to the server's budget or the envelope. Under room for what it charges at its
/// peak (measured unlimited first), it answers: no rows.
#[test]
fn a_vector_search_past_its_envelope_is_refused_and_releases_everything() {
    let n = 16 * CHUNK;
    let engine = store(n);
    let unlimited = Envelope::unlimited();
    assert_eq!(witness(&engine, unlimited.clone()).unwrap(), 0);
    let peak = unlimited.peak();
    eprintln!("peak charged, unlimited: {peak} bytes");

    let shared = SharedBudget::new(usize::MAX / 2);
    let small = Envelope::new(Some(n * HIT_BYTES / 2), Some(shared.clone()));
    let refused = witness(&engine, small.clone());
    assert!(
        matches!(refused, Err(QueryEvaluationError::MemoryLimit(_))),
        "accepted under {} bytes: {refused:?}",
        n * HIT_BYTES / 2
    );
    assert_eq!(
        shared.used(),
        0,
        "the server's budget holds nothing after the refusal"
    );

    let roomy = Envelope::new(Some(peak.max(1) * 2 + (1 << 20)), Some(shared.clone()));
    assert_eq!(witness(&engine, roomy).unwrap(), 0);
    assert_eq!(shared.used(), 0);
}
