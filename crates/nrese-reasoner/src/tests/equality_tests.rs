use crate::dataset_index::IndexedDataset;
use crate::identity::EqualityIndex;
use crate::test_support::OwnedSnapshot;

#[test]
fn equality_index_groups_transitively_equivalent_terms() {
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
    ]);

    let index = IndexedDataset::from_snapshot(&snapshot);
    let equality = EqualityIndex::build(&index);

    assert_eq!(equality.assertion_count(), 2);
    assert_eq!(equality.cluster_count(), 1);

    let alice_id = index
        .same_as_pairs()
        .first()
        .map(|(left_id, _)| *left_id)
        .expect("alice id");
    let members = equality.equivalents_of(alice_id).expect("equality cluster");

    assert_eq!(members.len(), 3);
}
