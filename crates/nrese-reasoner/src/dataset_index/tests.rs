use crate::dataset_index::IndexedDataset;
use crate::test_support::OwnedSnapshot;
use crate::vocabulary::{
    OWL_ALL_DIFFERENT, OWL_ALL_DISJOINT_CLASSES, OWL_ALL_DISJOINT_PROPERTIES,
    OWL_ASYMMETRIC_PROPERTY, OWL_DIFFERENT_FROM, OWL_DISJOINT_WITH, OWL_EQUIVALENT_CLASS,
    OWL_EQUIVALENT_PROPERTY, OWL_FUNCTIONAL_PROPERTY, OWL_INVERSE_FUNCTIONAL_PROPERTY,
    OWL_INVERSE_OF, OWL_IRREFLEXIVE_PROPERTY, OWL_MEMBERS, OWL_PROPERTY_DISJOINT_WITH,
    OWL_REFLEXIVE_PROPERTY, OWL_SAME_AS, OWL_SYMMETRIC_PROPERTY, OWL_TRANSITIVE_PROPERTY,
    RDF_FIRST, RDF_REST, RDF_TYPE, RDFS_DOMAIN, RDFS_RANGE, RDFS_SUBCLASS_OF, RDFS_SUBPROPERTY_OF,
};

#[test]
fn indexed_dataset_collects_rule_relevant_edges() {
    let snapshot = OwnedSnapshot::with_revision_and_unsupported(
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
            (
                "http://example.com/Child",
                "http://www.w3.org/2002/07/owl#disjointWith",
                "http://example.com/Other",
            ),
            (
                "http://example.com/p2",
                OWL_PROPERTY_DISJOINT_WITH,
                "http://example.com/p10",
            ),
            (
                "http://example.com/p1",
                "http://www.w3.org/2000/01/rdf-schema#subPropertyOf",
                "http://example.com/p2",
            ),
            (
                "http://example.com/p2",
                "http://www.w3.org/2002/07/owl#equivalentProperty",
                "http://example.com/p3",
            ),
            (
                "http://example.com/p2",
                "http://www.w3.org/2000/01/rdf-schema#domain",
                "http://example.com/Person",
            ),
            (
                "http://example.com/p2",
                "http://www.w3.org/2000/01/rdf-schema#range",
                "http://example.com/Document",
            ),
            (
                "http://example.com/alice",
                "http://example.com/p1",
                "http://example.com/bob",
            ),
            (
                "http://example.com/alice",
                OWL_SAME_AS,
                "http://example.com/alicia",
            ),
            (
                "http://example.com/alice",
                OWL_DIFFERENT_FROM,
                "http://example.com/ally",
            ),
            (
                "http://example.com/Child",
                "http://www.w3.org/2002/07/owl#equivalentClass",
                "http://example.com/Human",
            ),
            (
                "http://example.com/p3",
                "http://www.w3.org/2002/07/owl#inverseOf",
                "http://example.com/p4",
            ),
            ("http://example.com/p4", RDF_TYPE, OWL_FUNCTIONAL_PROPERTY),
            (
                "http://example.com/p5",
                RDF_TYPE,
                OWL_INVERSE_FUNCTIONAL_PROPERTY,
            ),
            ("http://example.com/p6", RDF_TYPE, OWL_IRREFLEXIVE_PROPERTY),
            ("http://example.com/p7", RDF_TYPE, OWL_ASYMMETRIC_PROPERTY),
            ("http://example.com/p7r", RDF_TYPE, OWL_REFLEXIVE_PROPERTY),
            ("http://example.com/p8", RDF_TYPE, OWL_SYMMETRIC_PROPERTY),
            ("http://example.com/p9", RDF_TYPE, OWL_TRANSITIVE_PROPERTY),
        ],
        3,
    );

    let index = IndexedDataset::from_snapshot(&snapshot);

    assert_eq!(index.supported_asserted_triples(), 20);
    assert_eq!(index.unsupported_asserted_triples(), 3);
    assert_eq!(index.subclass_edge_count(), 3);
    assert_eq!(index.subproperty_edge_count(), 3);
    assert_eq!(index.type_assertion_count(), 1);
    assert_eq!(index.property_assertion_count(), 1);
    assert_eq!(index.same_as_assertion_count(), 1);
    assert_eq!(index.domain_assertion_count(), 1);
    assert_eq!(index.range_assertion_count(), 1);
    assert_eq!(index.functional_properties().len(), 1);
    assert_eq!(index.inverse_functional_properties().len(), 1);
    assert_eq!(index.irreflexive_properties().len(), 1);
    assert_eq!(index.asymmetric_properties().len(), 1);
    assert_eq!(index.reflexive_properties().len(), 1);
    assert_eq!(index.different_from_pairs().len(), 1);
    assert!(index.observed_named_resources().len() >= 3);
    assert!(index.symbols().len() >= 5);
    assert_eq!(index.symbols().resolve(index.rdf_type_id()), Some(RDF_TYPE));
    assert_eq!(
        index.symbols().resolve(index.rdfs_subclass_of_id()),
        Some(RDFS_SUBCLASS_OF)
    );
    assert_eq!(
        index.symbols().resolve(index.rdfs_subproperty_of_id()),
        Some(RDFS_SUBPROPERTY_OF)
    );
    assert_eq!(
        index.symbols().resolve(index.vocabulary.rdfs_domain_id),
        Some(RDFS_DOMAIN)
    );
    assert_eq!(
        index.symbols().resolve(index.vocabulary.rdfs_range_id),
        Some(RDFS_RANGE)
    );
    assert_eq!(
        index
            .symbols()
            .resolve(index.vocabulary.owl_disjoint_with_id),
        Some(OWL_DISJOINT_WITH)
    );
    assert_eq!(
        index
            .symbols()
            .resolve(index.vocabulary.owl_equivalent_class_id),
        Some(OWL_EQUIVALENT_CLASS)
    );
    assert_eq!(
        index
            .symbols()
            .resolve(index.vocabulary.owl_equivalent_property_id),
        Some(OWL_EQUIVALENT_PROPERTY)
    );
    assert_eq!(
        index.symbols().resolve(index.vocabulary.owl_inverse_of_id),
        Some(OWL_INVERSE_OF)
    );
    assert_eq!(
        index
            .symbols()
            .resolve(index.vocabulary.owl_symmetric_property_id),
        Some(OWL_SYMMETRIC_PROPERTY)
    );
    assert_eq!(
        index
            .symbols()
            .resolve(index.vocabulary.owl_transitive_property_id),
        Some(OWL_TRANSITIVE_PROPERTY)
    );
    let disjoint_targets = index
        .disjoint_class_pairs()
        .values()
        .flatten()
        .filter_map(|target_id| index.symbols().resolve(*target_id))
        .collect::<Vec<_>>();
    assert!(disjoint_targets.contains(&"http://example.com/Other"));
    assert!(
        index
            .property_disjoint_pairs()
            .values()
            .flatten()
            .filter_map(|target_id| index.symbols().resolve(*target_id))
            .any(|iri| iri == "http://example.com/p10")
    );
    assert!(
        index
            .inverse_of_pairs()
            .values()
            .flatten()
            .filter_map(|target_id| index.symbols().resolve(*target_id))
            .any(|iri| iri == "http://example.com/p4")
    );
    assert!(
        index
            .symmetric_properties()
            .iter()
            .filter_map(|property_id| index.symbols().resolve(*property_id))
            .any(|iri| iri == "http://example.com/p8")
    );
    assert!(
        index
            .transitive_properties()
            .iter()
            .filter_map(|property_id| index.symbols().resolve(*property_id))
            .any(|iri| iri == "http://example.com/p9")
    );
    assert!(
        index
            .reflexive_properties()
            .iter()
            .filter_map(|property_id| index.symbols().resolve(*property_id))
            .any(|iri| iri == "http://example.com/p7r")
    );
}

#[test]
fn schema_cache_key_is_stable_for_abox_only_changes() {
    let first = OwnedSnapshot::new(vec![
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
    ]);
    let second = OwnedSnapshot::new(vec![
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
    ]);

    let first_index = IndexedDataset::from_snapshot(&first);
    let second_index = IndexedDataset::from_snapshot(&second);

    assert_eq!(
        first_index.schema_cache_key(),
        second_index.schema_cache_key()
    );
}

#[test]
fn schema_cache_key_changes_when_schema_changes() {
    let first = OwnedSnapshot::new(vec![(
        "http://example.com/Child",
        "http://www.w3.org/2000/01/rdf-schema#subClassOf",
        "http://example.com/Parent",
    )]);
    let second = OwnedSnapshot::new(vec![(
        "http://example.com/Child",
        "http://www.w3.org/2000/01/rdf-schema#subClassOf",
        "http://example.com/Ancestor",
    )]);

    let first_index = IndexedDataset::from_snapshot(&first);
    let second_index = IndexedDataset::from_snapshot(&second);

    assert_ne!(
        first_index.schema_cache_key(),
        second_index.schema_cache_key()
    );
}

#[test]
fn indexed_dataset_collects_property_chain_axiom_schema_artifacts() {
    let snapshot = OwnedSnapshot::new(vec![
        (
            "http://example.com/composed",
            "http://www.w3.org/2002/07/owl#propertyChainAxiom",
            "http://example.com/chain-head",
        ),
        (
            "http://example.com/chain-head",
            RDF_FIRST,
            "http://example.com/p1",
        ),
        (
            "http://example.com/chain-head",
            RDF_REST,
            "http://example.com/chain-tail",
        ),
        (
            "http://example.com/chain-tail",
            RDF_FIRST,
            "http://example.com/p2",
        ),
        (
            "http://example.com/chain-tail",
            RDF_REST,
            "http://www.w3.org/1999/02/22-rdf-syntax-ns#nil",
        ),
    ]);

    let index = IndexedDataset::from_snapshot(&snapshot);

    assert_eq!(
        index.property_chain_axiom_heads().len(),
        1,
        "expected one composed property entry"
    );
    assert_eq!(index.list_first_by_node().len(), 2);
    assert_eq!(index.list_rest_by_node().len(), 2);
    assert_eq!(index.property_assertion_count(), 0);
}

#[test]
fn schema_cache_key_changes_when_property_chain_schema_changes() {
    let first = OwnedSnapshot::new(vec![
        (
            "http://example.com/composed",
            "http://www.w3.org/2002/07/owl#propertyChainAxiom",
            "http://example.com/chain-head",
        ),
        (
            "http://example.com/chain-head",
            RDF_FIRST,
            "http://example.com/p1",
        ),
        (
            "http://example.com/chain-head",
            RDF_REST,
            "http://example.com/chain-tail",
        ),
        (
            "http://example.com/chain-tail",
            RDF_FIRST,
            "http://example.com/p2",
        ),
        (
            "http://example.com/chain-tail",
            RDF_REST,
            "http://www.w3.org/1999/02/22-rdf-syntax-ns#nil",
        ),
    ]);
    let second = OwnedSnapshot::new(vec![
        (
            "http://example.com/composed",
            "http://www.w3.org/2002/07/owl#propertyChainAxiom",
            "http://example.com/chain-head",
        ),
        (
            "http://example.com/chain-head",
            RDF_FIRST,
            "http://example.com/p1",
        ),
        (
            "http://example.com/chain-head",
            RDF_REST,
            "http://example.com/chain-tail",
        ),
        (
            "http://example.com/chain-tail",
            RDF_FIRST,
            "http://example.com/p3",
        ),
        (
            "http://example.com/chain-tail",
            RDF_REST,
            "http://www.w3.org/1999/02/22-rdf-syntax-ns#nil",
        ),
    ]);

    let first_index = IndexedDataset::from_snapshot(&first);
    let second_index = IndexedDataset::from_snapshot(&second);

    assert_ne!(
        first_index.schema_cache_key(),
        second_index.schema_cache_key()
    );
}

#[test]
fn indexed_dataset_expands_group_axioms_into_pairwise_constraints() {
    let snapshot = OwnedSnapshot::new(vec![
        (
            "http://example.com/all-different",
            RDF_TYPE,
            OWL_ALL_DIFFERENT,
        ),
        (
            "http://example.com/all-different",
            OWL_MEMBERS,
            "http://example.com/diff-head",
        ),
        (
            "http://example.com/diff-head",
            RDF_FIRST,
            "http://example.com/alice",
        ),
        (
            "http://example.com/diff-head",
            RDF_REST,
            "http://example.com/diff-tail",
        ),
        (
            "http://example.com/diff-tail",
            RDF_FIRST,
            "http://example.com/alicia",
        ),
        (
            "http://example.com/diff-tail",
            RDF_REST,
            "http://www.w3.org/1999/02/22-rdf-syntax-ns#nil",
        ),
        (
            "http://example.com/all-disjoint-classes",
            RDF_TYPE,
            OWL_ALL_DISJOINT_CLASSES,
        ),
        (
            "http://example.com/all-disjoint-classes",
            OWL_MEMBERS,
            "http://example.com/class-head",
        ),
        (
            "http://example.com/class-head",
            RDF_FIRST,
            "http://example.com/Child",
        ),
        (
            "http://example.com/class-head",
            RDF_REST,
            "http://example.com/class-tail",
        ),
        (
            "http://example.com/class-tail",
            RDF_FIRST,
            "http://example.com/Other",
        ),
        (
            "http://example.com/class-tail",
            RDF_REST,
            "http://www.w3.org/1999/02/22-rdf-syntax-ns#nil",
        ),
        (
            "http://example.com/all-disjoint-properties",
            RDF_TYPE,
            OWL_ALL_DISJOINT_PROPERTIES,
        ),
        (
            "http://example.com/all-disjoint-properties",
            OWL_MEMBERS,
            "http://example.com/property-head",
        ),
        (
            "http://example.com/property-head",
            RDF_FIRST,
            "http://example.com/p1",
        ),
        (
            "http://example.com/property-head",
            RDF_REST,
            "http://example.com/property-tail",
        ),
        (
            "http://example.com/property-tail",
            RDF_FIRST,
            "http://example.com/p2",
        ),
        (
            "http://example.com/property-tail",
            RDF_REST,
            "http://www.w3.org/1999/02/22-rdf-syntax-ns#nil",
        ),
    ]);

    let index = IndexedDataset::from_snapshot(&snapshot);

    assert!(index.different_from_pairs().iter().any(|&(left, right)| {
        matches!(
            (
                index.symbols().resolve(left),
                index.symbols().resolve(right)
            ),
            (
                Some("http://example.com/alice"),
                Some("http://example.com/alicia")
            )
        )
    }));
    assert!(
        index
            .disjoint_class_pairs()
            .values()
            .flatten()
            .filter_map(|target_id| index.symbols().resolve(*target_id))
            .any(|iri| iri == "http://example.com/Other")
    );
    assert!(
        index
            .property_disjoint_pairs()
            .values()
            .flatten()
            .filter_map(|target_id| index.symbols().resolve(*target_id))
            .any(|iri| iri == "http://example.com/p2")
    );
}

#[test]
fn indexed_dataset_reports_malformed_group_axiom_lists() {
    let snapshot = OwnedSnapshot::new(vec![
        (
            "http://example.com/all-different",
            RDF_TYPE,
            OWL_ALL_DIFFERENT,
        ),
        (
            "http://example.com/all-different",
            OWL_MEMBERS,
            "http://example.com/diff-head",
        ),
        (
            "http://example.com/diff-head",
            RDF_FIRST,
            "http://example.com/alice",
        ),
    ]);

    let index = IndexedDataset::from_snapshot(&snapshot);

    assert!(
        index
            .group_axiom_diagnostics()
            .iter()
            .any(|message| message.contains("owl:AllDifferent"))
    );
}
