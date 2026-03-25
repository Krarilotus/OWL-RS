use crate::{
    class_consistency::{PreparedClassConsistency, detect_class_consistency_conflicts},
    dataset_index::IndexedDataset,
    taxonomy::TaxonomyIndex,
    test_support,
    test_support::OwnedSnapshot,
};

#[test]
fn disjoint_type_conflicts_detect_asserted_and_inferred_types() {
    let snapshot = OwnedSnapshot::new(vec![
        (
            "http://example.com/Child",
            "http://www.w3.org/2000/01/rdf-schema#subClassOf",
            "http://example.com/Parent",
        ),
        (
            "http://example.com/Parent",
            "http://www.w3.org/2002/07/owl#disjointWith",
            "http://example.com/Other",
        ),
        (
            "http://example.com/p",
            "http://www.w3.org/2000/01/rdf-schema#domain",
            "http://example.com/Other",
        ),
        (
            "http://example.com/alice",
            "http://example.com/p",
            "http://example.com/spec",
        ),
        (
            "http://example.com/alice",
            "http://www.w3.org/1999/02/22-rdf-syntax-ns#type",
            "http://example.com/Child",
        ),
    ]);

    let index = IndexedDataset::from_snapshot(&snapshot);
    let taxonomy = TaxonomyIndex::from_edges(index.subclass_edges());
    let effective_types = test_support::build_effective_types(&index);
    let prepared = PreparedClassConsistency::build(&index, &taxonomy);
    let conflicts = detect_class_consistency_conflicts(&index, &effective_types, &prepared);

    assert_eq!(conflicts.len(), 1);
    assert!(conflicts[0].message.contains("http://example.com/alice"));
    assert!(
        conflicts[0]
            .message
            .contains("http://example.com/Parent (subclass-derived")
    );
    assert!(
        conflicts[0]
            .message
            .contains("http://example.com/Other (domain-derived")
    );
    assert_eq!(conflicts[0].violated_constraint, "owl:disjointWith");
    assert_eq!(conflicts[0].focus_resource, "http://example.com/alice");
    assert_eq!(
        conflicts[0].blame.heuristic,
        "prefer-more-direct-effective-type-origin"
    );
    assert_eq!(
        conflicts[0].blame.principal_contributor,
        "http://example.com/Other"
    );
    assert!(
        conflicts[0]
            .blame
            .principal_origin
            .contains("domain-derived")
    );
    assert_eq!(conflicts[0].evidence.len(), 3);
    assert!(
        conflicts[0]
            .evidence
            .iter()
            .any(|evidence| evidence.role == "constraint-axiom")
    );
}

#[test]
fn owl_nothing_conflicts_detect_explicit_nothing_type() {
    let snapshot = OwnedSnapshot::new(vec![(
        "http://example.com/alice",
        "http://www.w3.org/1999/02/22-rdf-syntax-ns#type",
        "http://www.w3.org/2002/07/owl#Nothing",
    )]);

    let index = IndexedDataset::from_snapshot(&snapshot);
    let taxonomy = TaxonomyIndex::from_edges(index.subclass_edges());
    let effective_types = test_support::build_effective_types(&index);
    let prepared = PreparedClassConsistency::build(&index, &taxonomy);
    let conflicts = detect_class_consistency_conflicts(&index, &effective_types, &prepared);

    assert_eq!(conflicts.len(), 1);
    assert_eq!(conflicts[0].violated_constraint, "owl:Nothing");
    assert!(conflicts[0].message.contains("owl#Nothing"));
    assert!(conflicts[0].focus_resource.contains("alice"));
}

#[test]
fn owl_nothing_conflicts_detect_named_class_that_closes_to_nothing() {
    let snapshot = OwnedSnapshot::new(vec![
        (
            "http://example.com/Impossible",
            "http://www.w3.org/2000/01/rdf-schema#subClassOf",
            "http://www.w3.org/2002/07/owl#Nothing",
        ),
        (
            "http://example.com/alice",
            "http://www.w3.org/1999/02/22-rdf-syntax-ns#type",
            "http://example.com/Impossible",
        ),
    ]);

    let index = IndexedDataset::from_snapshot(&snapshot);
    let taxonomy = TaxonomyIndex::from_edges(index.subclass_edges());
    let effective_types = test_support::build_effective_types(&index);
    let prepared = PreparedClassConsistency::build(&index, &taxonomy);
    let conflicts = detect_class_consistency_conflicts(&index, &effective_types, &prepared);

    assert_eq!(conflicts.len(), 1);
    assert_eq!(conflicts[0].violated_constraint, "owl:Nothing");
    assert!(
        conflicts[0]
            .message
            .contains("http://example.com/Impossible")
    );
    assert!(conflicts[0].right_type.contains("owl#Nothing"));
}
