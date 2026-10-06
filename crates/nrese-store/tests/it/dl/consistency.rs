//! Step 2: consistency on commit through the DL engines, rejected as the RL gate rejects.
//! Each inconsistency here is one the OWL 2 RL rules miss (checked under `owl2-rl`), and
//! each test names the engine that must decide it (the dispatch is a performance choice:
//! the context core where the ontology is Horn, the hypertableau otherwise).

use std::sync::Arc;

use nrese_reasoner::{ReasonerConfig, ReasonerService, ReasoningMode};
use nrese_store::{DlConfig, DlConsistency, MutationError, MutationPipeline, StoreService};

use super::{ask, insert, pipeline, pipeline_with};

/// `A ⊑ B ⊔ C`, `B` and `C` disjoint from `D`: an `A` that is a `D` has no model. RL
/// can't use a union on the right, so only the DL engines see it.
const UNION: &str = ":A owl:equivalentClass [ owl:unionOf ( :B :C ) ] . \
     :B owl:disjointWith :D . :C owl:disjointWith :D .";

/// `A ⊑ ∃r.B ⊓ ∀r.C` with `B`, `C` disjoint: no `A` can exist. Horn; RL needs an `r`
/// edge to fire `cls-avf`, and the existential gives none.
const EXISTENTIAL: &str = ":A rdfs:subClassOf [ a owl:Restriction ; owl:onProperty :r ; \
       owl:someValuesFrom :B ] , \
     [ a owl:Restriction ; owl:onProperty :r ; owl:allValuesFrom :C ] . \
     :B owl:disjointWith :C .";

fn rl_pipeline() -> MutationPipeline {
    let store = StoreService::new(crate::support::in_memory_store_config()).expect("store");
    MutationPipeline::new(
        Arc::new(store),
        Arc::new(ReasonerService::new(ReasonerConfig::for_mode(
            ReasoningMode::Owl2Rl,
        ))),
    )
}

fn rejected(result: Result<u64, MutationError>) -> String {
    match result {
        Err(MutationError::Rejected(reject)) => {
            let explanation = reject.explanation.expect("an explanation");
            assert_eq!(explanation.violated_constraint, "owl2-dl-consistency");
            reject.detail
        }
        other => panic!("expected a rejection, got {other:?}"),
    }
}

fn status(pipeline: &MutationPipeline) -> nrese_store::dl::DlStatus {
    pipeline.store().dl().status().expect("a DL status")
}

#[test]
fn an_inconsistency_through_a_union_is_rejected_by_the_hypertableau() {
    let rl = rl_pipeline();
    insert(&rl, UNION).expect("schema");
    insert(&rl, ":x a :A , :D .").expect("RL alone accepts it");

    let dl = pipeline();
    let revision = insert(&dl, UNION).expect("schema");
    let current = status(&dl);
    assert_eq!(current.revision, revision);
    assert_eq!(
        current.consistency.verdict,
        nrese_store::dl::Verdict::Consistent
    );
    // The schema alone: the upper bound proves it consistent, no DL engine runs.
    assert_eq!(current.consistency.engine, "upper-bound");
    let detail = rejected(insert(&dl, ":x a :A , :D ."));
    assert!(detail.contains("OWL 2 DL"), "{detail}");
    assert!(detail.contains("hypertableau"), "{detail}");
    assert!(!ask(&dl, ":x a :D"));
    // The status still describes the committed revision.
    assert_eq!(status(&dl).revision, revision);
    insert(&dl, ":y a :A .").expect("a consistent commit");
}

#[test]
fn an_inconsistency_through_an_existential_is_rejected_by_the_context_core() {
    let rl = rl_pipeline();
    insert(&rl, &format!("{EXISTENTIAL} :x a :A .")).expect("RL alone accepts it");

    let dl = pipeline();
    insert(&dl, EXISTENTIAL).expect("schema");
    let detail = rejected(insert(&dl, ":x a :A ."));
    assert!(detail.contains("context-core"), "{detail}");
    assert!(!ask(&dl, ":x a :A"));
}

#[test]
fn with_the_check_off_commits_pass_and_the_status_is_unknown() {
    let dl = pipeline_with(DlConfig {
        consistency: DlConsistency::Off,
        ..DlConfig::default()
    });
    insert(&dl, &format!("{UNION} :x a :A , :D .")).expect("not checked");
    let current = status(&dl);
    assert_eq!(current.consistency.verdict.as_str(), "unknown");
    assert!(
        current
            .consistency
            .verdict
            .reason()
            .unwrap()
            .contains("off")
    );
}

#[test]
fn inconsistent_data_loaded_past_the_gate_is_quarantined_not_locked() {
    let dl = pipeline();
    // Loaded past the pipeline (as a bulk load or restore would).
    dl.store()
        .execute_update_str(&format!(
            "{} INSERT DATA {{ {UNION} :x a :A , :D . }}",
            super::PREFIXES
        ))
        .expect("direct write");
    // A commit that leaves it inconsistent is accepted, and says so.
    insert(&dl, ":z a :B .").expect("accepted in quarantine");
    assert_eq!(status(&dl).consistency.verdict.as_str(), "inconsistent");
    // The repair makes it consistent.
    super::update(&dl, "DELETE DATA { :x a :D . }").expect("repair");
    assert_eq!(status(&dl).consistency.verdict.as_str(), "consistent");
    rejected(insert(&dl, ":x a :D ."));
}
