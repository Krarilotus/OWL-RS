use crate::{
    dataset_index::IndexedDataset, property_consistency::detect_property_characteristic_conflicts,
    test_support, test_support::OwnedSnapshot,
};

#[test]
fn detects_irreflexive_property_self_loop() {
    let snapshot = OwnedSnapshot::new(vec![
        (
            "http://example.com/p",
            "http://www.w3.org/1999/02/22-rdf-syntax-ns#type",
            "http://www.w3.org/2002/07/owl#IrreflexiveProperty",
        ),
        (
            "http://example.com/alice",
            "http://example.com/p",
            "http://example.com/alice",
        ),
    ]);

    let index = IndexedDataset::from_snapshot(&snapshot);
    let property_closure = test_support::build_property_closure(&index);
    let violations = detect_property_characteristic_conflicts(&index, &property_closure);

    assert_eq!(violations.len(), 1);
    assert!(violations[0].message.contains("owl:IrreflexiveProperty"));
}

#[test]
fn detects_asymmetric_property_reverse_pair() {
    let snapshot = OwnedSnapshot::new(vec![
        (
            "http://example.com/p",
            "http://www.w3.org/1999/02/22-rdf-syntax-ns#type",
            "http://www.w3.org/2002/07/owl#AsymmetricProperty",
        ),
        (
            "http://example.com/alice",
            "http://example.com/p",
            "http://example.com/bob",
        ),
        (
            "http://example.com/bob",
            "http://example.com/p",
            "http://example.com/alice",
        ),
    ]);

    let index = IndexedDataset::from_snapshot(&snapshot);
    let property_closure = test_support::build_property_closure(&index);
    let violations = detect_property_characteristic_conflicts(&index, &property_closure);

    assert_eq!(violations.len(), 1);
    assert!(violations[0].message.contains("owl:AsymmetricProperty"));
}

#[test]
fn does_not_treat_functional_property_multiplicity_as_consistency_violation() {
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
    let property_closure = test_support::build_property_closure(&index);
    let violations = detect_property_characteristic_conflicts(&index, &property_closure);

    assert!(violations.is_empty());
}

#[test]
fn does_not_treat_inverse_functional_property_multiplicity_as_consistency_violation() {
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
            "http://example.com/bob",
            "http://example.com/p",
            "http://example.com/id",
        ),
    ]);

    let index = IndexedDataset::from_snapshot(&snapshot);
    let property_closure = test_support::build_property_closure(&index);
    let violations = detect_property_characteristic_conflicts(&index, &property_closure);

    assert!(violations.is_empty());
}

#[test]
fn detects_property_disjointness_collision() {
    let snapshot = OwnedSnapshot::new(vec![
        (
            "http://example.com/p",
            "http://www.w3.org/2002/07/owl#propertyDisjointWith",
            "http://example.com/q",
        ),
        (
            "http://example.com/alice",
            "http://example.com/p",
            "http://example.com/bob",
        ),
        (
            "http://example.com/alice",
            "http://example.com/q",
            "http://example.com/bob",
        ),
    ]);

    let index = IndexedDataset::from_snapshot(&snapshot);
    let property_closure = test_support::build_property_closure(&index);
    let violations = detect_property_characteristic_conflicts(&index, &property_closure);

    assert_eq!(violations.len(), 1);
    assert_eq!(
        violations[0].violated_constraint,
        "owl:propertyDisjointWith"
    );
}

#[test]
fn detects_property_disjointness_across_effective_subproperties() {
    let snapshot = OwnedSnapshot::new(vec![
        (
            "http://example.com/p1",
            "http://www.w3.org/2000/01/rdf-schema#subPropertyOf",
            "http://example.com/p",
        ),
        (
            "http://example.com/q1",
            "http://www.w3.org/2000/01/rdf-schema#subPropertyOf",
            "http://example.com/q",
        ),
        (
            "http://example.com/p",
            "http://www.w3.org/2002/07/owl#propertyDisjointWith",
            "http://example.com/q",
        ),
        (
            "http://example.com/alice",
            "http://example.com/p1",
            "http://example.com/bob",
        ),
        (
            "http://example.com/alice",
            "http://example.com/q1",
            "http://example.com/bob",
        ),
    ]);

    let index = IndexedDataset::from_snapshot(&snapshot);
    let property_closure = test_support::build_property_closure(&index);
    let violations = detect_property_characteristic_conflicts(&index, &property_closure);

    assert_eq!(violations.len(), 1);
    assert_eq!(
        violations[0].violated_constraint,
        "owl:propertyDisjointWith"
    );
    assert!(violations[0].message.contains("http://example.com/p"));
    assert!(violations[0].message.contains("http://example.com/q"));
}

#[test]
fn does_not_duplicate_property_disjointness_when_axiom_is_bidirectional() {
    let snapshot = OwnedSnapshot::new(vec![
        (
            "http://example.com/p",
            "http://www.w3.org/2002/07/owl#propertyDisjointWith",
            "http://example.com/q",
        ),
        (
            "http://example.com/q",
            "http://www.w3.org/2002/07/owl#propertyDisjointWith",
            "http://example.com/p",
        ),
        (
            "http://example.com/alice",
            "http://example.com/p",
            "http://example.com/bob",
        ),
        (
            "http://example.com/alice",
            "http://example.com/q",
            "http://example.com/bob",
        ),
    ]);

    let index = IndexedDataset::from_snapshot(&snapshot);
    let property_closure = test_support::build_property_closure(&index);
    let violations = detect_property_characteristic_conflicts(&index, &property_closure);

    assert_eq!(violations.len(), 1);
    assert_eq!(
        violations[0].violated_constraint,
        "owl:propertyDisjointWith"
    );
}
