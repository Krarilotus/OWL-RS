use crate::{
    dataset_index::IndexedDataset, identity::prepare_identity,
    property_consistency::PropertyCharacteristicPlan, property_taxonomy::PropertyTaxonomyIndex,
    test_support, test_support::OwnedSnapshot,
};

#[test]
fn functional_property_implies_equality_between_objects() {
    let snapshot = OwnedSnapshot::new(vec![
        (
            "http://example.com/p",
            "http://www.w3.org/1999/02/22-rdf-syntax-ns#type",
            "http://www.w3.org/2002/07/owl#FunctionalProperty",
        ),
        (
            "http://example.com/alice",
            "http://example.com/p",
            "http://example.com/one",
        ),
        (
            "http://example.com/alice",
            "http://example.com/p",
            "http://example.com/two",
        ),
    ]);

    let index = IndexedDataset::from_snapshot(&snapshot);
    let property_taxonomy = PropertyTaxonomyIndex::from_edges(index.subproperty_edges());
    let plan = PropertyCharacteristicPlan::build(&index);
    let identity = prepare_identity(
        &index,
        &property_taxonomy,
        &plan,
        &test_support::default_rules_mvp_policy(),
    );

    assert_eq!(identity.inferred_same_as_pairs().len(), 1);
    let one_id = index
        .symbols()
        .id_of("http://example.com/one")
        .expect("one id");
    let two_id = index
        .symbols()
        .id_of("http://example.com/two")
        .expect("two id");
    assert!(identity.equality().are_equivalent(one_id, two_id));
}

#[test]
fn inverse_functional_property_implies_equality_between_subjects() {
    let snapshot = OwnedSnapshot::new(vec![
        (
            "http://example.com/p",
            "http://www.w3.org/1999/02/22-rdf-syntax-ns#type",
            "http://www.w3.org/2002/07/owl#InverseFunctionalProperty",
        ),
        (
            "http://example.com/alice",
            "http://example.com/p",
            "http://example.com/id",
        ),
        (
            "http://example.com/alicia",
            "http://example.com/p",
            "http://example.com/id",
        ),
    ]);

    let index = IndexedDataset::from_snapshot(&snapshot);
    let property_taxonomy = PropertyTaxonomyIndex::from_edges(index.subproperty_edges());
    let plan = PropertyCharacteristicPlan::build(&index);
    let identity = prepare_identity(
        &index,
        &property_taxonomy,
        &plan,
        &test_support::default_rules_mvp_policy(),
    );

    assert_eq!(identity.inferred_same_as_pairs().len(), 1);
    let alice_id = index
        .symbols()
        .id_of("http://example.com/alice")
        .expect("alice id");
    let alicia_id = index
        .symbols()
        .id_of("http://example.com/alicia")
        .expect("alicia id");
    assert!(identity.equality().are_equivalent(alice_id, alicia_id));
}

#[test]
fn functional_property_merges_multiple_objects_into_one_cluster() {
    let snapshot = OwnedSnapshot::new(vec![
        (
            "http://example.com/p",
            "http://www.w3.org/1999/02/22-rdf-syntax-ns#type",
            "http://www.w3.org/2002/07/owl#FunctionalProperty",
        ),
        (
            "http://example.com/alice",
            "http://example.com/p",
            "http://example.com/one",
        ),
        (
            "http://example.com/alice",
            "http://example.com/p",
            "http://example.com/two",
        ),
        (
            "http://example.com/alice",
            "http://example.com/p",
            "http://example.com/three",
        ),
    ]);

    let index = IndexedDataset::from_snapshot(&snapshot);
    let property_taxonomy = PropertyTaxonomyIndex::from_edges(index.subproperty_edges());
    let plan = PropertyCharacteristicPlan::build(&index);
    let identity = prepare_identity(
        &index,
        &property_taxonomy,
        &plan,
        &test_support::default_rules_mvp_policy(),
    );

    assert_eq!(identity.inferred_same_as_pairs().len(), 2);
    let one_id = index
        .symbols()
        .id_of("http://example.com/one")
        .expect("one id");
    let two_id = index
        .symbols()
        .id_of("http://example.com/two")
        .expect("two id");
    let three_id = index
        .symbols()
        .id_of("http://example.com/three")
        .expect("three id");
    assert!(identity.equality().are_equivalent(one_id, two_id));
    assert!(identity.equality().are_equivalent(one_id, three_id));
}
