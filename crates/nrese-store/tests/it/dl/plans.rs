//! A plan's status against its run's (docs/spec/reasoning-semantics.md, line 78: the
//! status is in the `NRESE-Completeness` header and EXPLAIN). A plan is said before the
//! query runs, so it may claim less than its run, never more: a plan that says `complete`
//! where the run says `sound-only` promises answers the run doesn't certify. Two reasons
//! the run adds whatever path decides are checked: user rules beside OWL 2 DL, and a
//! consistency that isn't known (`dl.consistency = "off"`). Each on the RL route and off
//! it. And a consistency no check has decided yet, which the run decides.

use std::sync::Arc;

use nrese_reasoner::{ReasonerConfig, ReasonerService, ReasoningMode, UserRules};
use nrese_store::{
    CancellationToken, DlConfig, DlConsistency, MutationPipeline, SparqlQueryRequest, StoreConfig,
    StoreService,
};

use super::{PREFIXES, insert, pipeline_with};

/// In OWL 2 RL: the rules route.
const RL: &str = ":Dog rdfs:subClassOf :Animal . :rex a :Dog .";

/// The same with a union on the right of a subclass axiom, outside OWL 2 RL: U1 and the
/// DL engines. `:Animal` gets no fact in U1 beyond L, so it stays closed.
const OFF_RL: &str = ":Dog rdfs:subClassOf :Animal . :rex a :Dog . \
     :A rdfs:subClassOf [ owl:unionOf ( :B :C ) ] .";

const QUERY: &str = "SELECT ?x { ?x a :Animal }";

/// `owl2-dl` with a user rule that reads nothing of [`RL`].
fn with_user_rules() -> MutationPipeline {
    let rules = UserRules::n3(
        "special.n3",
        "{ ?x a <http://example.com/Special> } => { ?x a <http://example.com/Rare> } .",
    )
    .expect("rules");
    let config = ReasonerConfig::for_mode(ReasoningMode::Owl2Dl)
        .with_rules(Some(Arc::new(rules)))
        .expect("owl2-dl with rules");
    let store = StoreService::new(crate::support::in_memory_store_config()).expect("store");
    MutationPipeline::new(Arc::new(store), Arc::new(ReasonerService::new(config)))
}

fn with_consistency_off() -> MutationPipeline {
    pipeline_with(DlConfig {
        consistency: DlConsistency::Off,
        ..DlConfig::default()
    })
}

#[test]
fn a_plan_never_claims_more_than_its_run() {
    // `not-certified` claims nothing, so it is the lowest.
    let order = ["not-certified", "unsound", "sound-only", "complete"];
    let rank = |name: &str| order.iter().position(|o| *o == name);
    let cancel = CancellationToken::new();
    let mut wrong = Vec::new();
    for (config, make) in [
        ("user rules", with_user_rules as fn() -> MutationPipeline),
        ("dl.consistency = off", with_consistency_off),
    ] {
        for (route, data) in [("on the RL route", RL), ("off it", OFF_RL)] {
            let at = format!("{config}, {route}");
            let dl = make();
            insert(&dl, data).expect("data");
            let store = dl.store();
            let request = SparqlQueryRequest::all(format!("{PREFIXES}{QUERY}"));
            let run = store.execute_query(&request).expect("answered");
            let run = run.completeness.as_ref().map_or("none", |c| c.as_str());
            // Guard: the run adds its reason, so it isn't complete (rows 31, 49).
            if run == "complete" || run == "none" {
                wrong.push(format!("{at}: the run says {run}"));
            }
            let prepared = store.prepare_query(&request).expect("prepared");
            let planned = store.plan_query(&prepared, &cancel).expect("planned");
            let plan = planned.completeness.as_ref().map_or("none", |c| c.as_str());
            if rank(plan) > rank(run) {
                wrong.push(format!(
                    "{at}: the plan says {plan}, the run {run}: {:?}",
                    planned.completeness
                ));
            }
        }
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
}

/// `owl2-dl` over a store kept in `dir`.
fn on_disk(dir: &std::path::Path) -> MutationPipeline {
    let store = StoreService::new(StoreConfig::on_disk(dir)).expect("store");
    let config = ReasonerConfig::for_mode(ReasoningMode::Owl2Dl);
    MutationPipeline::new(Arc::new(store), Arc::new(ReasonerService::new(config)))
}

#[test]
fn a_plan_where_no_check_decided_the_consistency_says_the_run_decides_it() {
    // A revision no commit's check recorded (the store reopened: the record is kept in
    // memory), and U1 can't prove the data consistent (it doesn't check a data at-most
    // restriction, here the functional property's): before the run checks it, the data's
    // consistency is unknown, so the plan can't say `complete`.
    let dir = tempfile::tempdir().expect("dir");
    let functional = ":age a owl:DatatypeProperty , owl:FunctionalProperty .";
    insert(&on_disk(dir.path()), &format!("{OFF_RL} {functional}")).expect("data");
    let dl = on_disk(dir.path());
    let store = dl.store();
    let request = SparqlQueryRequest::all(format!("{PREFIXES}{QUERY}"));
    let prepared = store.prepare_query(&request).expect("prepared");
    let planned = store
        .plan_query(&prepared, &CancellationToken::new())
        .expect("planned");
    let status = planned.completeness.expect("a status");
    assert_eq!(status.as_str(), "sound-only", "{status:?}");
    let undecided = |text: &str| text.contains("consistency") && text.contains("when it runs");
    assert!(
        status.reasons.iter().any(|r| undecided(&r.text)),
        "{status:?}"
    );
}
