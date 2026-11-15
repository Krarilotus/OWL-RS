//! The OWL 2 RL rules' clashes in data that reached the store past the commit gates (a
//! bulk load, a start-up rebuild), under `owl2-dl`: undecided where OWL 2 DL reads the
//! ontology differently, so that nothing states an unproven inconsistency, and in
//! quarantine all the same.

use std::sync::Arc;

use nrese_rdf::Triple;
use nrese_rdf_io::{RdfFormat, RdfParser};
use nrese_reasoner::{ReasonerConfig, ReasonerService, ReasoningMode, RuleProgram};
use nrese_store::{ConsistencyStatus, MutationPipeline, StoreConfig, StoreService, WhyUndecided};

use super::{PREFIXES, update};

/// An annotation property's domain, disjoint with its subject's class: the rules clash
/// (prp-dom on the annotation), while OWL 2 DL gives the axiom no meaning for annotations.
const ANNOTATION_CLASH: &str = ":note a owl:AnnotationProperty ; rdfs:domain :C . \
     :C owl:disjointWith :D . :x a :D ; :note \"v\" .";

/// A store under `mode` with `config`.
fn store(config: StoreConfig, mode: ReasoningMode) -> MutationPipeline {
    MutationPipeline::new(
        Arc::new(StoreService::new(config).expect("store")),
        Arc::new(ReasonerService::new(ReasonerConfig::for_mode(mode))),
    )
}

/// The rules the store under `owl2-dl` maintains.
fn rules() -> RuleProgram {
    ReasonerConfig::for_mode(ReasoningMode::Owl2Dl)
        .materialised_program()
        .expect("owl2-dl maintains the OWL 2 RL closure")
}

/// `data` written past the commit gates and the closure rebuilt, as a load or a start
/// does: the store's consistency.
fn loaded(pipeline: &MutationPipeline, data: &str) -> ConsistencyStatus {
    let store = pipeline.store();
    store
        .execute_update_str(&format!("{PREFIXES}INSERT DATA {{ {data} }}"))
        .expect("written past the gates");
    store.rematerialise(rules()).expect("rebuilt");
    store.consistency()
}

fn conclusion(text: &str) -> Vec<Triple> {
    RdfParser::from_format(RdfFormat::Turtle)
        .for_slice(format!("@prefix : <http://example.com/> . {text}").as_bytes())
        .map(|t| Triple::from(t.unwrap()))
        .collect()
}

/// Where the detection fires, the rules' clash is undecided, not inconsistent, and
/// entailment gives no answer (an inconsistent premise would entail everything), with the
/// one text: what is known and the ways out.
#[test]
fn a_rules_clash_by_load_that_owl2_dl_reads_differently_is_undecided() {
    let dl = store(StoreConfig::in_memory(), ReasoningMode::Owl2Dl);
    let consistency = loaded(&dl, ANNOTATION_CLASH);
    let ConsistencyStatus::Undecided { violations, why } = consistency else {
        panic!("{consistency:?}");
    };
    assert!(violations > 0);
    assert_eq!(why, WhyUndecided::ReadsDifferently);
    let error = dl
        .store()
        .entails(rules(), &conclusion(":x a :E ."))
        .expect_err("no answer on an undecided premise");
    assert_eq!(error.to_string(), why.text(violations));
}

/// Where the detection doesn't fire, a rules clash by load stays inconsistent.
#[test]
fn a_plain_rules_clash_by_load_stays_inconsistent() {
    let dl = store(StoreConfig::in_memory(), ReasoningMode::Owl2Dl);
    let consistency = loaded(&dl, ":C owl:disjointWith :D . :x a :C , :D .");
    assert!(
        matches!(consistency, ConsistencyStatus::Inconsistent { .. }),
        "{consistency:?}"
    );
}

/// An undecided store is in quarantine: a commit rebuilds it, so the one that removes the
/// clash's premises takes it out.
#[test]
fn an_undecided_store_leaves_quarantine_at_the_commit_that_repairs_it() {
    let dl = store(StoreConfig::in_memory(), ReasoningMode::Owl2Dl);
    loaded(&dl, ANNOTATION_CLASH);
    update(&dl, "DELETE DATA { :x :note \"v\" . }").expect("the repair commits");
    assert_eq!(dl.store().consistency(), ConsistencyStatus::Consistent);
}

/// A detection whose read its budget refuses leaves the clash undecided too, and says so.
#[test]
fn a_detection_refused_at_its_budget_leaves_the_clash_undecided() {
    let config = StoreConfig {
        total_query_memory_bytes: 1,
        ..StoreConfig::in_memory()
    };
    let dl = store(config, ReasoningMode::Owl2Dl);
    let consistency = loaded(&dl, ":C owl:disjointWith :D . :x a :C , :D .");
    assert!(
        matches!(
            consistency,
            ConsistencyStatus::Undecided {
                why: WhyUndecided::Unchecked,
                ..
            }
        ),
        "{consistency:?}"
    );
}

/// The reason is derived, never stored: a store opened again finds it again under
/// `owl2-dl`, and not under `owl2-rl`, whose promise is the rules' reading.
#[test]
fn the_reason_is_derived_again_at_open_under_the_mode_opened_with() {
    let dir = tempfile::tempdir().unwrap();
    let dl = store(StoreConfig::on_disk(dir.path()), ReasoningMode::Owl2Dl);
    loaded(&dl, ANNOTATION_CLASH);
    drop(dl);
    let rl = store(StoreConfig::on_disk(dir.path()), ReasoningMode::Owl2Rl);
    let consistency = rl.store().consistency();
    assert!(
        matches!(consistency, ConsistencyStatus::Inconsistent { .. }),
        "{consistency:?}"
    );
    drop(rl);
    let dl = store(StoreConfig::on_disk(dir.path()), ReasoningMode::Owl2Dl);
    let consistency = dl.store().consistency();
    assert!(
        matches!(consistency, ConsistencyStatus::Undecided { .. }),
        "{consistency:?}"
    );
}
