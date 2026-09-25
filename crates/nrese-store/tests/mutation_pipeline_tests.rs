mod support;

use std::sync::Arc;

use nrese_reasoner::{ReasonerConfig, ReasonerService, ReasoningMode};
use nrese_store::{
    MutationCommand, MutationError, MutationPipeline, MutationTicket, SparqlUpdateRequest,
    StoreService,
};
use support::in_memory_store_config;

fn pipeline(mode: ReasoningMode) -> MutationPipeline {
    let store = StoreService::new(in_memory_store_config()).expect("store");
    MutationPipeline::new(
        Arc::new(store),
        Arc::new(ReasonerService::new(ReasonerConfig::for_mode(mode))),
    )
}

fn insert(triple: &str) -> MutationCommand {
    MutationCommand::Update(SparqlUpdateRequest::new(format!(
        "INSERT DATA {{ {triple} }}"
    )))
}

fn contains(pipeline: &MutationPipeline, triple: &str) -> bool {
    let result = pipeline
        .store()
        .execute_query_str(&format!("ASK {{ {triple} }}"))
        .expect("ask");
    String::from_utf8(result.payload)
        .expect("utf8")
        .contains("true")
}

/// Regression for audit finding F2: a mutation whose caller gave up must never commit.
#[test]
fn cancelled_ticket_prevents_commit() {
    let pipeline = pipeline(ReasoningMode::Disabled);
    let triple = "<http://example.com/ghost> <http://example.com/p> <http://example.com/o>";
    let ticket = MutationTicket::new();
    assert!(ticket.cancel());

    let result = pipeline.apply(insert(triple), &ticket);

    assert!(matches!(result, Err(MutationError::Cancelled)));
    assert!(!contains(&pipeline, triple));
}

#[test]
fn committed_mutation_cannot_be_cancelled_afterwards() {
    let pipeline = pipeline(ReasoningMode::Disabled);
    let triple = "<http://example.com/a> <http://example.com/p> <http://example.com/b>";
    let ticket = MutationTicket::new();

    pipeline.apply(insert(triple), &ticket).expect("commit");

    assert!(
        !ticket.cancel(),
        "a started commit must not report as cancelled"
    );
    assert!(contains(&pipeline, triple));
}

#[test]
fn gate_rejection_is_recorded_and_not_committed() {
    let pipeline = pipeline(ReasoningMode::RulesMvp);
    let setup = "<http://example.com/A> <http://www.w3.org/2002/07/owl#disjointWith> <http://example.com/B> .
                 <http://example.com/x> a <http://example.com/A> .";
    pipeline
        .apply(insert(setup), &MutationTicket::new())
        .expect("consistent setup");

    let conflicting = "<http://example.com/x> a <http://example.com/B>";
    let result = pipeline.apply(insert(conflicting), &MutationTicket::new());

    assert!(matches!(result, Err(MutationError::Rejected(_))));
    assert!(!contains(&pipeline, conflicting));
    assert!(pipeline.last_reasoning_run().is_some());
}
