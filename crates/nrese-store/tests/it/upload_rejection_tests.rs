//! A rejected upload leaves no trace (review UPLOAD-R1-F1): a document whose last statement
//! isn't valid, after good statements with terms the store hasn't seen, changes neither the
//! store's statements nor its revision nor its dictionary, whatever route it takes; sent
//! again with blank nodes, it doesn't grow the dictionary either; and a later commit of
//! something else doesn't carry its terms into the log. The dictionary is shared and never
//! rolled back, so a document that will fail must not reach it.

use std::sync::Arc;

use nrese_reasoner::{ReasonerConfig, ReasonerService};
use nrese_store::{
    DatasetBackupFormat, DatasetRestoreRequest, GraphResultFormat, GraphTarget, GraphWriteRequest,
    MutationCommand, MutationPipeline, MutationTicket, RdfPayload, ReadContext, Requester,
    ShaclValidationRequest, ShapesSource, SparqlUpdateRequest, StatementOp, StatementPattern,
    StatementsRequest, StoreConfig, StoreService, TellRequest,
};

/// Good statements with new terms, then one that isn't N-Triples (nor N-Quads).
const REJECTED: &str = "<urn:upload-r1:new-subject> <urn:upload-r1:new-predicate> \"new object\" .\n\
                        <urn:upload-r1:other> <urn:upload-r1:new-predicate> <urn:upload-r1:new-object> .\n\
                        <urn:upload-r1:bad> <urn:upload-r1:new-predicate> .\n";
/// The same with blank nodes: fresh ones each time it is read.
const REJECTED_BLANK: &str = "_:b1 <urn:upload-r1:p> _:b2 .\n_:b2 <urn:upload-r1:p> \"x\" .\n\
                              <urn:upload-r1:bad> <urn:upload-r1:p> .\n";
const BASE: &str = "<urn:base:s> <urn:base:p> <urn:base:o> .\n\
                    <urn:base:s> <urn:base:p> <urn:base:o> <urn:base:g> .\n";

/// What a rejected upload must leave as it was.
#[derive(Debug, PartialEq)]
struct State {
    statements: Vec<String>,
    revision: u64,
    terms: u64,
    arena_bytes: u64,
}

fn state(store: &StoreService) -> State {
    let mut statements: Vec<String> = store
        .statements(&ReadContext::all(), &StatementPattern::default())
        .expect("read")
        .iter()
        .map(ToString::to_string)
        .collect();
    statements.sort();
    let stats = store.engine_stats();
    State {
        statements,
        revision: store.current_revision(),
        terms: stats.dictionary.terms,
        arena_bytes: stats.dictionary.arena_bytes,
    }
}

fn pipeline(config: StoreConfig) -> MutationPipeline {
    MutationPipeline::new(
        Arc::new(StoreService::new(config).expect("store")),
        Arc::new(ReasonerService::new(ReasonerConfig::default())),
    )
}

fn apply(pipeline: &MutationPipeline, command: MutationCommand) -> Result<(), String> {
    pipeline
        .apply(command, &Requester::all(), &MutationTicket::new())
        .map(drop)
        .map_err(|e| e.to_string())
}

fn payload(text: &str) -> RdfPayload {
    RdfPayload {
        payload: text.as_bytes().to_vec(),
        format: GraphResultFormat::NTriples,
        base_iri: None,
    }
}

fn graph_write(text: &str, target: GraphTarget) -> MutationCommand {
    MutationCommand::GraphWrite(GraphWriteRequest {
        target,
        format: GraphResultFormat::NTriples,
        base_iri: None,
        payload: text.as_bytes().to_vec(),
        replace: true,
    })
}

/// A store holding `BASE`.
fn with_base(config: StoreConfig) -> MutationPipeline {
    let pipeline = pipeline(config);
    let base = RdfPayload {
        format: GraphResultFormat::NQuads,
        ..payload(BASE)
    };
    apply(
        &pipeline,
        MutationCommand::Statements(StatementsRequest::new(vec![StatementOp::Add {
            data: base,
            contexts: Vec::new(),
        }])),
    )
    .expect("the base");
    pipeline
}

/// A way to send a document to a store: the outcome.
type Route = Box<dyn Fn(&MutationPipeline, &str) -> Result<(), String>>;

/// Each way an upload reaches the store, sending `text` that must be refused.
fn routes() -> Vec<(&'static str, Route)> {
    let named = || GraphTarget::NamedGraph("urn:upload-r1:g".to_owned());
    vec![
        (
            "Graph Store PUT, default graph",
            Box::new(|p, text| apply(p, graph_write(text, GraphTarget::DefaultGraph))),
        ),
        (
            "Graph Store PUT, named graph",
            Box::new(move |p, text| apply(p, graph_write(text, named()))),
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
                let contexts = vec![
                    nrese_rdf::NamedNode::new_unchecked("urn:upload-r1:c1").into(),
                    nrese_rdf::NamedNode::new_unchecked("urn:upload-r1:c2").into(),
                ];
                apply(
                    p,
                    MutationCommand::Statements(StatementsRequest::new(vec![StatementOp::Add {
                        data: payload(text),
                        contexts,
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
                p.store()
                    .validate_shacl(
                        &nrese_store::ReadScope::All,
                        &request,
                        &nrese_store::CancellationToken::new(),
                    )
                    .map(drop)
                    .map_err(|e| e.to_string())
            }),
        ),
        (
            "a transaction's pending Add, read inside it",
            Box::new(|p, text| {
                let pending = StatementsRequest::new(vec![StatementOp::Add {
                    data: payload(text),
                    contexts: Vec::new(),
                }]);
                p.store()
                    .count(
                        &ReadContext {
                            pending: Some(&pending),
                            ..ReadContext::all()
                        },
                        &StatementPattern::default(),
                    )
                    .map(drop)
                    .map_err(|e| e.to_string())
            }),
        ),
    ]
}

#[test]
fn a_rejected_upload_leaves_statements_revision_and_dictionary_as_they_were() {
    let mut failed = Vec::new();
    for (route, send) in routes() {
        // (document, times sent): a blank-node document is sent three times, fresh nodes
        // each time.
        for (document, times) in [(REJECTED, 1), (REJECTED_BLANK, 3)] {
            let pipeline = with_base(StoreConfig::in_memory());
            let before = state(pipeline.store());
            let mut outcome = Ok(());
            for _ in 0..times {
                if send(&pipeline, document).is_ok() {
                    outcome = Err("accepted".to_owned());
                }
            }
            let after = state(pipeline.store());
            if outcome.is_ok() && after != before {
                outcome = Err(format!("before {before:?}, after {after:?}"));
            }
            if let Err(why) = outcome {
                failed.push(format!("{route} ({times}×): {why}"));
            }
        }
    }
    assert!(
        failed.is_empty(),
        "{} row(s) failed:\n{}",
        failed.len(),
        failed.join("\n")
    );
}

/// On disk: a later commit of something else must not carry a rejected upload's terms into
/// the log (a reopened store has the dictionary of one that never saw the upload).
#[test]
fn a_later_commit_logs_no_term_of_a_rejected_upload() {
    let terms = |rejected: bool| {
        let dir = tempfile::tempdir().expect("dir");
        {
            let pipeline = with_base(StoreConfig::on_disk(dir.path()));
            if rejected {
                assert!(
                    apply(&pipeline, graph_write(REJECTED, GraphTarget::DefaultGraph)).is_err()
                );
            }
            apply(
                &pipeline,
                MutationCommand::Update(SparqlUpdateRequest::new(
                    "INSERT DATA { <urn:later:s> <urn:later:p> <urn:later:o> }",
                )),
            )
            .expect("a later commit");
        }
        let reopened = StoreService::new(StoreConfig::on_disk(dir.path())).expect("reopened");
        let state = state(&reopened);
        (state.statements, state.terms)
    };
    assert_eq!(terms(true), terms(false));
}
