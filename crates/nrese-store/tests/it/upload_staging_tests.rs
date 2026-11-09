//! An upload's parsed document is charged to the memory the requests share
//! (`total_query_memory_bytes`) before it is kept, on every route that writes it, whatever its
//! transaction (D16-039 S1–S3; the master's direction of 9 Nov: "Charge the parsed body at its
//! producer, regardless of the transaction type"). A document refused for memory leaves the
//! store's statements, revision and dictionary as they were: nothing reaches the shared
//! dictionary before the whole document is parsed and its room is paid for (review UPLOAD-R1-F1).
//! With memory to spare, each document is written (guard rows).
//!
//! Design: notes/2026-11-09-upload-staging-design.md (Harness), rows R1, R2 and G1.

use std::sync::Arc;

use nrese_reasoner::{ReasonerConfig, ReasonerService};
use nrese_store::{
    DatasetBackupFormat, DatasetRestoreRequest, GraphResultFormat, GraphTarget, GraphWriteRequest,
    MutationCommand, MutationError, MutationPipeline, MutationTicket, RdfPayload, ReadContext,
    Requester, ShaclValidationRequest, ShapesSource, StatementOp, StatementPattern,
    StatementsRequest, StoreConfig, StoreError, StoreService, TellRequest,
};

/// How a route answered a document.
#[derive(Debug, PartialEq)]
enum Outcome {
    Written,
    RefusedForMemory,
    Failed(String),
}

fn of_mutation<T>(result: Result<T, MutationError>) -> Outcome {
    match result {
        Ok(_) => Outcome::Written,
        Err(MutationError::Store { source, .. }) if source.is_memory_limit() => {
            Outcome::RefusedForMemory
        }
        Err(error) => Outcome::Failed(error.to_string()),
    }
}

fn of_store<T>(result: Result<T, StoreError>) -> Outcome {
    match result {
        Ok(_) => Outcome::Written,
        Err(error) if error.is_memory_limit() => Outcome::RefusedForMemory,
        Err(error) => Outcome::Failed(error.to_string()),
    }
}

/// What a refused document must leave as it was.
#[derive(Debug, PartialEq)]
struct State {
    quads: usize,
    revision: u64,
    terms: u64,
}

fn state(store: &StoreService) -> State {
    State {
        // Counted without a query, which would draw on the same small memory.
        quads: store.stats().expect("stats").quad_count,
        revision: store.current_revision(),
        terms: store.engine_stats().dictionary.terms,
    }
}

/// A store whose requests share `shared` bytes, holding one statement.
fn pipeline(shared: usize) -> MutationPipeline {
    let mut config = StoreConfig::in_memory();
    config.total_query_memory_bytes = shared;
    let pipeline = MutationPipeline::new(
        Arc::new(StoreService::new(config).expect("store")),
        Arc::new(ReasonerService::new(ReasonerConfig::default())),
    );
    let base = MutationCommand::Statements(StatementsRequest::new(vec![StatementOp::Add {
        data: payload("<urn:base:s> <urn:base:p> <urn:base:o> .\n"),
        contexts: Vec::new(),
    }]));
    assert_eq!(apply(&pipeline, base), Outcome::Written, "the base");
    pipeline
}

fn apply(pipeline: &MutationPipeline, command: MutationCommand) -> Outcome {
    of_mutation(pipeline.apply(command, &Requester::all(), &MutationTicket::new()))
}

fn payload(text: &str) -> RdfPayload {
    RdfPayload {
        payload: text.as_bytes().to_vec().into(),
        format: GraphResultFormat::NTriples,
        base_iri: None,
    }
}

fn contexts(n: usize) -> Vec<nrese_rdf::GraphName> {
    (0..n)
        .map(|i| nrese_rdf::NamedNode::new_unchecked(format!("urn:staging:c{i}")).into())
        .collect()
}

/// A way to send an N-Triples document to a store: how it answered.
type Route = Box<dyn Fn(&MutationPipeline, &str) -> Outcome>;

/// Each way an upload reaches the store and is kept there, at least for its request.
fn routes() -> Vec<(&'static str, Route)> {
    let named = || GraphTarget::NamedGraph("urn:staging:g".to_owned());
    vec![
        (
            "Graph Store PUT",
            Box::new(move |p, text| {
                apply(
                    p,
                    MutationCommand::GraphWrite(GraphWriteRequest {
                        target: named(),
                        format: GraphResultFormat::NTriples,
                        base_iri: None,
                        payload: text.as_bytes().to_vec().into(),
                        replace: true,
                    }),
                )
            }),
        ),
        (
            "TELL",
            Box::new(move |p, text| {
                apply(
                    p,
                    MutationCommand::Tell(TellRequest {
                        target: named(),
                        format: GraphResultFormat::NTriples,
                        base_iri: None,
                        payload: text.as_bytes().to_vec(),
                    }),
                )
            }),
        ),
        (
            "statements Add into two contexts",
            Box::new(|p, text| {
                apply(
                    p,
                    MutationCommand::Statements(StatementsRequest::new(vec![StatementOp::Add {
                        data: payload(text),
                        contexts: contexts(2),
                    }])),
                )
            }),
        ),
        (
            "restore",
            Box::new(|p, text| {
                apply(
                    p,
                    MutationCommand::Restore(DatasetRestoreRequest {
                        format: DatasetBackupFormat::NQuads,
                        payload: text.as_bytes().to_vec(),
                    }),
                )
            }),
        ),
        (
            "SHACL shapes sent with a validation",
            Box::new(|p, text| {
                let request = ShaclValidationRequest {
                    shapes: ShapesSource::Payload {
                        format: GraphResultFormat::NTriples,
                        base_iri: None,
                        payload: text.as_bytes().to_vec(),
                    },
                    ..ShaclValidationRequest::default()
                };
                of_store(p.store().validate_shacl(
                    &nrese_store::ReadScope::All,
                    &request,
                    &nrese_store::CancellationToken::new(),
                ))
            }),
        ),
        (
            "a transaction's pending Add, read inside it",
            Box::new(|p, text| {
                let pending = StatementsRequest::new(vec![StatementOp::Add {
                    data: payload(text),
                    contexts: Vec::new(),
                }]);
                of_store(p.store().count(
                    &ReadContext {
                        pending: Some(&pending),
                        ..ReadContext::all()
                    },
                    &StatementPattern::default(),
                ))
            }),
        ),
    ]
}

/// Sends `document` by every route to a fresh store sharing `shared` bytes, and returns the
/// rows that didn't answer `expected` (a refused document must also leave the store as it was).
fn rows_not(expected: &Outcome, shared: usize, document: &str) -> Vec<String> {
    let mut wrong = Vec::new();
    for (route, send) in routes() {
        let pipeline = pipeline(shared);
        let before = state(pipeline.store());
        let outcome = send(&pipeline, document);
        let after = state(pipeline.store());
        if &outcome != expected {
            wrong.push(format!("{route}: {outcome:?}, expected {expected:?}"));
        } else if outcome == Outcome::RefusedForMemory && after != before {
            wrong.push(format!(
                "{route}: refused; before {before:?}, after {after:?}"
            ));
        }
    }
    wrong
}

/// Every row is checked; the wrong ones are reported together.
fn none_wrong(wrong: &[String]) {
    assert!(
        wrong.is_empty(),
        "{} row(s):\n{}",
        wrong.len(),
        wrong.join("\n")
    );
}

/// R1: `N` statements, each with terms the store hasn't seen. Kept by any write, they take at
/// least `N` encoded quads of 32 bytes, ten times the shared memory: the write is refused.
const N: usize = 40_000;
const SHARED: usize = 64 << 10;
const _: () = assert!(N * 32 >= 10 * SHARED);

fn new_terms() -> String {
    (0..N)
        .map(|i| format!("<urn:staging:s{i}> <urn:staging:p> \"v{i}\" .\n"))
        .collect()
}

/// R1: a write larger than the shared memory is refused, and leaves no term behind.
#[test]
fn a_write_refused_for_memory_interns_nothing() {
    none_wrong(&rows_not(&Outcome::RefusedForMemory, SHARED, &new_terms()));
}

/// R2: a few statements whose literals take `LITERALS` bytes, twice the shared memory. The
/// pending changes hold four encoded quads, so only the parsed document can be refused.
const LITERAL: usize = 128 << 10;
const LITERALS: usize = 4 * LITERAL;
const SHARED_R2: usize = 256 << 10;
const _: () = assert!(LITERALS >= 2 * SHARED_R2);

fn long_literals() -> String {
    (0..4)
        .map(|i| {
            let value: String = std::iter::repeat_n(char::from(b'a' + i as u8), LITERAL).collect();
            format!("<urn:staging:long{i}> <urn:staging:p> \"{value}\" .\n")
        })
        .collect()
}

/// R2: the parsed document is charged, not only the pending changes it makes.
#[test]
fn a_document_larger_than_the_shared_memory_is_refused() {
    none_wrong(&rows_not(
        &Outcome::RefusedForMemory,
        SHARED_R2,
        &long_literals(),
    ));
}

/// Guard (R1, R2): with memory to spare, every route writes both documents.
#[test]
fn guard_documents_within_the_shared_memory_are_written() {
    let mut wrong = rows_not(&Outcome::Written, 256 << 20, &new_terms());
    wrong.extend(rows_not(&Outcome::Written, 4 << 20, &long_literals()));
    none_wrong(&wrong);
}

/// G1 (guard): contexts don't multiply what a document costs. 1,000 statements with 1 KiB
/// literals into 16 contexts: a copy of the document per context would take 16 MiB, four times
/// the shared memory; one copy with its 16,000 pending statements fits.
#[test]
fn guard_contexts_dont_multiply_a_documents_charge() {
    let document: String = (0..1_000)
        .map(|i| {
            let value = format!("{i:0>1024}");
            format!("<urn:staging:c-s{i}> <urn:staging:p> \"{value}\" .\n")
        })
        .collect();
    let pipeline = pipeline(4 << 20);
    let outcome = apply(
        &pipeline,
        MutationCommand::Statements(StatementsRequest::new(vec![StatementOp::Add {
            data: payload(&document),
            contexts: contexts(16),
        }])),
    );
    assert_eq!(outcome, Outcome::Written);
    assert_eq!(
        state(pipeline.store()).quads,
        1 + 16_000,
        "every statement in every context"
    );
}

/// Every kind of RDF 1.2 term in one document: a simple, a language-tagged, a directional and
/// a typed literal, and a triple term (twice) holding a directional literal.
const TERM_KINDS: &str = "<urn:t:s> <urn:t:p> \"plain\" .\n\
                          <urn:t:s> <urn:t:p> \"tagged\"@en .\n\
                          <urn:t:s> <urn:t:p> \"rtl\"@ar--rtl .\n\
                          <urn:t:s> <urn:t:p> \"1\"^^<http://www.w3.org/2001/XMLSchema#integer> .\n\
                          <urn:t:s> <urn:t:p> <<( <urn:t:a> <urn:t:b> \"x\"@en--ltr )>> .\n\
                          <urn:t:s> <urn:t:q> <<( <urn:t:a> <urn:t:b> \"x\"@en--ltr )>> .\n";

/// The store's statements without their graphs, the base's left out, sorted.
fn triples(store: &StoreService) -> Vec<String> {
    let mut triples: Vec<String> = store
        .statements(&ReadContext::all(), &StatementPattern::default())
        .expect("read")
        .iter()
        .filter(|quad| quad.subject.to_string() != "<urn:base:s>")
        .map(|quad| format!("{} {} {}", quad.subject, quad.predicate, quad.object))
        .collect();
    triples.sort();
    triples.dedup();
    triples
}

/// Guard: every route that keeps a document writes each term kind as a SPARQL update of the
/// same statements does (which reads them its own way, unstaged).
#[test]
fn guard_every_term_kind_is_written_as_a_sparql_update_writes_it() {
    let expected = {
        let pipeline = pipeline(256 << 20);
        let update = format!("INSERT DATA {{ {TERM_KINDS} }}");
        let command = MutationCommand::Update(nrese_store::SparqlUpdateRequest::new(&update));
        assert_eq!(apply(&pipeline, command), Outcome::Written);
        triples(pipeline.store())
    };
    assert_eq!(
        expected.len(),
        6,
        "the oracle holds the document: {expected:#?}"
    );
    let mut wrong = Vec::new();
    // The routes that keep it: the first four.
    for (route, send) in routes().into_iter().take(4) {
        let pipeline = pipeline(256 << 20);
        let outcome = send(&pipeline, TERM_KINDS);
        let got = triples(pipeline.store());
        if outcome != Outcome::Written || got != expected {
            wrong.push(format!("{route}: {outcome:?}, {got:#?}"));
        }
    }
    none_wrong(&wrong);
}
