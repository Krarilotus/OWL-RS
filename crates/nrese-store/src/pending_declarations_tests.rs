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
        Register::DuringRead => register_during_read(&store, false),
    }
    read_pending(&store, ReadScope::All, answer)
}

fn register_during_read(store: &Arc<StoreService>, repeat: bool) {
    let store = Arc::clone(store);
    hooks::AFTER_PENDING_GENERATION.set(Some(Box::new(move || {
        store.use_reasoning_rules(Some(Ruleset::Owl2Ql.into()));
        if repeat {
            register_during_read(&store, false);
        }
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

// A declaration change rebuilds the transaction's inferred stack under the rules at the
// read's entry. The same read with no rules ever registered is the reference.
fn check_read<T: std::fmt::Debug + PartialEq>(
    answer: impl Fn(&StoreService, &ReadContext<'_>, &StatementPattern) -> StoreResult<T>,
    without_rules: T,
    with_rules: T,
) {
    let never = read(Register::Never, &answer).expect("read without rules");
    assert_eq!(never, without_rules, "without rules");
    let raced = read(Register::DuringRead, &answer).expect("read during registration");
    let hook_ran = hooks::AFTER_PENDING_GENERATION
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
fn query_uses_the_rules_at_the_declaration_reads_entry() {
    check_read(query, false, true);
}

#[test]
fn get_uses_the_rules_at_the_declaration_reads_entry() {
    check_read(get, String::new(), TYPE.to_owned());
}

#[test]
fn size_uses_the_rules_at_the_declaration_reads_entry() {
    check_read(StoreService::count, 0, 1);
}

fn restricted_read<T>(
    repeat: bool,
    answer: impl FnOnce(&StoreService, &ReadContext<'_>, &StatementPattern) -> StoreResult<T>,
) -> (StoreResult<T>, bool) {
    let config = StoreConfig {
        ql_rewriting: QlRewritingMode::Off,
        ..StoreConfig::in_memory()
    };
    let store = store_with_domain(config, ":note");
    store.use_reasoning_rules(Some(Ruleset::Owl2Ql.into()));
    store
        .rematerialise(Ruleset::Owl2Ql)
        .expect("materialise OWL 2 QL");

    // QUERY announces the format on the committed view before entering the pending read.
    // Drop the rules after that announcement, then register them during the pending read.
    let writer = Arc::clone(&store);
    hooks::BEFORE_PENDING_GENERATION.set(Some(Box::new(move || writer.use_reasoning_rules(None))));
    register_during_read(&store, repeat);
    let scope = ReadScope::Graphs(Arc::new(GraphAccess {
        default_graph: true,
        inferred: true,
        inferred_by_support: true,
        ..GraphAccess::default()
    }));
    let result = read_pending(&store, scope, answer);
    let cleared = hooks::BEFORE_PENDING_GENERATION
        .with_borrow_mut(Option::take)
        .is_none();
    let registered = hooks::AFTER_PENDING_GENERATION
        .with_borrow_mut(Option::take)
        .is_none();
    (result, cleared && registered)
}

// The registered program suppresses the domain inference once :note is declared an
// annotation property. A restricted reader must see the rebuilt view under that program.
fn check_restricted_read<T: std::fmt::Debug + PartialEq>(
    answer: impl FnOnce(&StoreService, &ReadContext<'_>, &StatementPattern) -> StoreResult<T>,
    expected: T,
) {
    let (result, changed) = restricted_read(false, answer);
    assert!(
        matches!(&result, Ok(value) if *value == expected),
        "restricted read after a rule change: expected {expected:?}, got {result:?}",
    );
    assert!(changed, "rules changed during the read");
}

fn check_restricted_refusal<T: std::fmt::Debug>(
    answer: impl FnOnce(&StoreService, &ReadContext<'_>, &StatementPattern) -> StoreResult<T>,
) {
    let (result, changed) = restricted_read(true, answer);
    assert!(
        matches!(
            result,
            Err(StoreError::NotRematerialised { replica: false })
        ),
        "restricted read after two rule changes: expected refusal, got {result:?}",
    );
    assert!(changed, "rules changed during both attempts");
}

#[test]
fn restricted_query_retries_after_a_rule_change() {
    check_restricted_read(query, false);
}

#[test]
fn restricted_get_retries_after_a_rule_change() {
    check_restricted_read(get, String::new());
}

#[test]
fn restricted_size_retries_after_a_rule_change() {
    check_restricted_read(StoreService::count, 0);
}

#[test]
fn restricted_query_refuses_after_two_rule_changes() {
    check_restricted_refusal(query);
}

#[test]
fn restricted_get_refuses_after_two_rule_changes() {
    check_restricted_refusal(get);
}

#[test]
fn restricted_size_refuses_after_two_rule_changes() {
    check_restricted_refusal(StoreService::count);
}
