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

fn delete(triple: &str) -> MutationCommand {
    MutationCommand::Update(SparqlUpdateRequest::new(format!(
        "DELETE DATA {{ {triple} }}"
    )))
}

const EX: &str = "http://example.com/";

/// Reasoner v2 (R4): commits keep the inferred stack equal to the closure, and queries see
/// it through the default (materialised) read model.
#[test]
fn owl2_rl_inferences_follow_commits() {
    let pipeline = pipeline(ReasoningMode::Owl2Rl);
    let ticket = MutationTicket::new;
    let schema = format!(
        "<{EX}Cat> <http://www.w3.org/2000/01/rdf-schema#subClassOf> <{EX}Animal> .
         <{EX}knows> a <http://www.w3.org/2002/07/owl#SymmetricProperty> .
         <{EX}ancestor> a <http://www.w3.org/2002/07/owl#TransitiveProperty> ."
    );
    pipeline.apply(insert(&schema), &ticket()).expect("schema");
    let facts = format!(
        "<{EX}tom> a <{EX}Cat> . <{EX}a> <{EX}knows> <{EX}b> .
         <{EX}x> <{EX}ancestor> <{EX}y> . <{EX}y> <{EX}ancestor> <{EX}z> ."
    );
    pipeline.apply(insert(&facts), &ticket()).expect("facts");
    for inferred in [
        format!("<{EX}tom> a <{EX}Animal>"),
        format!("<{EX}b> <{EX}knows> <{EX}a>"),
        format!("<{EX}x> <{EX}ancestor> <{EX}z>"),
    ] {
        assert!(contains(&pipeline, &inferred), "missing {inferred}");
    }
    // Deleting the support retracts the inference.
    pipeline
        .apply(
            delete(&format!("<{EX}y> <{EX}ancestor> <{EX}z>")),
            &ticket(),
        )
        .expect("delete");
    assert!(!contains(
        &pipeline,
        &format!("<{EX}x> <{EX}ancestor> <{EX}z>")
    ));
    assert!(contains(&pipeline, &format!("<{EX}tom> a <{EX}Animal>")));
    // An asserted statement that is also derivable counts as asserted only.
    pipeline
        .apply(insert(&format!("<{EX}tom> a <{EX}Animal>")), &ticket())
        .expect("explicit");
    let inferred_before = pipeline.store().stats().expect("stats").inferred_count;
    pipeline
        .apply(delete(&format!("<{EX}tom> a <{EX}Animal>")), &ticket())
        .expect("delete explicit");
    // Still derivable, so still visible: it moved back to the inferred stack.
    assert!(contains(&pipeline, &format!("<{EX}tom> a <{EX}Animal>")));
    let inferred_after = pipeline.store().stats().expect("stats").inferred_count;
    assert_eq!(inferred_after, inferred_before + 1);
}

#[test]
fn owl2_rl_rejects_inconsistent_commits() {
    let pipeline = pipeline(ReasoningMode::Owl2Rl);
    let setup = format!(
        "<{EX}A> <http://www.w3.org/2002/07/owl#disjointWith> <{EX}B> .
         <{EX}x> a <{EX}A> ."
    );
    pipeline
        .apply(insert(&setup), &MutationTicket::new())
        .expect("consistent setup");
    let conflicting = format!("<{EX}x> a <{EX}B>");
    let result = pipeline.apply(insert(&conflicting), &MutationTicket::new());
    let Err(MutationError::Rejected(reject)) = result else {
        panic!("expected a rejection, got {result:?}");
    };
    assert!(reject.detail.contains("cax-dw"), "{}", reject.detail);
    assert!(!contains(&pipeline, &conflicting));
}

#[test]
fn rematerialise_installs_the_closure_of_existing_data() {
    // Data committed without reasoning, then the ruleset is switched on.
    let plain = pipeline(ReasoningMode::Disabled);
    let data = format!(
        "<{EX}Cat> <http://www.w3.org/2000/01/rdf-schema#subClassOf> <{EX}Animal> .
         <{EX}tom> a <{EX}Cat> ."
    );
    plain
        .apply(insert(&data), &MutationTicket::new())
        .expect("data");
    let inferred = format!("<{EX}tom> a <{EX}Animal>");
    assert!(!contains(&plain, &inferred));
    let report = plain
        .store()
        .rematerialise(nrese_reasoner::v2::rulesets::Ruleset::Rdfs)
        .expect("rematerialise");
    assert_eq!((report.inferred_inserted, report.inferred_deleted), (1, 0));
    assert!(contains(&plain, &inferred));
    // Unchanged data: no new revision.
    let again = plain
        .store()
        .rematerialise(nrese_reasoner::v2::rulesets::Ruleset::Rdfs)
        .expect("again");
    assert_eq!(again.revision, report.revision);
}
