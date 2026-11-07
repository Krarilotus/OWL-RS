//! Guard of what an upload holds while it is applied: a document's statements go to their
//! consumer (a write's transaction, the speculative one of a validation) as they are read,
//! never all at once as owned copies. A Turtle document with one long namespace and many
//! short statements names the same few terms each time; held as owned copies it costs
//! about 3 KB per 11-byte statement (the upload sizing's witness, 64 MiB of it about
//! 17 GiB). Read as it goes, it costs its dictionary entries and one statement. Measured
//! (`nrese_exec::measure`, every thread counts), so this file is a test binary of its own.

use std::sync::Arc;

use nrese_exec::measure;
use nrese_reasoner::{ReasonerConfig, ReasonerService};
use nrese_store::{
    DatasetRestoreRequest, GraphResultFormat, GraphTarget, GraphWriteRequest, MutationCommand,
    MutationPipeline, MutationTicket, RdfPayload, Requester, ShaclValidationRequest, ShapesSource,
    StatementOp, StatementsRequest, StoreConfig, StoreService,
};

#[global_allocator]
static ALLOCATOR: measure::Measured<std::alloc::System> = measure::Measured(std::alloc::System);

/// Statements in the document: held as owned copies, about 150 MB.
const STATEMENTS: usize = 50_000;
/// What applying it may take at its peak: its dictionary entries, one statement, the
/// transaction's few distinct quads, and the store's fixed working sets.
const PEAK: usize = 32 << 20;

/// `STATEMENTS` times the same statement, its terms in a 1,000-character namespace.
fn long_prefix_turtle() -> Vec<u8> {
    let namespace = format!("http://example.com/{}/", "x".repeat(1_000));
    let mut document = format!("@prefix : <{namespace}> .\n");
    for _ in 0..STATEMENTS {
        document.push_str(":s :p :o .\n");
    }
    document.into_bytes()
}

/// The same statements as N-Quads, as a restore reads them.
fn long_iri_nquads() -> Vec<u8> {
    let iri = format!("<http://example.com/{}/s>", "x".repeat(1_000));
    let line = format!("{iri} {iri} {iri} .\n");
    line.repeat(STATEMENTS).into_bytes()
}

fn pipeline() -> MutationPipeline {
    MutationPipeline::new(
        Arc::new(StoreService::new(StoreConfig::in_memory()).expect("store")),
        Arc::new(ReasonerService::new(ReasonerConfig::default())),
    )
}

fn apply(pipeline: &MutationPipeline, command: MutationCommand) -> usize {
    let (outcome, peak) =
        measure::measure(|| pipeline.apply(command, &Requester::all(), &MutationTicket::new()));
    outcome.expect("applied");
    peak
}

fn below(what: &str, peak: usize) {
    assert!(
        peak < PEAK,
        "{what}: {} MiB at its peak, more than {} MiB",
        peak >> 20,
        PEAK >> 20
    );
}

#[test]
fn a_graph_store_write_holds_no_copy_of_its_document() {
    if !measure::alone() {
        return;
    }
    let pipeline = pipeline();
    let payload = long_prefix_turtle();
    let peak = apply(
        &pipeline,
        MutationCommand::GraphWrite(GraphWriteRequest {
            target: GraphTarget::NamedGraph("http://example.com/g".to_owned()),
            format: GraphResultFormat::Turtle,
            base_iri: None,
            payload,
            replace: true,
        }),
    );
    below("Graph Store PUT", peak);
}

#[test]
fn statements_added_into_contexts_hold_no_copy_of_their_document() {
    if !measure::alone() {
        return;
    }
    let pipeline = pipeline();
    let contexts = (0..3)
        .map(|i| nrese_rdf::NamedNode::new_unchecked(format!("http://example.com/c{i}")).into())
        .collect();
    let peak = apply(
        &pipeline,
        MutationCommand::Statements(StatementsRequest::new(vec![StatementOp::Add {
            data: RdfPayload {
                payload: long_prefix_turtle(),
                format: GraphResultFormat::Turtle,
                base_iri: None,
            },
            contexts,
        }])),
    );
    below("statements into three contexts", peak);
}

#[test]
fn a_restore_holds_no_copy_of_its_dataset() {
    if !measure::alone() {
        return;
    }
    let pipeline = pipeline();
    let peak = apply(
        &pipeline,
        MutationCommand::Restore(DatasetRestoreRequest {
            format: nrese_store::DatasetBackupFormat::NQuads,
            payload: long_iri_nquads(),
        }),
    );
    below("restore", peak);
}

#[test]
fn shapes_sent_with_a_validation_hold_no_copy_of_their_document() {
    if !measure::alone() {
        return;
    }
    let store = StoreService::new(StoreConfig::in_memory()).expect("store");
    let request = ShaclValidationRequest {
        shapes: ShapesSource::Payload {
            format: GraphResultFormat::Turtle,
            base_iri: None,
            payload: long_prefix_turtle(),
        },
        ..ShaclValidationRequest::default()
    };
    let (outcome, peak) = measure::measure(|| {
        store.validate_shacl(
            &nrese_store::ReadScope::All,
            &request,
            &nrese_store::CancellationToken::new(),
        )
    });
    outcome.expect("validated");
    below("request shapes", peak);
}
