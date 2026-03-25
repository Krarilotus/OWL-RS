use crate::{
    dataset_index::IndexedDataset, effective_types::EffectiveTypeSet, test_support,
    test_support::OwnedSnapshot,
};

#[test]
fn effective_types_include_asserted_domain_range_and_subclass_origins() {
    let snapshot = OwnedSnapshot::new(vec![
        (
            "http://example.com/Child",
            "http://www.w3.org/2000/01/rdf-schema#subClassOf",
            "http://example.com/Person",
        ),
        (
            "http://example.com/p1",
            "http://www.w3.org/2000/01/rdf-schema#subPropertyOf",
            "http://example.com/p2",
        ),
        (
            "http://example.com/p2",
            "http://www.w3.org/2000/01/rdf-schema#domain",
            "http://example.com/Child",
        ),
        (
            "http://example.com/p2",
            "http://www.w3.org/2000/01/rdf-schema#range",
            "http://example.com/Document",
        ),
        (
            "http://example.com/alice",
            "http://example.com/p1",
            "http://example.com/spec",
        ),
    ]);

    let index = IndexedDataset::from_snapshot(&snapshot);
    let effective = test_support::build_effective_types(&index);

    let alice_id = index
        .asserted_triples()
        .iter()
        .find_map(|(subject_id, _, _)| {
            index
                .symbols()
                .resolve(*subject_id)
                .filter(|iri| *iri == "http://example.com/alice")
                .map(|_| *subject_id)
        })
        .expect("alice id");
    let alice_types = effective.classes_for(alice_id).expect("alice types");

    assert!(alice_types.keys().any(|class_id| {
        index.symbols().resolve(*class_id) == Some("http://example.com/Child")
    }));
    assert!(alice_types.keys().any(|class_id| {
        index.symbols().resolve(*class_id) == Some("http://example.com/Person")
    }));
}

#[test]
fn equality_propagates_direct_types_between_equivalent_instances() {
    let snapshot = OwnedSnapshot::new(vec![
        (
            "http://example.com/alice",
            "http://www.w3.org/2002/07/owl#sameAs",
            "http://example.com/alicia",
        ),
        (
            "http://example.com/alicia",
            "http://www.w3.org/1999/02/22-rdf-syntax-ns#type",
            "http://example.com/Person",
        ),
    ]);

    let index = IndexedDataset::from_snapshot(&snapshot);
    let effective = test_support::build_effective_types(&index);

    let alice_id = index
        .asserted_triples()
        .iter()
        .find_map(|(subject_id, _, _)| {
            index
                .symbols()
                .resolve(*subject_id)
                .filter(|iri| *iri == "http://example.com/alice")
                .map(|_| *subject_id)
        })
        .expect("alice id");
    let alice_types = effective.classes_for(alice_id).expect("alice types");
    let person_origin = alice_types
        .iter()
        .find(|(class_id, _)| {
            index.symbols().resolve(**class_id) == Some("http://example.com/Person")
        })
        .map(|(_, origin)| origin)
        .expect("person origin");

    let description =
        EffectiveTypeSet::describe_origin(&index, person_origin).expect("origin description");
    assert!(description.contains("equality-derived"));
    assert!(description.contains("http://example.com/alicia"));
}
