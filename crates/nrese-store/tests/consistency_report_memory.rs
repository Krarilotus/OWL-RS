//! Guard of the consistency report's memory (P2-C4, OTH-premises): a commit's consistency
//! check records its violations within the request's budget. A rule with three
//! independent body atoms over n facts each fires n³ times; the report doesn't grow with
//! the firings past the budget, and the commit is still rejected, saying the count is a
//! lower bound. Measured (`nrese_exec::measure`), so this file is a test binary of its own.

use std::sync::Arc;

use nrese_exec::measure;
use nrese_reasoner::{ReasonerConfig, ReasonerService, ReasoningMode, UserRules};
use nrese_store::{
    MutationCommand, MutationError, MutationPipeline, MutationTicket, Requester,
    SparqlUpdateRequest, StoreConfig, StoreService,
};

#[global_allocator]
static ALLOCATOR: measure::Measured<std::alloc::System> = measure::Measured(std::alloc::System);

const EX: &str = "http://example.com/";

/// Three body atoms that share no variable: every combination of a `p`, a `q` and an `r`
/// fact is a violation of its own.
const RULES: &str = r#"
@prefix : <http://example.com/> .
{ ?a :p ?x . ?b :q ?y . ?c :r ?z } => false .
"#;

/// What all requests together may hold: the commit's report draws on it.
const POOL: usize = 4 << 20;

fn pipeline() -> MutationPipeline {
    let mut config = StoreConfig::in_memory();
    config.total_query_memory_bytes = POOL;
    let store = StoreService::new(config).expect("store");
    let rules = Arc::new(UserRules::n3("check.n3", RULES).expect("the rules compile"));
    let reasoner = ReasonerConfig::for_mode(ReasoningMode::Custom)
        .with_rules(Some(rules))
        .expect("a mode for the rules");
    MutationPipeline::new(Arc::new(store), Arc::new(ReasonerService::new(reasoner)))
}

/// `n` facts for each of `p`, `q` and `r`, in one insert.
fn facts(n: usize) -> MutationCommand {
    let mut triples = String::new();
    for p in ["p", "q", "r"] {
        for i in 0..n {
            triples.push_str(&format!("<{EX}{p}s{i}> <{EX}{p}> <{EX}{p}o{i}> .\n"));
        }
    }
    MutationCommand::Update(SparqlUpdateRequest::new(format!(
        "INSERT DATA {{ {triples} }}"
    )))
}

/// The most bytes the commit held at once, and its rejection's detail.
fn commit(n: usize) -> (usize, String) {
    let pipeline = pipeline();
    let command = facts(n);
    let (result, peak) = measure::measure(|| {
        pipeline.apply(command, &Requester::all(), &MutationTicket::new())
    });
    let Err(MutationError::Rejected(reject)) = result else {
        panic!("{n} facts each: expected a rejection, got {result:?}");
    };
    (peak, reject.detail)
}

/// 40 and 80 facts each: 64,000 and 512,000 violations. Without a charge the report alone
/// grows by tens of MB between them (a name, a bindings vector and a set entry per
/// violation); within the budget it grows by at most the pool, beside the facts' own few
/// KB. The larger commit is still rejected, and says its count is a lower bound.
#[test]
fn a_commit_records_its_violations_within_the_request_budget() {
    if !measure::alone() {
        return;
    }
    let (small, _) = commit(40);
    let (large, detail) = commit(80);
    let grown = large.saturating_sub(small);
    assert!(
        grown < 4 * POOL,
        "the commit's peak grew by {grown} bytes from 64,000 to 512,000 violations \
         ({small} to {large}); the budget is {POOL}"
    );
    assert!(detail.contains("at least"), "{detail}");
}
