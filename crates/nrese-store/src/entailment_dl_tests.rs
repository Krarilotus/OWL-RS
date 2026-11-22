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

/// Spec (`entails`: one certified closure, reads that answer only for it): `:x a :P` and
/// `:x a ¬C` hold together in neither revision, the one with `:x a :P` (where `:x` may be a
/// `:C`) nor the one after a commit that replaces it with `:x a :D` (`:D` disjoint with
/// `:C`). With that commit between the closure's read and the refutation, the check gives no
/// answer (undecided), never `Entailed`.
#[test]
fn a_commit_between_the_closure_and_the_refutation_leaves_the_answer_undecided() {
    let (store, pipeline) = dl_store(DlConfig::default(), ":D owl:disjointWith :C . :x a :P .");
    let mut conclusion = not_in("x", "C");
    conclusion.push(Triple::new(iri("x"), nrese_rdf::vocab::rdf::TYPE, iri("P")));
    let rules = ReasonerConfig::for_mode(ReasoningMode::Owl2Dl)
        .materialised_program()
        .unwrap();
    hooks::BETWEEN_ENTAILMENT_HALVES.set(Some(Box::new(move || {
        update(
            &pipeline,
            "DELETE DATA { :x a :P } ; INSERT DATA { :x a :D }",
        );
    })));
    let answer = store.entails(rules, &conclusion);
    hooks::BETWEEN_ENTAILMENT_HALVES.set(None);
    assert!(
        matches!(answer, Err(crate::StoreError::Undecided(_))),
        "{answer:?}"
    );
}
