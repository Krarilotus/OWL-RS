use std::sync::Arc;

use crate::service::hooks;
use crate::{
    CancellationToken, GraphResultFormat, MutationCommand, MutationPipeline, MutationTicket,
    QlRewritingMode, ReadContext, ReadScope, Requester, SparqlQueryRequest, SparqlUpdateRequest,
    StatementOp, StatementPattern, StatementsRequest, StoreConfig, StoreError, StoreResult,
    StoreService,
};
use nrese_reasoner::rulesets::Ruleset;
use nrese_reasoner::{ReasonerConfig, ReasonerService, ReasoningMode};
use nrese_sparql::GraphAccess;

const PREFIXES: &str = "PREFIX : <http://example.com/> \
     PREFIX rdfs: <http://www.w3.org/2000/01/rdf-schema#> \
     PREFIX owl: <http://www.w3.org/2002/07/owl#> ";
const TYPE: &str = "<http://example.com/alice> <http://www.w3.org/1999/02/22-rdf-syntax-ns#type> <http://example.com/Doc> .\n";

enum Register {
    Never,
    DuringRead,
    BeforeTransaction,
}

fn store_with_domain(config: StoreConfig, property: &str) -> Arc<StoreService> {
    let store = Arc::new(StoreService::new(config).expect("store"));
    let reasoner = ReasonerService::new(ReasonerConfig::for_mode(ReasoningMode::Disabled));
    let pipeline = MutationPipeline::new(Arc::clone(&store), Arc::new(reasoner));
    let data =
        format!("{PREFIXES} INSERT DATA {{ {property} rdfs:domain :Doc . :alice {property} :w }}");
    pipeline
        .apply(
            MutationCommand::Update(SparqlUpdateRequest::new(data)),
            &Requester::all(),
            &MutationTicket::new(),
        )
        .expect("base data");
    store
}

fn read_pending<T>(
    store: &StoreService,
    scope: ReadScope,
    answer: impl FnOnce(&StoreService, &ReadContext<'_>, &StatementPattern) -> StoreResult<T>,
) -> StoreResult<T> {
    let pending = StatementsRequest::new(vec![StatementOp::Update(SparqlUpdateRequest::new(
        format!("{PREFIXES} INSERT DATA {{ :note a owl:AnnotationProperty }}"),
    ))]);
    let context = ReadContext {
        scope,
        infer: true,
        pending: Some(&pending),
        cancel: CancellationToken::new(),
        memory: None,
    };
    let pattern = StatementPattern {
        subject: Some(nrese_rdf::NamedNode::new_unchecked("http://example.com/alice").into()),
        predicate: Some(nrese_rdf::vocab::rdf::TYPE.into_owned()),
        object: Some(nrese_rdf::NamedNode::new_unchecked("http://example.com/Doc").into()),
        ..StatementPattern::default()
    };
    answer(store, &context, &pattern)
}

fn read<T>(
    register: Register,
    answer: impl FnOnce(&StoreService, &ReadContext<'_>, &StatementPattern) -> StoreResult<T>,
) -> StoreResult<T> {
    let store = store_with_domain(StoreConfig::in_memory(), ":wrote");
    match register {
        Register::Never => {}
        Register::BeforeTransaction => store.use_reasoning_rules(Some(Ruleset::Owl2Ql.into())),
        Register::DuringRead => register_during_read(&store),
    }
    read_pending(&store, ReadScope::All, answer)
}

fn register_during_read(store: &Arc<StoreService>) {
    let store = Arc::clone(store);
    hooks::AFTER_PENDING_BASE.set(Some(Box::new(move || {
        store.use_reasoning_rules(Some(Ruleset::Owl2Ql.into()));
    })));
}

fn query(store: &StoreService, read: &ReadContext<'_>, _: &StatementPattern) -> StoreResult<bool> {
    let query = format!("{PREFIXES} ASK {{ :alice a :Doc }}");
    let prepared = store.prepare_query(&SparqlQueryRequest::new(query, read.scope.clone()))?;
    let mut out = Vec::new();
    store.run_query_pending(
        read.pending.expect("pending operations"),
        &prepared,
        &read.cancel,
        &mut out,
    )?;
    let result: serde_json::Value = serde_json::from_slice(&out).expect("ASK JSON");
    Ok(result["boolean"].as_bool().expect("ASK boolean"))
}

fn get(
    store: &StoreService,
    read: &ReadContext<'_>,
    pattern: &StatementPattern,
) -> StoreResult<String> {
    let mut out = Vec::new();
    store.write_statements(read, pattern, GraphResultFormat::NTriples, &mut out)?;
    Ok(String::from_utf8(out).expect("N-Triples"))
}

// Spec: docs/spec/02-storage-and-transactions.md:24 and docs/spec/reasoning-semantics.md:95.
// A pending declaration rebuild uses the rules of the committed version held by the read.
// The same read with no rules ever registered is the reference.
fn check_read<T: std::fmt::Debug + PartialEq>(
    answer: impl Fn(&StoreService, &ReadContext<'_>, &StatementPattern) -> StoreResult<T>,
    without_rules: T,
    with_rules: T,
) {
    let never = read(Register::Never, &answer).expect("read without rules");
    assert_eq!(never, without_rules, "without rules");
    let raced = read(Register::DuringRead, &answer).expect("read during registration");
    let hook_ran = hooks::AFTER_PENDING_BASE
        .with_borrow_mut(Option::take)
        .is_none();
    assert_eq!(raced, never, "rules registered during the read");
    assert!(hook_ran, "registration during the read ran");
    assert_eq!(
        read(Register::BeforeTransaction, &answer).expect("read with rules"),
        with_rules,
        "rules registered before the transaction"
    );
}

#[test]
fn query_uses_the_rules_of_the_committed_base() {
    check_read(query, false, true);
}

#[test]
fn get_uses_the_rules_of_the_committed_base() {
    check_read(get, String::new(), TYPE.to_owned());
}

#[test]
fn size_uses_the_rules_of_the_committed_base() {
    check_read(StoreService::count, 0, 1);
}

// Spec: docs/spec/02-storage-and-transactions.md:24 and docs/spec/reasoning-semantics.md:95.
// The registered program suppresses the domain inference once :note is declared an
// annotation property. Each restricted route sees that rebuild with one or two later
// registrations in flight. A fresh read cannot vouch for the previous program's closure.
fn check_restricted_read<T: std::fmt::Debug + PartialEq>(
    replacements: u8,
    answer: impl Fn(&StoreService, &ReadContext<'_>, &StatementPattern) -> StoreResult<T>,
    expected: T,
) {
    let config = StoreConfig {
        ql_rewriting: QlRewritingMode::Off,
        ..StoreConfig::in_memory()
    };
    let store = store_with_domain(config, ":note");
    store.use_reasoning_rules(Some(Ruleset::Owl2Ql.into()));
    store
        .rematerialise(Ruleset::Owl2Ql)
        .expect("materialise OWL 2 QL");

    let writer = Arc::clone(&store);
    hooks::AFTER_PENDING_BASE.set(Some(Box::new(move || {
        writer.use_reasoning_rules(None);
        if replacements == 2 {
            writer.use_reasoning_rules(Some(Ruleset::Rdfs.into()));
        }
    })));
    let scope = ReadScope::Graphs(Arc::new(GraphAccess {
        default_graph: true,
        inferred: true,
        inferred_by_support: true,
        ..GraphAccess::default()
    }));
    let held = read_pending(&store, scope.clone(), &answer);
    let hook_ran = hooks::AFTER_PENDING_BASE
        .with_borrow_mut(Option::take)
        .is_none();
    assert!(hook_ran, "rules replaced during the read");
    assert_eq!(held.expect("read of the held publication"), expected);
    let latest = read_pending(&store, scope, &answer);
    assert!(
        matches!(
            latest,
            Err(StoreError::NotRematerialised { replica: false })
        ),
        "a fresh read has no closure under the new rules, got {latest:?}",
    );
}

#[test]
fn restricted_query_keeps_its_base_after_a_rule_change() {
    check_restricted_read(1, query, false);
}

#[test]
fn restricted_get_keeps_its_base_after_a_rule_change() {
    check_restricted_read(1, get, String::new());
}

#[test]
fn restricted_size_keeps_its_base_after_a_rule_change() {
    check_restricted_read(1, StoreService::count, 0);
}

#[test]
fn restricted_query_keeps_its_base_after_two_rule_changes() {
    check_restricted_read(2, query, false);
}

#[test]
fn restricted_get_keeps_its_base_after_two_rule_changes() {
    check_restricted_read(2, get, String::new());
}

#[test]
fn restricted_size_keeps_its_base_after_two_rule_changes() {
    check_restricted_read(2, StoreService::count, 0);
}
