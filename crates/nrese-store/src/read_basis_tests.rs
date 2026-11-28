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

// Spec: docs/ops/http-api.md:107. EXPLAIN and the plan carry the same bare complete
// claim that the query sends for an asserted-only read.
#[test]
fn read_basis_explanations_carry_the_asserted_reads_claim() {
    let pipeline = pipeline(ReasoningMode::Owl2Rl, ":alice :p :value .");
    let store = pipeline.store();
    let mut request = SparqlQueryRequest::all(format!("{PREFIXES} ASK {{ :alice :p :value }}"));
    request.read_model = Some(crate::ReadModel::Asserted);
    let prepared = store.prepare_query(&request).expect("prepare asserted ASK");
    let cancel = CancellationToken::new();
    let explained = store.explain_query(&prepared, &cancel).expect("explain");
    let planned = store.plan_query(&prepared, &cancel).expect("plan");
    for (route, assessment) in [
        ("explain", explained.completeness),
        ("plan", planned.completeness),
    ] {
        assert_eq!(
            assessment.as_ref().map(|c| c.header()).as_deref(),
            Some("complete"),
            "{route}"
        );
    }
}

// Spec: docs/spec/reasoning-semantics.md §OWL 2 DL. An uncertified bulk load
// gives asserted rows under DL without certifying the answers or accepting exact.
#[test]
fn read_basis_dl_bulk_load_without_rules_is_not_certified() {
    let store = StoreService::new(StoreConfig::in_memory()).expect("store");
    store.use_dl(true, false);
    let dir = tempfile::tempdir().expect("load directory");
    let file = dir.path().join("data.nt");
    std::fs::write(
        &file,
        "<http://example.com/alice> <http://example.com/p> <http://example.com/value> .\n",
    )
    .expect("write input");
    store
        .bulk_load(&crate::BulkLoadRequest {
            files: vec![file],
            replace: false,
            graph: crate::GraphTarget::DefaultGraph,
            invalid: None,
        })
        .expect("bulk load");
    let mut read = SparqlQueryRequest::all(format!("{PREFIXES} ASK {{ :alice :p :value }}"));
    let answer = store.execute_query(&read).expect("DL query");
    let rows: serde_json::Value = serde_json::from_slice(&answer.payload).expect("ASK JSON");
    assert_eq!(rows["boolean"], true);
    read.dl_answers = Some(crate::DlAnswers::Exact);
    let exact = store.execute_query(&read);
    assert_eq!(
        answer.completeness.as_ref().map(|c| c.as_str()),
        Some("not-certified"),
        "exact accepted the uncertified read: {}",
        exact.is_ok(),
    );
    assert!(matches!(
        exact,
        Err(StoreError::Incomplete(ref e)) if e.status.as_str() == "not-certified"
    ));
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

// Spec: docs/spec/02-storage-and-transactions.md:24 and docs/spec/reasoning-semantics.md:130.
// A DL mode switch can replace the publication without incrementing its revision. The held
// view still reports DL's undecided literal while the next one reports the rules' verdict.
#[test]
fn read_basis_a_mode_switch_at_the_same_revision_keeps_both_verdicts() {
    use crate::ConsistencyStatus::{Consistent, Unknown};
    let pipeline = pipeline(
        ReasoningMode::Owl2Dl,
        ":d a owl:DatatypeProperty . :a :d \"abc\"^^<http://www.w3.org/2001/XMLSchema#integer> .",
    );
    let store = pipeline.store();
    let writer = Arc::clone(store);
    hooks::AFTER_DECIDED.set(Some(Box::new(move || writer.use_dl(false, false))));
    let held = store.publication_report();
    let latest = store.publication_report();
    assert_eq!(
        (held.claim.consistency(), latest.claim.consistency()),
        (Unknown, Consistent)
    );
    assert_eq!(held.revision, latest.revision);
    assert!(held.dl_mode && !latest.dl_mode);
}

// Spec: docs/spec/02-storage-and-transactions.md:24 and docs/spec/reasoning-semantics.md:167.
// The repository's information is one observation of a repair that leaves quarantine.
#[test]
fn read_basis_repository_information_survives_revalidation() {
    use crate::ConsistencyStatus::{Consistent, Inconsistent};
    let import = pipeline(
        ReasoningMode::Disabled,
        ":A owl:disjointWith :B . :a a :A, :B .",
    );
    let store = import.store();
    let repair = MutationPipeline::new(
        Arc::clone(store),
        Arc::new(ReasonerService::new(ReasonerConfig::for_mode(
            ReasoningMode::Owl2Rl,
        ))),
    );
    store
        .rematerialise(nrese_reasoner::rulesets::Ruleset::Owl2Rl)
        .expect("rebuild imported data");
    let revision = store.current_revision();
    hooks::AFTER_DECIDED.set(Some(Box::new(move || {
        repair
            .execute_update_str(&format!("{PREFIXES} DELETE DATA {{ :a a :B }}"))
            .expect("repair");
    })));
    let held = store.publication_report();
    assert_eq!(held.claim.consistency(), Inconsistent { violations: 1 });
    assert_eq!(
        (held.revision, held.shacl.view_revision, held.quarantined),
        (revision, revision, true)
    );
    let latest = store.publication_report();
    assert_eq!(latest.claim.consistency(), Consistent);
    assert!(latest.revision > held.revision && !latest.quarantined);
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

// Spec: docs/spec/reasoning-semantics.md:159. The QL rules cannot decide the negated
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
// An unreasoned write to a previously reasoned store publishes the new data without vouching
// for its closure, even when the inferred stack is empty.
#[test]
fn an_unreasoned_commit_keeps_the_need_for_certification() {
    let store = Arc::new(StoreService::new(StoreConfig::in_memory()).expect("store"));
    store.use_reasoning_rules(Some(nrese_reasoner::rulesets::Ruleset::Owl2Ql.into()));
    let writer = MutationPipeline::new(
        Arc::clone(&store),
        Arc::new(ReasonerService::new(ReasonerConfig::for_mode(
            ReasoningMode::Disabled,
        ))),
    );
    writer
        .execute_update_str(&format!("{PREFIXES} INSERT DATA {{ :alice :p :value }}"))
        .expect("unreasoned commit");
    assert_uncertified_fact(&store, "alice");
}

// Spec: docs/spec/02-storage-and-transactions.md:24 and docs/spec/reasoning-semantics.md:86.
// A replica publishes each incoming record with its own evidence. The rows are readable
// immediately, and the replica cannot vouch for the closure it received.
#[test]
fn an_applied_log_record_keeps_the_replicas_need_for_certification() {
    let dir = tempfile::tempdir().expect("primary directory");
    let primary = MutationPipeline::new(
        Arc::new(StoreService::new(StoreConfig::on_disk(dir.path())).expect("primary")),
        Arc::new(ReasonerService::new(ReasonerConfig::for_mode(
            ReasoningMode::Disabled,
        ))),
    );
    let replica = StoreService::new(StoreConfig {
        replica: true,
        ..StoreConfig::in_memory()
    })
    .expect("replica");
    replica.use_reasoning_rules(Some(nrese_reasoner::rulesets::Ruleset::Owl2Ql.into()));
    for subject in ["alice", "bob"] {
        primary
            .execute_update_str(&format!(
                "{PREFIXES} INSERT DATA {{ :{subject} :p :value }}",
            ))
            .expect("primary commit");
        let after = replica.current_revision();
        let batch = primary
            .store()
            .replication_log(after, 1 << 20)
            .expect("log");
        let applied = replica
            .apply_replication_log(
                &batch.frames,
                &batch.expectation(after),
                &nrese_exec::stop::StopSource::none(),
            )
            .expect("apply record");
        assert_eq!(applied.revision, after + 1);
        assert_uncertified_fact(&replica, subject);
    }
}

fn assert_uncertified_fact(store: &StoreService, subject: &str) {
    let query = SparqlQueryRequest::all(format!("{PREFIXES} ASK {{ :{subject} :p :value }}"));
    let answer = store.execute_query(&query).expect("query");
    let payload: serde_json::Value = serde_json::from_slice(&answer.payload).expect("ASK JSON");
    assert_eq!(payload["boolean"], true, "{subject}");
    assert_eq!(
        answer
            .completeness
            .as_deref()
            .map(nrese_sparql::Completeness::as_str),
        Some("not-certified"),
        "{subject}",
    );
}
