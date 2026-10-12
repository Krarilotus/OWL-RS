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
    // Loaded past the pipeline, by a bulk load.
    let data = format!("{}\n{UNION} :x a :A , :D .", super::PREFIXES);
    crate::support::load_past_the_pipeline(dl.store(), &data);
    // A commit that leaves it inconsistent is accepted, and says so.
    insert(&dl, ":z a :B .").expect("accepted in quarantine");
    assert_eq!(status(&dl).consistency.verdict.as_str(), "inconsistent");
    // The repair makes it consistent.
    super::update(&dl, "DELETE DATA { :x a :D . }").expect("repair");
    assert_eq!(status(&dl).consistency.verdict.as_str(), "consistent");
    rejected(insert(&dl, ":x a :D ."));
}

/// The gate's incremental paths (docs/design/owl2-dl.md §9) against a cold rebuild: after
/// each commit of a random sequence of assertion changes, the gate accepts exactly what a
/// fresh store given the same data in one commit accepts. The schema needs the
/// hypertableau (a union U1 can't decide: `:r`'s ends are `:A` and `:E`, consistent
/// through `:B`, a clash in U1, which derives `:C` too) and splits the ABox into islands
/// (`:r` carries nothing between its ends, `:s` does). The
/// test counts the commits each path decided, so it can't pass by checking everything
/// whole.
#[test]
fn incremental_consistency_on_commit_is_the_cold_check() {
    const SCHEMA: &str = ":A owl:equivalentClass [ owl:unionOf ( :B :C ) ] . \
         :B owl:disjointWith :D . :C owl:disjointWith :D . :C owl:disjointWith :E . \
         :r rdfs:domain :A . :r rdfs:range :E . \
         :A rdfs:subClassOf [ a owl:Restriction ; owl:onProperty :s ; owl:allValuesFrom :D ] .";
    let classes = ["A", "B", "C", "D", "E", "F"];
    let mut engines: std::collections::HashMap<&'static str, usize> = Default::default();
    let (mut accepted, mut refused) = (0, 0);
    let seeds = std::env::var("NRESE_FUZZ_CASES")
        .ok()
        .and_then(|n| n.parse().ok())
        .unwrap_or(8u64);
    for seed in 0..seeds {
        let dl = pipeline();
        insert(&dl, SCHEMA).expect("schema");
        let mut rng = nrese_owl::fuzz::Rng::new(seed);
        let mut asserted: Vec<String> = Vec::new();
        for step in 0..16 {
            let removal = !asserted.is_empty() && rng.one_in(4);
            let (command, candidate) = if removal {
                let gone = asserted[rng.below(asserted.len() as u64) as usize].clone();
                let rest: Vec<String> = asserted.iter().filter(|f| **f != gone).cloned().collect();
                (format!("DELETE DATA {{ {gone} }}"), rest)
            } else {
                let fact = if rng.one_in(2) {
                    format!(":i{} a :{} .", rng.below(6), classes[rng.below(6) as usize])
                } else {
                    let p = if rng.one_in(2) { "r" } else { "s" };
                    format!(":i{} :{p} :i{} .", rng.below(6), rng.below(6))
                };
                let mut with = asserted.clone();
                with.push(fact.clone());
                (format!("INSERT DATA {{ {fact} }}"), with)
            };
            // The cold check: a fresh store, the schema and the data in one commit.
            let cold = pipeline();
            let cold_ok = insert(&cold, &format!("{SCHEMA} {}", candidate.join(" "))).is_ok();
            let warm_ok = super::update(&dl, &command).is_ok();
            assert_eq!(warm_ok, cold_ok, "seed {seed}, step {step}: {command}");
            if warm_ok {
                accepted += 1;
                asserted = candidate;
                *engines.entry(status(&dl).consistency.engine).or_default() += 1;
            } else {
                refused += 1;
            }
        }
    }
    eprintln!("{accepted} accepted, {refused} refused, by engine {engines:?}");
    assert!(refused > 0, "no commit was refused");
    assert!(
        engines.get("touched-islands").copied().unwrap_or(0) > 0,
        "{engines:?}"
    );
    assert!(
        engines.get("monotone").copied().unwrap_or(0) > 0,
        "{engines:?}"
    );
}
