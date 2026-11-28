use std::sync::Arc;

use nrese_rdf::{NamedNode, Triple, vocab::owl};
use nrese_reasoner::{ReasonerConfig, ReasonerService, ReasoningMode};

use super::{StoreService, hooks};
use crate::{
    CancellationToken, MutationPipeline, QlRewritingMode, SparqlQueryRequest, SparqlUpdateRequest,
    StatementOp, StatementsRequest, StoreConfig, StoreError,
};

const PREFIXES: &str = "PREFIX : <http://example.com/> \
    PREFIX owl: <http://www.w3.org/2002/07/owl#> \
    PREFIX rdfs: <http://www.w3.org/2000/01/rdf-schema#> ";

fn pipeline(mode: ReasoningMode, data: &str) -> MutationPipeline {
    let store = Arc::new(StoreService::new(StoreConfig::in_memory()).expect("store"));
    let reasoner = Arc::new(ReasonerService::new(ReasonerConfig::for_mode(mode)));
    let pipeline = MutationPipeline::new(store, reasoner);
    pipeline
        .execute_update_str(&format!("{PREFIXES} INSERT DATA {{ {data} }}"))
        .expect("insert premise");
    pipeline
}

fn pending_membership(store: &StoreService) -> bool {
    let pending = StatementsRequest::new(vec![StatementOp::Update(SparqlUpdateRequest::new(
        format!("{PREFIXES} INSERT DATA {{ :note a owl:AnnotationProperty }}"),
    ))]);
    let prepared = store
        .prepare_query(&SparqlQueryRequest::all(format!(
            "{PREFIXES} ASK {{ :alice a :Doc }}"
        )))
        .expect("prepared ASK");
    let mut payload = Vec::new();
    store
        .run_query_pending(&pending, &prepared, &CancellationToken::new(), &mut payload)
        .expect("pending ASK");
    let answer: serde_json::Value = serde_json::from_slice(&payload).expect("ASK JSON");
    answer["boolean"].as_bool().expect("ASK boolean")
}

fn pending_fixture() -> MutationPipeline {
    let pipeline = pipeline(
        ReasoningMode::Owl2Ql,
        ":note rdfs:domain :Doc . :alice :note :value .",
    );
    pipeline.store().set_ql_rewriting(QlRewritingMode::Off);
    pipeline
}

// Spec: docs/spec/02-storage-and-transactions.md:24 and docs/spec/reasoning-semantics.md:95.
// A declaration rebuild uses the committed base's program. Declaring the property to be an
// annotation removes its domain inference even if the registered program is cleared meanwhile.
#[test]
fn read_basis_pending_base_keeps_its_program_during_registration() {
    let pipeline = pending_fixture();
    let store = pipeline.store();
    let writer = Arc::clone(store);
    hooks::AFTER_PENDING_BASE.set(Some(Box::new(move || writer.use_reasoning_rules(None))));
    assert!(
        !pending_membership(store),
        "pending read used rules installed after its base"
    );
    assert!(
        pending_membership(store),
        "a fresh read uses the publication without rules"
    );
}

// Spec: docs/spec/02-storage-and-transactions.md:24 and docs/spec/reasoning-semantics.md:95.
// A read during registration sees the published program and certificate as a pair.
#[test]
fn read_basis_registration_gap_keeps_the_published_program() {
    let pipeline = pending_fixture();
    let store = pipeline.store();
    let reader = Arc::clone(store);
    hooks::AFTER_RULES_REGISTERED.set(Some(Box::new(move || {
        assert!(
            !pending_membership(&reader),
            "pending read used rules before their publication"
        );
    })));
    store.use_reasoning_rules(None);
    assert!(
        pending_membership(store),
        "the next read uses the newly published rules"
    );
}

// Spec: docs/spec/reasoning-semantics.md:21 and :135. The QL rules cannot decide the negated
// equality in this fixture by refutation. Absence of that proof supplies no negative verdict.
#[test]
fn read_basis_rules_that_cannot_refute_give_no_entailment_verdict() {
    let pipeline = pipeline(
        ReasoningMode::Owl2Ql,
        ":a a :A . :b a :B . :A owl:disjointWith :B .",
    );
    let iri = |name| NamedNode::new_unchecked(format!("http://example.com/{name}"));
    let conclusion = [Triple::new(
        iri("a"),
        owl::DIFFERENT_FROM.into_owned(),
        iri("b"),
    )];
    let program = ReasonerConfig::for_mode(ReasoningMode::Owl2Ql)
        .materialised_program()
        .expect("QL program");
    let answer = pipeline.store().entails(&program, &conclusion);
    assert!(
        matches!(answer, Err(StoreError::Undecided(_))),
        "unproved refutation gave {answer:?}"
    );
}

// Spec: docs/spec/02-storage-and-transactions.md:24 and docs/spec/reasoning-semantics.md:86.
// A query's rewriting report describes its own schema, even after a hazardous axiom is removed.
#[test]
fn read_basis_ql_schema_forgets_a_deleted_axiom() {
    let pipeline = pipeline(
        ReasoningMode::Owl2Ql,
        ":p1 rdfs:subPropertyOf :p0 . :i1 owl:sameAs :i0 .",
    );
    let store = pipeline.store();
    let query = SparqlQueryRequest::all(format!("{PREFIXES} SELECT ?x ?o WHERE {{ ?x :p0 ?o }}"));
    let read = || {
        store
            .execute_query(&query)
            .expect("query")
            .completeness
            .expect("report")
    };
    let before = read();
    pipeline
        .execute_update_str(&format!(
            "{PREFIXES} DELETE DATA {{ :p1 rdfs:subPropertyOf :p0 }}"
        ))
        .expect("delete subproperty axiom");
    let after = read();
    let names_deleted_axiom =
        |text: &str| text.contains("an axiom outside the profile equates individuals");
    assert!(
        !after
            .reasons
            .iter()
            .any(|reason| names_deleted_axiom(&reason.text)),
        "later query still named the deleted axiom: {after:?}"
    );
    assert!(
        before
            .reasons
            .iter()
            .any(|reason| names_deleted_axiom(&reason.text)),
        "the live axiom should be reported: {before:?}"
    );
}
