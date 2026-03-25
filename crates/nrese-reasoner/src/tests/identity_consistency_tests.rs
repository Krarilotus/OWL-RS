use crate::{
    dataset_index::IndexedDataset, identity::EqualityIndex,
    identity::detect_different_from_conflicts, test_support::OwnedSnapshot,
};

#[test]
fn different_from_conflicts_with_asserted_same_as() {
    let snapshot = OwnedSnapshot::new(vec![
        (
            "http://example.com/alice",
            "http://www.w3.org/2002/07/owl#sameAs",
            "http://example.com/alicia",
        ),
        (
            "http://example.com/alice",
            "http://www.w3.org/2002/07/owl#differentFrom",
            "http://example.com/alicia",
        ),
    ]);

    let index = IndexedDataset::from_snapshot(&snapshot);
    let equality = EqualityIndex::build(&index);
    let conflicts = detect_different_from_conflicts(&index, &equality);

    assert_eq!(conflicts.len(), 1);
    assert_eq!(conflicts[0].violated_constraint, "owl:differentFrom");
    assert_eq!(conflicts[0].evidence.len(), 2);
}

#[test]
fn different_from_conflicts_with_transitive_same_as_closure() {
    let snapshot = OwnedSnapshot::new(vec![
        (
            "http://example.com/alice",
            "http://www.w3.org/2002/07/owl#sameAs",
            "http://example.com/alicia",
        ),
        (
            "http://example.com/alicia",
            "http://www.w3.org/2002/07/owl#sameAs",
            "http://example.com/ally",
        ),
        (
            "http://example.com/alice",
            "http://www.w3.org/2002/07/owl#differentFrom",
            "http://example.com/ally",
        ),
    ]);

    let index = IndexedDataset::from_snapshot(&snapshot);
    let equality = EqualityIndex::build(&index);
    let conflicts = detect_different_from_conflicts(&index, &equality);

    assert_eq!(conflicts.len(), 1);
    assert!(conflicts[0].message.contains("same effective equality set"));
}
