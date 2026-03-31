use nrese_core::ReasonerEngine;

use crate::test_support::OwnedSnapshot;
use crate::{
    FeatureMode, ReasonerConfig, ReasonerService, ReasoningMode, RulesMvpFeaturePolicy,
    UnsupportedConstructBehavior,
};

#[test]
fn disabled_mode_produces_skip_report() {
    let service = ReasonerService::new(ReasonerConfig::for_mode(ReasoningMode::Disabled));
    let snapshot = OwnedSnapshot::empty_with_revision(42);

    let plan = service.plan(&snapshot).expect("plan should succeed");
    let output = service.run(&snapshot, &plan).expect("run should succeed");

    assert_eq!(service.mode_name(), "disabled");
    assert_eq!(output.report.revision, 42);
    assert_eq!(output.report.metrics.asserted_triples_seen, 0);
    assert_eq!(output.report.status, nrese_core::ReasonerRunStatus::Skipped);
}

#[test]
fn rules_mvp_mode_exposes_capabilities() {
    let service = ReasonerService::new(ReasonerConfig::for_mode(ReasoningMode::RulesMvp));

    assert_eq!(service.profile_name(), "nrese-rules-mvp");
    assert!(!service.capabilities().is_empty());
}

#[test]
fn rules_mvp_mode_produces_real_inference() {
    let snapshot = OwnedSnapshot::with_revision_and_unsupported(
        9,
        vec![
            (
                "http://example.com/Child",
                "http://www.w3.org/2000/01/rdf-schema#subClassOf",
                "http://example.com/Parent",
            ),
            (
                "http://example.com/Parent",
                "http://www.w3.org/2000/01/rdf-schema#subClassOf",
                "http://example.com/Ancestor",
            ),
            (
                "http://example.com/alice",
                "http://www.w3.org/1999/02/22-rdf-syntax-ns#type",
                "http://example.com/Child",
            ),
        ],
        0,
    );
    let service = ReasonerService::new(ReasonerConfig::for_mode(ReasoningMode::RulesMvp));

    let plan = service.plan(&snapshot).expect("plan");
    let output = service.run(&snapshot, &plan).expect("run");

    assert_eq!(
        output.report.status,
        nrese_core::ReasonerRunStatus::Completed
    );
    assert_eq!(output.report.metrics.inferred_triples_produced, 3);
    assert_eq!(output.inferred.derived_triples.len(), 3);
}

#[test]
fn rules_mvp_rejects_disjoint_type_conflicts() {
    let snapshot = OwnedSnapshot::with_revision_and_unsupported(
        11,
        vec![
            (
                "http://example.com/Parent",
                "http://www.w3.org/2002/07/owl#disjointWith",
                "http://example.com/Other",
            ),
            (
                "http://example.com/Child",
                "http://www.w3.org/2000/01/rdf-schema#subClassOf",
                "http://example.com/Parent",
            ),
            (
                "http://example.com/alice",
                "http://www.w3.org/1999/02/22-rdf-syntax-ns#type",
                "http://example.com/Child",
            ),
            (
                "http://example.com/alice",
                "http://www.w3.org/1999/02/22-rdf-syntax-ns#type",
                "http://example.com/Other",
            ),
        ],
        0,
    );
    let service = ReasonerService::new(ReasonerConfig::for_mode(ReasoningMode::RulesMvp));

    let plan = service.plan(&snapshot).expect("plan");
    let output = service.run(&snapshot, &plan).expect("run");

    assert_eq!(
        output.report.status,
        nrese_core::ReasonerRunStatus::Rejected
    );
    assert_eq!(output.report.metrics.consistency_violations, 1);
    assert_eq!(output.inferred.consistency_violations, 1);
}

#[test]
fn rules_mvp_reuses_cached_inference_for_identical_snapshot_key() {
    let service = ReasonerService::new(ReasonerConfig::for_mode(ReasoningMode::RulesMvp));
    let first = OwnedSnapshot::with_revision_and_unsupported(
        1,
        vec![(
            "http://example.com/alice",
            "http://www.w3.org/1999/02/22-rdf-syntax-ns#type",
            "http://example.com/Person",
        )],
        0,
    );
    let second = OwnedSnapshot::with_revision_and_unsupported(
        9,
        vec![(
            "http://example.com/alice",
            "http://www.w3.org/1999/02/22-rdf-syntax-ns#type",
            "http://example.com/Person",
        )],
        0,
    );

    let first_plan = service.plan(&first).expect("first plan");
    let first_output = service.run(&first, &first_plan).expect("first run");
    let second_plan = service.plan(&second).expect("second plan");
    let second_output = service.run(&second, &second_plan).expect("second run");

    assert!(
        !first_output
            .report
            .notes
            .iter()
            .any(|note| note.contains("reused memoized"))
    );
    assert!(
        second_output
            .report
            .notes
            .iter()
            .any(|note| note.contains("reused memoized"))
    );
}

#[test]
fn rules_mvp_does_not_reuse_cache_for_different_snapshot_content() {
    let service = ReasonerService::new(ReasonerConfig::for_mode(ReasoningMode::RulesMvp));
    let first = OwnedSnapshot::with_revision_and_unsupported(
        1,
        vec![(
            "http://example.com/alice",
            "http://www.w3.org/1999/02/22-rdf-syntax-ns#type",
            "http://example.com/Person",
        )],
        0,
    );
    let second = OwnedSnapshot::with_revision_and_unsupported(
        1,
        vec![(
            "http://example.com/bob",
            "http://www.w3.org/1999/02/22-rdf-syntax-ns#type",
            "http://example.com/Person",
        )],
        0,
    );

    let first_plan = service.plan(&first).expect("first plan");
    let _ = service.run(&first, &first_plan).expect("first run");
    let second_plan = service.plan(&second).expect("second plan");
    let second_output = service.run(&second, &second_plan).expect("second run");

    assert!(
        second_output
            .report
            .notes
            .iter()
            .any(|note| note.contains("reused memoized schema preparation artifacts"))
    );
    assert!(
        !second_output
            .report
            .notes
            .iter()
            .any(|note| note.contains("reused memoized preparation and inference artifacts"))
    );
}

#[test]
fn rules_mvp_reuses_schema_cache_for_abox_only_change() {
    let service = ReasonerService::new(ReasonerConfig::for_mode(ReasoningMode::RulesMvp));
    let first = OwnedSnapshot::with_revision_and_unsupported(
        1,
        vec![
            (
                "http://example.com/Child",
                "http://www.w3.org/2000/01/rdf-schema#subClassOf",
                "http://example.com/Parent",
            ),
            (
                "http://example.com/alice",
                "http://www.w3.org/1999/02/22-rdf-syntax-ns#type",
                "http://example.com/Child",
            ),
        ],
        0,
    );
    let second = OwnedSnapshot::with_revision_and_unsupported(
        2,
        vec![
            (
                "http://example.com/Child",
                "http://www.w3.org/2000/01/rdf-schema#subClassOf",
                "http://example.com/Parent",
            ),
            (
                "http://example.com/bob",
                "http://www.w3.org/1999/02/22-rdf-syntax-ns#type",
                "http://example.com/Child",
            ),
        ],
        0,
    );

    let first_plan = service.plan(&first).expect("first plan");
    let _ = service.run(&first, &first_plan).expect("first run");
    let second_plan = service.plan(&second).expect("second plan");
    let second_output = service.run(&second, &second_plan).expect("second run");

    assert!(
        second_output
            .report
            .notes
            .iter()
            .any(|note| note.contains("reused memoized schema preparation artifacts"))
    );
    assert!(
        !second_output
            .report
            .notes
            .iter()
            .any(|note| note.contains("reused memoized preparation and inference artifacts"))
    );
}

#[test]
fn rules_mvp_reuses_full_cache_after_alternating_between_two_snapshots() {
    let service = ReasonerService::new(ReasonerConfig::for_mode(ReasoningMode::RulesMvp));
    let first = OwnedSnapshot::with_revision_and_unsupported(
        1,
        vec![(
            "http://example.com/alice",
            "http://www.w3.org/1999/02/22-rdf-syntax-ns#type",
            "http://example.com/Person",
        )],
        0,
    );
    let second = OwnedSnapshot::with_revision_and_unsupported(
        2,
        vec![(
            "http://example.com/bob",
            "http://www.w3.org/1999/02/22-rdf-syntax-ns#type",
            "http://example.com/Person",
        )],
        0,
    );
    let first_again = OwnedSnapshot::with_revision_and_unsupported(
        3,
        vec![(
            "http://example.com/alice",
            "http://www.w3.org/1999/02/22-rdf-syntax-ns#type",
            "http://example.com/Person",
        )],
        0,
    );

    let first_plan = service.plan(&first).expect("first plan");
    let first_output = service.run(&first, &first_plan).expect("first run");
    let second_plan = service.plan(&second).expect("second plan");
    let second_output = service.run(&second, &second_plan).expect("second run");
    let third_plan = service.plan(&first_again).expect("third plan");
    let third_output = service.run(&first_again, &third_plan).expect("third run");

    assert!(
        !first_output
            .report
            .notes
            .iter()
            .any(|note| note.contains("reused memoized preparation and inference artifacts"))
    );
    assert!(
        !second_output
            .report
            .notes
            .iter()
            .any(|note| note.contains("reused memoized preparation and inference artifacts"))
    );
    assert!(
        third_output
            .report
            .notes
            .iter()
            .any(|note| note.contains("reused memoized preparation and inference artifacts"))
    );
}

#[test]
fn rules_mvp_can_disable_consistency_enforcement_via_policy() {
    let snapshot = OwnedSnapshot::with_revision_and_unsupported(
        13,
        vec![
            (
                "http://example.com/Parent",
                "http://www.w3.org/2002/07/owl#disjointWith",
                "http://example.com/Other",
            ),
            (
                "http://example.com/alice",
                "http://www.w3.org/1999/02/22-rdf-syntax-ns#type",
                "http://example.com/Parent",
            ),
            (
                "http://example.com/alice",
                "http://www.w3.org/1999/02/22-rdf-syntax-ns#type",
                "http://example.com/Other",
            ),
        ],
        0,
    );
    let service = ReasonerService::new(ReasonerConfig {
        mode: ReasoningMode::RulesMvp,
        rules_mvp: crate::RulesMvpConfig {
            preset: crate::RulesMvpPreset::Custom,
            feature_policy: RulesMvpFeaturePolicy {
                owl_consistency_check: FeatureMode::Disabled,
                ..RulesMvpFeaturePolicy::industry_default()
            },
        },
    });

    let plan = service.plan(&snapshot).expect("plan");
    let output = service.run(&snapshot, &plan).expect("run");

    assert_eq!(
        output.report.status,
        nrese_core::ReasonerRunStatus::Completed
    );
    assert_eq!(output.report.metrics.consistency_violations, 0);
    assert!(
        output
            .report
            .notes
            .iter()
            .any(|note| note.contains("consistency gates were disabled"))
    );
}

#[test]
fn rules_mvp_can_disable_unsupported_construct_diagnostics_via_policy() {
    let snapshot = OwnedSnapshot::with_revision_and_unsupported(
        14,
        vec![(
            "http://example.com/restriction",
            "http://www.w3.org/2002/07/owl#allValuesFrom",
            "http://example.com/Target",
        )],
        0,
    );
    let service = ReasonerService::new(ReasonerConfig {
        mode: ReasoningMode::RulesMvp,
        rules_mvp: crate::RulesMvpConfig {
            preset: crate::RulesMvpPreset::Custom,
            feature_policy: RulesMvpFeaturePolicy {
                unsupported_constructs: UnsupportedConstructBehavior::Ignore,
                ..RulesMvpFeaturePolicy::industry_default()
            },
        },
    });

    let plan = service.plan(&snapshot).expect("plan");
    let output = service.run(&snapshot, &plan).expect("run");

    assert!(
        output
            .inferred
            .diagnostics
            .iter()
            .all(|message| !message.contains("not implemented in rules-mvp"))
    );
    assert!(
        output
            .report
            .notes
            .iter()
            .any(|note| note.contains("unsupported-construct diagnostics were disabled"))
    );
}
