//! What an upload does to the store, read as it goes: a document with a syntax error in
//! its last statement changes nothing, whatever its write (Graph Store PUT, TELL,
//! statements, restore), and a blank node keeps one identity throughout its document.
//! Guards: these held while uploads were parsed whole first, and must hold now that they
//! are applied as they are read.

use std::sync::Arc;

use nrese_reasoner::{ReasonerConfig, ReasonerService};
use nrese_store::{
    DatasetBackupFormat, DatasetRestoreRequest, GraphResultFormat, GraphTarget, GraphWriteRequest,
    MutationCommand, MutationPipeline, MutationTicket, RdfPayload, ReadContext, Requester,
    StatementOp, StatementPattern, StatementsRequest, StoreConfig, StoreService, TellRequest,
};

/// A table's failing rows, reported together; `guard` rows held before the change too.
#[derive(Default)]
struct Table(Vec<String>);

impl Table {
    fn row(&mut self, guard: bool, label: &str, outcome: Result<(), String>) {
        if let Err(why) = outcome {
            let kind = if guard { "guard " } else { "" };
            self.0.push(format!("{kind}{label}: {why}"));
        }
    }

    fn done(self) {
        assert!(
            self.0.is_empty(),
            "{} row(s) failed:\n{}",
            self.0.len(),
            self.0.join("\n")
        );
    }
}

fn same<T: PartialEq + std::fmt::Debug>(got: T, want: T) -> Result<(), String> {
    match got == want {
        true => Ok(()),
        false => Err(format!("got {got:?}, want {want:?}")),
    }
}

fn pipeline() -> MutationPipeline {
    MutationPipeline::new(
        Arc::new(StoreService::new(StoreConfig::in_memory()).expect("store")),
        Arc::new(ReasonerService::new(ReasonerConfig::default())),
    )
}

fn apply(pipeline: &MutationPipeline, command: MutationCommand) -> Result<(), String> {
    pipeline
        .apply(command, &Requester::all(), &MutationTicket::new())
        .map(drop)
        .map_err(|e| e.to_string())
}

fn count(pipeline: &MutationPipeline) -> u64 {
    pipeline
        .store()
        .count(&ReadContext::all(), &StatementPattern::default())
        .expect("counted")
}

const GOOD: &str = "<http://e/a> <http://e/p> <http://e/b> .\n";
/// Many good statements, then one that isn't Turtle.
fn late_error() -> Vec<u8> {
    let mut document = String::new();
    for i in 0..1_000 {
        document.push_str(&format!("<http://e/s{i}> <http://e/p> <http://e/o{i}> .\n"));
    }
    document.push_str("<http://e/s> <http://e/p> .\n");
    document.into_bytes()
}

#[test]
fn a_late_syntax_error_changes_nothing() {
    let named = GraphTarget::NamedGraph("http://e/g".to_owned());
    let payload = |payload: Vec<u8>| RdfPayload {
        payload: payload.into(),
        format: GraphResultFormat::Turtle,
        base_iri: None,
    };
    let mut table = Table::default();
    for (what, command) in [
        (
            "Graph Store PUT into the graph",
            MutationCommand::GraphWrite(GraphWriteRequest {
                target: named.clone(),
                format: GraphResultFormat::Turtle,
                base_iri: None,
                payload: late_error().into(),
                replace: true,
            }),
        ),
        (
            "Graph Store PUT into the default graph",
            MutationCommand::GraphWrite(GraphWriteRequest {
                target: GraphTarget::DefaultGraph,
                format: GraphResultFormat::Turtle,
                base_iri: None,
                payload: late_error().into(),
                replace: true,
            }),
        ),
        (
            "TELL",
            MutationCommand::Tell(TellRequest {
                target: named.clone(),
                format: GraphResultFormat::Turtle,
                base_iri: None,
                payload: late_error(),
            }),
        ),
        (
            "statements added",
            MutationCommand::Statements(StatementsRequest::new(vec![StatementOp::Add {
                data: payload(late_error()),
                contexts: Vec::new(),
            }])),
        ),
        (
            "statements removed",
            MutationCommand::Statements(StatementsRequest::new(vec![StatementOp::RemoveData {
                data: payload(format!("{GOOD}<http://e/s> <http://e/p> .\n").into_bytes()),
                contexts: Vec::new(),
            }])),
        ),
        (
            "restore",
            MutationCommand::Restore(DatasetRestoreRequest {
                format: DatasetBackupFormat::NQuads,
                payload: format!("{GOOD}<http://e/s> <http://e/p> .\n").into_bytes(),
            }),
        ),
    ] {
        let pipeline = pipeline();
        // One statement in the graph and one in the default graph beforehand.
        let before = format!("{GOOD}<http://e/g1> <http://e/p> <http://e/o> <http://e/g> .\n");
        apply(
            &pipeline,
            MutationCommand::Statements(StatementsRequest::new(vec![StatementOp::Add {
                data: RdfPayload {
                    payload: before.into_bytes().into(),
                    format: GraphResultFormat::NQuads,
                    base_iri: None,
                },
                contexts: Vec::new(),
            }])),
        )
        .expect("the data before");
        let outcome = match apply(&pipeline, command) {
            Ok(()) => Err("applied".to_owned()),
            Err(_) => same(count(&pipeline), 2),
        };
        table.row(true, what, outcome);
    }
    table.done();
}

#[test]
fn a_blank_node_is_one_node_throughout_its_document() {
    let pipeline = pipeline();
    let document = "_:b <http://e/p> <http://e/o1> .\n<http://e/s> <http://e/q> _:b .\n_:b <http://e/p> <http://e/o2> .\n";
    apply(
        &pipeline,
        MutationCommand::GraphWrite(GraphWriteRequest {
            target: GraphTarget::DefaultGraph,
            format: GraphResultFormat::NTriples,
            base_iri: None,
            payload: document.as_bytes().to_vec().into(),
            replace: true,
        }),
    )
    .expect("written");
    let subjects = pipeline
        .store()
        .execute_query_str(
            "SELECT (COUNT(DISTINCT ?b) AS ?n) WHERE { <http://e/s> <http://e/q> ?b . ?b <http://e/p> ?o }",
        )
        .expect("queried");
    let text = String::from_utf8_lossy(&subjects.payload).into_owned();
    assert!(text.contains("\"1\""), "one blank node: {text}");
}
