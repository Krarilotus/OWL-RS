//! The OWL 2 RL rules' clashes in data that reached the store past the commit gates (a
//! bulk load, a start-up rebuild), under `owl2-dl`: undecided where OWL 2 DL reads the
//! ontology differently, so that nothing states an unproven inconsistency, and in
//! quarantine all the same.

use std::sync::Arc;

use nrese_rdf::Triple;
use nrese_rdf_io::{RdfFormat, RdfParser};
use nrese_reasoner::{ReasonerConfig, ReasonerService, ReasoningMode, RuleProgram};
use nrese_store::{
    ConsistencyStatus, DlAnswers, MutationPipeline, StoreConfig, StoreError, StoreService,
    WhyUndecided,
};

use super::{PREFIXES, insert, update};

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

/// `data` loaded past the commit gates and the closure rebuilt, as a load or a start
/// does, the store keeping the mode `pipeline` runs: the store's consistency.
fn loaded(pipeline: &MutationPipeline, data: &str) -> ConsistencyStatus {
    let store = pipeline.store();
    crate::support::load_past_the_pipeline(store, &format!("{PREFIXES}{data}"));
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

/// A clash the rules find (cax-dw: `:w` in two disjoint classes), in OWL 2 RL's profile, so
/// on the rules' route; `:H`, a class nothing is asserted in; `:u`, a plain fact.
const CLASH: &str = ":A a owl:Class . :B a owl:Class . :H a owl:Class . \
     :A owl:disjointWith :B . :w a :A , :B . :u a :Thing .";

/// [`CLASH`] without the clash: `:w` in one of the classes.
const CLEAN: &str = ":A a owl:Class . :B a owl:Class . :H a owl:Class . \
     :A owl:disjointWith :B . :w a :A . :u a :Thing .";

/// A read the clash decides: no `:H` is asserted, so over consistent data there is none,
/// and over inconsistent data every individual is one (every answer is entailed).
const IN_H: &str = "SELECT ?x { ?x a :H }";

/// What a read over data the full check finds inconsistent is told (`dl::query`).
const INCONSISTENT: &str = "the data is inconsistent under OWL 2 DL, so it entails every answer";

/// A store under `owl2-dl` holding `data`, loaded past the gates and rebuilt: in quarantine.
fn quarantined(data: &str) -> MutationPipeline {
    let dl = store(StoreConfig::in_memory(), ReasoningMode::Owl2Dl);
    let consistency = loaded(&dl, data);
    assert!(
        matches!(consistency, ConsistencyStatus::Inconsistent { .. }),
        "the setup: {consistency:?}"
    );
    dl
}

/// [`IN_H`]'s rows and status under `mode`.
fn in_h(
    dl: &MutationPipeline,
    mode: Option<DlAnswers>,
) -> Result<(Vec<String>, super::Status), StoreError> {
    super::queries::query_with(dl, IN_H, mode)
}

/// Spec: under `owl2-dl` a read over data the store itself calls inconsistent is told so:
/// `sound-only`, with the full check's reason, and the rows the store holds (inconsistent
/// data entail every answer, so they are sound, and not all). The words go to the
/// documentation.
fn assert_told_inconsistent(dl: &MutationPipeline, at: &str) {
    let (rows, status) = in_h(dl, None).expect("answered");
    let header = status.shared.header();
    assert_eq!(
        (header.split(';').next(), status.reasons()),
        (Some("sound-only"), vec![INCONSISTENT.to_owned()]),
        "{at}: {header}"
    );
    assert!(rows.is_empty(), "{at}: {rows:?}");
}

/// Spec: a store in quarantine proves nothing on the rules' route; at its start, before any
/// commit, a read is told what the full check finds.
#[test]
fn a_quarantined_store_at_its_start_is_told_inconsistent() {
    let dl = quarantined(CLASH);
    assert_told_inconsistent(&dl, "the start");
}

/// Spec: `dl.answers = "exact"` refuses an answer that isn't complete (`dl::query::
/// refuse_unless_complete`), so over a store in quarantine it refuses the read.
#[test]
fn exact_answers_refuse_a_read_over_a_quarantined_store() {
    let dl = quarantined(CLASH);
    let answer = in_h(&dl, Some(DlAnswers::Exact));
    assert!(
        matches!(answer, Err(StoreError::Incomplete(_))),
        "{answer:?}"
    );
}

/// Spec: a commit into a store in quarantine on the rules' route records no DL verdict for
/// its revision (its version keeps no assessment); the reads after it are told what they
/// were told before it.
#[test]
fn a_commit_in_quarantine_records_no_verdict_and_reads_stay_told_inconsistent() {
    let dl = quarantined(CLASH);
    let revision = insert(&dl, ":v a :Thing .").expect("an unrelated commit in quarantine");
    let recorded = dl.store().dl().status();
    assert!(
        recorded.as_ref().is_none_or(|s| s.revision != revision),
        "recorded for the commit's revision {revision}: {recorded:?}"
    );
    assert_told_inconsistent(&dl, "after the commit");
    let answer = in_h(&dl, Some(DlAnswers::Exact));
    assert!(
        matches!(answer, Err(StoreError::Incomplete(_))),
        "{answer:?}"
    );
}

/// Spec: an ill-typed literal under a range is a clash the rules find (dt-not-type), and the
/// full check has no value for it (`dl/datatypes.rs`): a read is told the consistency isn't
/// known, never `complete` on the rules' claim.
#[test]
fn an_ill_typed_literal_in_quarantine_is_told_its_consistency_is_not_known() {
    let dl = quarantined(
        ":p a owl:DatatypeProperty ; rdfs:range xsd:integer . :s :p \"abc\"^^xsd:integer . \
         :H a owl:Class . :u a :Thing .",
    );
    let (rows, status) = in_h(&dl, None).expect("answered");
    let header = status.shared.header();
    let reasons = status.reasons();
    assert_eq!(header.split(';').next(), Some("sound-only"), "{header}");
    assert!(
        reasons.len() == 1
            && reasons[0].starts_with("the data's consistency under OWL 2 DL isn't known ("),
        "{header}"
    );
    assert!(rows.is_empty(), "{rows:?}");
}

/// Spec: a commit into a store in quarantine is admitted as long as the data were
/// inconsistent before it (`dl::gate`; off the route,
/// `inconsistent_data_loaded_past_the_gate_is_quarantined_not_locked`). An earlier commit on
/// the rules' route must leave no record that makes the data look consistent before the
/// next: here the next leaves the route (a union, which OWL 2 RL doesn't hold) and keeps
/// the clash, so the full check runs.
#[test]
fn a_commit_leaving_the_rules_route_in_quarantine_is_admitted_after_an_earlier_commit() {
    let dl = quarantined(CLASH);
    insert(&dl, ":v a :Thing .").expect("an unrelated commit in quarantine");
    let left = insert(&dl, ":E owl:equivalentClass [ owl:unionOf ( :F :G ) ] .");
    assert!(left.is_ok(), "{left:?}");
    let consistency = dl.store().consistency();
    assert!(
        matches!(consistency, ConsistencyStatus::Inconsistent { .. }),
        "{consistency:?}"
    );
}

/// Spec: a quarantine begins at a new revision (a rebuild that changes no rows keeps its
/// revision, one that finds a new clash has changed them), so a view of the revision before,
/// built while the data were consistent, isn't what the read after it is told.
#[test]
fn a_quarantine_beginning_after_a_read_is_told_by_its_own_revision() {
    let dl = store(StoreConfig::in_memory(), ReasoningMode::Owl2Dl);
    assert_eq!(loaded(&dl, CLEAN), ConsistencyStatus::Consistent);
    let (_, before) = in_h(&dl, None).expect("answered");
    assert!(
        before.is_complete(),
        "the setup: {}",
        before.shared.header()
    );
    let revision = dl.store().current_revision();
    let consistency = loaded(&dl, ":w a :B .");
    assert!(
        matches!(consistency, ConsistencyStatus::Inconsistent { .. }),
        "the setup: {consistency:?}"
    );
    assert!(
        dl.store().current_revision() > revision,
        "the setup: the same revision"
    );
    assert_told_inconsistent(&dl, "after the clash loaded");
}

/// Spec (the control): a clean store on the rules' route is told `complete` with no rows at
/// each point, and a commit into it is recorded `consistent`, decided by the rules.
#[test]
fn a_clean_store_on_the_rules_route_stays_complete_and_recorded() {
    let dl = store(StoreConfig::in_memory(), ReasoningMode::Owl2Dl);
    assert_eq!(loaded(&dl, CLEAN), ConsistencyStatus::Consistent);
    let complete = |at: &str| {
        let (rows, status) = in_h(&dl, None).expect("answered");
        assert!(
            status.is_complete() && rows.is_empty(),
            "{at}: {} {rows:?}",
            status.shared.header()
        );
    };
    complete("the start");
    let revision = insert(&dl, ":v a :Thing .").expect("committed");
    let recorded = dl.store().dl().status().expect("a record");
    assert_eq!(
        (
            recorded.revision,
            recorded.consistency.verdict.as_str(),
            recorded.consistency.engine
        ),
        (revision, "consistent", "rules")
    );
    complete("after the commit");
    update(&dl, "DELETE DATA { :w a :B . }").expect("committed");
    complete("after the delete");
}

/// Spec: once the repair takes a store out of quarantine, a read is told what a clean store
/// is told.
#[test]
fn a_repaired_store_is_told_what_a_clean_store_is_told() {
    let q = quarantined(CLASH);
    insert(&q, ":v a :Thing .").expect("an unrelated commit in quarantine");
    update(&q, "DELETE DATA { :w a :B . }").expect("the repair commits");
    assert_eq!(q.store().consistency(), ConsistencyStatus::Consistent);
    let clean = store(StoreConfig::in_memory(), ReasoningMode::Owl2Dl);
    assert_eq!(loaded(&clean, CLEAN), ConsistencyStatus::Consistent);
    insert(&clean, ":v a :Thing .").expect("committed");
    update(&clean, "DELETE DATA { :w a :B . }").expect("committed");
    let told = |dl: &MutationPipeline| {
        let (rows, status) = in_h(dl, None).expect("answered");
        (status.shared.header(), rows)
    };
    assert_eq!(told(&q), told(&clean));
}

/// Spec: a rebuild that finds a clash and changes no rows keeps its revision (an individual
/// different from itself: the rules check it without materialising `:w owl:sameAs :w`). A
/// view of that revision built before the rebuild, while nothing had assessed the data the
/// load brought, carries no claim past it: the read after it is told what the full check
/// finds.
#[test]
fn a_rebuild_finding_a_clash_at_a_revision_already_read_is_told_by_the_clash() {
    let dl = store(StoreConfig::in_memory(), ReasoningMode::Owl2Dl);
    assert_eq!(loaded(&dl, CLEAN), ConsistencyStatus::Consistent);
    crate::support::load_past_the_pipeline(
        dl.store(),
        &format!("{PREFIXES}:w owl:differentFrom :w ."),
    );
    let revision = dl.store().current_revision();
    in_h(&dl, None).expect("the read before the rebuild");
    dl.store().rematerialise(rules()).expect("rebuilt");
    assert_told_inconsistent(&dl, "after the rebuild");
    assert!(
        matches!(
            dl.store().consistency(),
            ConsistencyStatus::Inconsistent { .. }
        ),
        "the setup: {:?}",
        dl.store().consistency()
    );
    assert_eq!(
        dl.store().current_revision(),
        revision,
        "the setup: the rebuild took a new revision"
    );
}

/// Spec: data loaded past the gates are assessed by no one until the rebuild (the load
/// clears the certificate as it publishes): a read in between is told `not-certified`, never
/// `complete`, and `exact` refuses it; no verdict is recorded for its revision (nothing it is
/// told depends on one).
#[test]
fn a_read_after_a_load_and_before_its_rebuild_is_told_not_certified() {
    let dl = store(StoreConfig::in_memory(), ReasoningMode::Owl2Dl);
    assert_eq!(loaded(&dl, CLEAN), ConsistencyStatus::Consistent);
    crate::support::load_past_the_pipeline(dl.store(), &format!("{PREFIXES}:w a :B ."));
    let (_, status) = in_h(&dl, None).expect("answered");
    assert_eq!(status.shared.as_str(), "not-certified", "{status:?}");
    let answer = in_h(&dl, Some(DlAnswers::Exact));
    assert!(
        matches!(answer, Err(StoreError::Incomplete(_))),
        "{answer:?}"
    );
    assert!(dl.store().dl().status().is_none());
}
