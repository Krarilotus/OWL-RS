//! The `owl2-dl` entailment check at its seams: its time is one budget, from its start, and
//! it reads one revision.

use std::sync::Arc;
use std::time::Duration;

use nrese_rdf::{BlankNode, NamedNode, Triple};
use nrese_reasoner::{ReasonerConfig, ReasonerService, ReasoningMode};

use crate::dl::entailment::Entailed;
use crate::service::hooks;
use crate::{
    DlConfig, MutationCommand, MutationPipeline, MutationTicket, Requester, SparqlUpdateRequest,
    StoreConfig, StoreService,
};

const PREFIXES: &str = "PREFIX : <http://example.com/> \
     PREFIX owl: <http://www.w3.org/2002/07/owl#> \
     PREFIX rdfs: <http://www.w3.org/2000/01/rdf-schema#> ";

/// `:C ⊑ ∃p.A`, the range of `:p` is `:B`, `:A` disjoint with `:B`: `:C` is empty, so
/// `:a a ¬C` follows, and only the DL engines show it.
const EMPTY_C: &str = ":p a owl:ObjectProperty ; rdfs:range :B . :A owl:disjointWith :B . \
     :C rdfs:subClassOf [ a owl:Restriction ; owl:onProperty :p ; owl:someValuesFrom :A ] . \
     :a a :D .";

/// A store under `owl2-dl` with `dl`, its pipeline, `data` committed.
fn dl_store(dl: DlConfig, data: &str) -> (Arc<StoreService>, MutationPipeline) {
    let store = Arc::new(
        StoreService::new(StoreConfig {
            dl,
            ..StoreConfig::in_memory()
        })
        .unwrap(),
    );
    let pipeline = MutationPipeline::new(
        Arc::clone(&store),
        Arc::new(ReasonerService::new(ReasonerConfig::for_mode(
            ReasoningMode::Owl2Dl,
        ))),
    );
    update(&pipeline, &format!("INSERT DATA {{ {data} }}"));
    (store, pipeline)
}

fn update(pipeline: &MutationPipeline, text: &str) {
    pipeline
        .apply(
            MutationCommand::Update(SparqlUpdateRequest::new(format!("{PREFIXES}{text}"))),
            &Requester::all(),
            &MutationTicket::new(),
        )
        .unwrap();
}

fn iri(local: &str) -> NamedNode {
    NamedNode::new_unchecked(format!("http://example.com/{local}"))
}

/// `:a a [ owl:complementOf :class ]`.
fn not_in(a: &str, class: &str) -> Vec<Triple> {
    let complement = BlankNode::new_unchecked("c");
    vec![
        Triple::new(iri(a), nrese_rdf::vocab::rdf::TYPE, complement.clone()),
        Triple::new(complement, nrese_rdf::vocab::owl::COMPLEMENT_OF, iri(class)),
    ]
}

/// Spec (`entails_dl`: decided within `dl.timeout` per test, the operation's remaining time
/// for each nested check, `Budget::remaining`): when the premise's check leaves no time, the
/// conclusion's tests decide nothing; with the time they need, they decide it.
#[test]
fn the_conclusion_is_tested_in_what_the_premise_check_left() {
    let dl = DlConfig {
        timeout: Some(Duration::from_millis(300)),
        ..DlConfig::default()
    };
    let (store, _pipeline) = dl_store(dl, EMPTY_C);
    let conclusion = not_in("a", "C");
    assert_eq!(
        store.entails_dl(&conclusion).unwrap().answer,
        Entailed::Yes,
        "with time"
    );
    hooks::AFTER_DL_PREMISE.set(Some(Box::new(|| {
        std::thread::sleep(Duration::from_millis(600));
    })));
    let late = store.entails_dl(&conclusion).unwrap().answer;
    hooks::AFTER_DL_PREMISE.set(None);
    assert!(
        matches!(late, Entailed::Unknown(_)),
        "past the deadline: {late:?}"
    );
}

/// Spec (`entailment` module docs: what the DL engines can't decide is undecided): where the
/// DL check runs out of time after the premise's check, a complement only it shows is
/// undecided, never denied.
#[test]
fn a_dl_check_out_of_time_leaves_the_answer_undecided() {
    let dl = DlConfig {
        timeout: Some(Duration::from_millis(300)),
        ..DlConfig::default()
    };
    let (store, _pipeline) = dl_store(dl, EMPTY_C);
    let rules = ReasonerConfig::for_mode(ReasoningMode::Owl2Dl)
        .materialised_program()
        .unwrap();
    hooks::AFTER_DL_PREMISE.set(Some(Box::new(|| {
        std::thread::sleep(Duration::from_millis(600));
    })));
    let answer = store.entails(rules, &not_in("a", "C"));
    hooks::AFTER_DL_PREMISE.set(None);
    assert!(
        matches!(answer, Err(crate::StoreError::Undecided(_))),
        "{answer:?}"
    );
}

/// The store's check of `conclusion` under `owl2-dl` with `dl.timeout` 300 ms over `data`,
/// its rules' pass taking 600 ms when `late`.
fn entails_in_300_ms(data: &str, conclusion: &[Triple], late: bool) -> String {
    let dl = DlConfig {
        timeout: Some(Duration::from_millis(300)),
        ..DlConfig::default()
    };
    let (store, _pipeline) = dl_store(dl, data);
    let rules = ReasonerConfig::for_mode(ReasoningMode::Owl2Dl)
        .materialised_program()
        .unwrap();
    if late {
        hooks::BETWEEN_ENTAILMENT_HALVES.set(Some(Box::new(|| {
            std::thread::sleep(Duration::from_millis(600));
        })));
    }
    let answer = store.entails(rules, conclusion);
    hooks::BETWEEN_ENTAILMENT_HALVES.set(None);
    match answer {
        Err(crate::StoreError::Undecided(_)) => "undecided".to_owned(),
        other => format!("{other:?}"),
    }
}

/// The ground statement `:s <predicate> <object>`, both full IRIs.
fn ground(s: &str, predicate: &str, object: &str) -> Triple {
    let named = NamedNode::new_unchecked;
    Triple::new(iri(s), named(predicate), named(object))
}

const OWL: &str = "http://www.w3.org/2002/07/owl#";

/// Spec (`entails` under `owl2-dl`: one `dl.timeout` from the call, which the rules' pass
/// spends too): where the rules leave a ground conclusion (`:C ⊑ ⊥`) to the DL engines and
/// their pass uses up the time, the DL check gets none and the answer is undecided; with
/// the time, entailed.
#[test]
fn a_rules_pass_that_uses_up_the_time_leaves_the_dl_check_none() {
    let conclusion = [ground(
        "C",
        "http://www.w3.org/2000/01/rdf-schema#subClassOf",
        &format!("{OWL}Nothing"),
    )];
    assert_eq!(
        [false, true].map(|late| entails_in_300_ms(EMPTY_C, &conclusion, late)),
        ["Ok(Entailed)".to_owned(), "undecided".to_owned()]
    );
}

/// Spec (as above): an answer the rules find after the time is up is no answer either;
/// within it, entailed.
#[test]
fn a_rules_answer_past_the_time_is_undecided() {
    let data = ":D rdfs:subClassOf :C . :x a :D .";
    let conclusion = vec![Triple::new(iri("x"), nrese_rdf::vocab::rdf::TYPE, iri("C"))];
    assert_eq!(
        [false, true].map(|late| entails_in_300_ms(data, &conclusion, late)),
        ["Ok(Entailed)".to_owned(), "undecided".to_owned()]
    );
}

/// Spec (`entailment` module docs: under `owl2-dl` the rules decide plain ground
/// assertions): a ground conclusion the rules show is entailed with no DL check.
#[test]
fn a_ground_conclusion_the_rules_show_takes_no_dl_check() {
    let (store, _pipeline) = dl_store(DlConfig::default(), ":D rdfs:subClassOf :C . :x a :D .");
    let rules = ReasonerConfig::for_mode(ReasoningMode::Owl2Dl)
        .materialised_program()
        .unwrap();
    let checked = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let seen = Arc::clone(&checked);
    hooks::AFTER_DL_PREMISE.set(Some(Box::new(move || {
        seen.store(true, std::sync::atomic::Ordering::Relaxed);
    })));
    let answer = store.entails(
        rules,
        &[Triple::new(iri("x"), nrese_rdf::vocab::rdf::TYPE, iri("C"))],
    );
    hooks::AFTER_DL_PREMISE.set(None);
    assert_eq!(
        (
            format!("{answer:?}"),
            checked.load(std::sync::atomic::Ordering::Relaxed)
        ),
        ("Ok(Entailed)".to_owned(), false),
        "the answer, whether the DL check ran"
    );
}

/// Spec (`entails`: one certified closure, reads that answer only for it): `:x a :P` and
/// `:x ≠ :y` hold together in neither revision, the one with `:x a :P` (where `:x` may be
/// `:y`) nor the one after a commit that replaces it with `:x a :D` and `:y a :C` (`:D`
/// disjoint with `:C`). With that commit between the closure's read and the refutation, the
/// check gives no answer (undecided), never `Entailed`.
#[test]
fn a_commit_between_the_closure_and_the_refutation_leaves_the_answer_undecided() {
    let (store, pipeline) = dl_store(DlConfig::default(), ":D owl:disjointWith :C . :x a :P .");
    let conclusion = [
        Triple::new(iri("x"), nrese_rdf::vocab::rdf::TYPE, iri("P")),
        ground("x", &format!("{OWL}differentFrom"), "http://example.com/y"),
    ];
    let rules = ReasonerConfig::for_mode(ReasoningMode::Owl2Dl)
        .materialised_program()
        .unwrap();
    hooks::BETWEEN_ENTAILMENT_HALVES.set(Some(Box::new(move || {
        update(
            &pipeline,
            "DELETE DATA { :x a :P } ; INSERT DATA { :x a :D . :y a :C }",
        );
    })));
    let answer = store.entails(rules, &conclusion);
    hooks::BETWEEN_ENTAILMENT_HALVES.set(None);
    assert!(
        matches!(answer, Err(crate::StoreError::Undecided(_))),
        "{answer:?}"
    );
}
