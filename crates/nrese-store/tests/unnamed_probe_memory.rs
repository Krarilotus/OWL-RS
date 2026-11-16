//! The process's memory limit refuses reasoning under `reasoner.unnamed_classes = "skip"` with
//! the store's typed refusal, the check of whether the rules make a declared class's members
//! things (`nrese_reasoner::unnamed::members_are_things`) included: over the limit that check
//! panicked (Sol's review of D16-773 row 1, r1 S2). A rematerialisation, and an entailment
//! question, whose refutation compiles the program: the path a server meets first. A test
//! binary of its own: the limit is process-wide.

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;

use nrese_exec::memory::{READS_MEMORY, set_process_limit};
use nrese_rdf::{NamedNode, NamedOrBlankNode, Term, Triple};
use nrese_reasoner::rulesets::Ruleset;
use nrese_reasoner::{ReasonerConfig, ReasonerService};
use nrese_store::{MutationPipeline, StoreConfig, StoreError, StoreService};

/// `ex:x` a member of a declared union class by `ex:p`'s range: OWL 2 RL makes it an
/// `owl:Thing`, and under `skip` the rewrite of its hidden membership does.
const DATA: &str = "PREFIX ex: <http://example.com/> \
    PREFIX rdfs: <http://www.w3.org/2000/01/rdf-schema#> \
    PREFIX owl: <http://www.w3.org/2002/07/owl#> \
    INSERT DATA { ex:p rdfs:range [ a owl:Class ; owl:unionOf ( ex:A ex:B ) ] . ex:s ex:p ex:x }";
const THING: &str = "ASK { <http://example.com/x> a <http://www.w3.org/2002/07/owl#Thing> }";

/// What a call came to: `panicked: …`, `Ok` or `Err(…)`.
fn outcome<T>(done: &std::thread::Result<Result<T, StoreError>>) -> String {
    match done {
        Err(panic) => {
            let text = panic.downcast_ref::<&str>().map(|s| (*s).to_owned());
            let text = text.or_else(|| panic.downcast_ref::<String>().cloned());
            format!("panicked: {}", text.unwrap_or_default())
        }
        Ok(Ok(_)) => "Ok".to_owned(),
        Ok(Err(error)) => format!("Err({error:?})"),
    }
}

/// One check's line, `CASE {case}: right|WRONG: {got}, must be {expected}`; a wrong one also
/// goes to `wrong`.
fn check(wrong: &mut Vec<String>, case: &str, got: &str, right: bool, expected: &str) {
    let verdict = if right { "right" } else { "WRONG" };
    println!("CASE {case}: {verdict}: {got}, must be {expected}");
    if !right {
        wrong.push(case.to_owned());
    }
}

/// Whether the call answered the process's memory limit's typed refusal.
fn refused<T>(done: &std::thread::Result<Result<T, StoreError>>) -> bool {
    matches!(done, Ok(Err(StoreError::ProcessMemoryLimit { .. })))
}

fn holds(store: &StoreService, ask: &str) -> bool {
    let answer = store.execute_query_str(ask).expect("the ask").payload;
    String::from_utf8(answer).expect("utf-8").contains("true")
}

#[test]
fn over_the_memory_limit_skip_reasoning_is_refused_by_type_and_under_it_answers() {
    if !READS_MEMORY {
        return;
    }
    let config = StoreConfig {
        hide_unnamed_classes: true,
        ..StoreConfig::in_memory()
    };
    let store = StoreService::new(config).expect("the store opens");
    // Asserted only: written without reasoning, the closure made below.
    let unreasoned = Arc::new(ReasonerService::new(ReasonerConfig::default()));
    let writer = MutationPipeline::new(Arc::new(store.clone()), unreasoned);
    writer.execute_update_str(DATA).expect("the data");
    let mut wrong = Vec::new();

    set_process_limit(1);
    let done = catch_unwind(AssertUnwindSafe(|| store.rematerialise(Ruleset::Owl2Rl)));
    set_process_limit(0);
    let typed = "the typed memory refusal";
    check(
        &mut wrong,
        "rematerialise at limit 1",
        &outcome(&done),
        refused(&done),
        typed,
    );
    let (current, thing) = (
        store.reasoning_is_current(Ruleset::Owl2Rl),
        holds(&store, THING),
    );
    let got = format!("current {current}, ex:x a owl:Thing {thing}");
    check(
        &mut wrong,
        "nothing published",
        &got,
        !current && !thing,
        "neither",
    );

    let done = catch_unwind(AssertUnwindSafe(|| store.rematerialise(Ruleset::Owl2Rl)));
    let got = format!(
        "{}, ex:x a owl:Thing {}",
        outcome(&done),
        holds(&store, THING)
    );
    let right = matches!(done, Ok(Ok(_))) && holds(&store, THING);
    check(
        &mut wrong,
        "rematerialise without a limit",
        &got,
        right,
        "Ok, true",
    );

    // `ex:a owl:differentFrom ex:b`, decided by refuting `ex:a owl:sameAs ex:b`.
    let ex = |local: &str| NamedNode::new_unchecked(format!("http://example.com/{local}"));
    let different = NamedNode::new_unchecked("http://www.w3.org/2002/07/owl#differentFrom");
    let conclusion = Triple::new(
        NamedOrBlankNode::NamedNode(ex("a")),
        different,
        Term::NamedNode(ex("b")),
    );
    set_process_limit(1);
    let done = catch_unwind(AssertUnwindSafe(|| {
        store.entails(Ruleset::Owl2Rl, &[conclusion])
    }));
    set_process_limit(0);
    check(
        &mut wrong,
        "entails at limit 1",
        &outcome(&done),
        refused(&done),
        typed,
    );
    assert!(wrong.is_empty(), "wrong: {wrong:?}");
}
