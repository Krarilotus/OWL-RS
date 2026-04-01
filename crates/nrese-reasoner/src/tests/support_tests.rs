use crate::dataset_index::IndexedDataset;
use crate::support::collect_unsupported_construct_diagnostics;
use crate::test_support;
use crate::test_support::OwnedSnapshot;

#[test]
fn unsupported_constructs_are_reported_deterministically() {
    let snapshot = OwnedSnapshot::new(vec![(
        "http://example.com/restriction",
        "http://www.w3.org/2002/07/owl#allValuesFrom",
        "http://example.com/Target",
    )]);

    let index = IndexedDataset::from_snapshot(&snapshot);
    let diagnostics = collect_unsupported_construct_diagnostics(
        &index,
        &test_support::default_rules_mvp_policy(),
    );

    assert_eq!(diagnostics.len(), 1);
    assert!(
        diagnostics
            .iter()
            .any(|message| message.contains("owl:allValuesFrom"))
    );
    assert!(
        !diagnostics
            .iter()
            .any(|message| message.contains("owl:FunctionalProperty"))
    );
    assert!(
        !diagnostics
            .iter()
            .any(|message| message.contains("owl:ReflexiveProperty"))
    );
    assert!(
        !diagnostics
            .iter()
            .any(|message| message.contains("owl:differentFrom"))
    );
}
