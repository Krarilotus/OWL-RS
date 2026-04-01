use std::collections::BTreeSet;

use crate::{
    dataset_index::IndexedDataset, identity::EqualityIndex, property_chain::PropertyChainPlan,
    property_closure::PropertyClosure, property_taxonomy::PropertyTaxonomyIndex, test_support,
    test_support::OwnedSnapshot,
};

#[test]
fn property_closure_derives_inverse_symmetric_and_transitive_assertions() {
    let snapshot = OwnedSnapshot::new(vec![
        (
            "http://example.com/p1",
            "http://www.w3.org/2002/07/owl#inverseOf",
            "http://example.com/p2",
        ),
        (
            "http://example.com/p2",
            "http://www.w3.org/1999/02/22-rdf-syntax-ns#type",
            "http://www.w3.org/2002/07/owl#SymmetricProperty",
        ),
        (
            "http://example.com/p3",
            "http://www.w3.org/1999/02/22-rdf-syntax-ns#type",
            "http://www.w3.org/2002/07/owl#TransitiveProperty",
        ),
        (
            "http://example.com/a",
            "http://example.com/p1",
            "http://example.com/b",
        ),
        (
            "http://example.com/a",
            "http://example.com/p3",
            "http://example.com/b",
        ),
        (
            "http://example.com/b",
            "http://example.com/p3",
            "http://example.com/c",
        ),
    ]);

    let index = IndexedDataset::from_snapshot(&snapshot);
    let taxonomy = PropertyTaxonomyIndex::from_edges(index.subproperty_edges());
    let equality = EqualityIndex::build(&index);
    let property_chain_plan = PropertyChainPlan::build(&index);
    let closure = PropertyClosure::build(
        &index,
        &taxonomy,
        &equality,
        &property_chain_plan,
        &test_support::default_rules_mvp_policy(),
    );

    let rendered = closure
        .derived_assertions()
        .iter()
        .filter_map(|(s, p, o)| {
            Some((
                index.symbols().resolve(*s)?.to_owned(),
                index.symbols().resolve(*p)?.to_owned(),
                index.symbols().resolve(*o)?.to_owned(),
            ))
        })
        .collect::<BTreeSet<_>>();

    assert!(rendered.contains(&(
        "http://example.com/b".to_owned(),
        "http://example.com/p2".to_owned(),
        "http://example.com/a".to_owned(),
    )));
    assert!(rendered.contains(&(
        "http://example.com/a".to_owned(),
        "http://example.com/p2".to_owned(),
        "http://example.com/b".to_owned(),
    )));
    assert!(rendered.contains(&(
        "http://example.com/a".to_owned(),
        "http://example.com/p3".to_owned(),
        "http://example.com/c".to_owned(),
    )));
}

#[test]
fn property_closure_expands_assertions_across_same_as_clusters() {
    let snapshot = OwnedSnapshot::new(vec![
        (
            "http://example.com/alice",
            "http://www.w3.org/2002/07/owl#sameAs",
            "http://example.com/alicia",
        ),
        (
            "http://example.com/bob",
            "http://www.w3.org/2002/07/owl#sameAs",
            "http://example.com/robert",
        ),
        (
            "http://example.com/alice",
            "http://example.com/knows",
            "http://example.com/bob",
        ),
    ]);

    let index = IndexedDataset::from_snapshot(&snapshot);
    let taxonomy = PropertyTaxonomyIndex::from_edges(index.subproperty_edges());
    let equality = EqualityIndex::build(&index);
    let property_chain_plan = PropertyChainPlan::build(&index);
    let closure = PropertyClosure::build(
        &index,
        &taxonomy,
        &equality,
        &property_chain_plan,
        &test_support::default_rules_mvp_policy(),
    );

    let rendered = closure
        .derived_assertions()
        .iter()
        .filter_map(|(subject_id, predicate_id, object_id)| {
            Some((
                index.symbols().resolve(*subject_id)?.to_owned(),
                index.symbols().resolve(*predicate_id)?.to_owned(),
                index.symbols().resolve(*object_id)?.to_owned(),
            ))
        })
        .collect::<BTreeSet<_>>();

    assert!(rendered.contains(&(
        "http://example.com/alicia".to_owned(),
        "http://example.com/knows".to_owned(),
        "http://example.com/bob".to_owned(),
    )));
    assert!(rendered.contains(&(
        "http://example.com/alice".to_owned(),
        "http://example.com/knows".to_owned(),
        "http://example.com/robert".to_owned(),
    )));
    assert!(rendered.contains(&(
        "http://example.com/alicia".to_owned(),
        "http://example.com/knows".to_owned(),
        "http://example.com/robert".to_owned(),
    )));
}

#[test]
fn property_closure_derives_reflexive_assertions_for_observed_resources() {
    let snapshot = OwnedSnapshot::new(vec![
        (
            "http://example.com/p",
            "http://www.w3.org/1999/02/22-rdf-syntax-ns#type",
            "http://www.w3.org/2002/07/owl#ReflexiveProperty",
        ),
        (
            "http://example.com/alice",
            "http://example.com/knows",
            "http://example.com/bob",
        ),
    ]);

    let index = IndexedDataset::from_snapshot(&snapshot);
    let taxonomy = PropertyTaxonomyIndex::from_edges(index.subproperty_edges());
    let equality = EqualityIndex::build(&index);
    let property_chain_plan = PropertyChainPlan::build(&index);
    let closure = PropertyClosure::build(
        &index,
        &taxonomy,
        &equality,
        &property_chain_plan,
        &test_support::default_rules_mvp_policy(),
    );

    let rendered = closure
        .derived_assertions()
        .iter()
        .filter_map(|(subject_id, predicate_id, object_id)| {
            Some((
                index.symbols().resolve(*subject_id)?.to_owned(),
                index.symbols().resolve(*predicate_id)?.to_owned(),
                index.symbols().resolve(*object_id)?.to_owned(),
            ))
        })
        .collect::<BTreeSet<_>>();

    assert!(rendered.contains(&(
        "http://example.com/alice".to_owned(),
        "http://example.com/p".to_owned(),
        "http://example.com/alice".to_owned(),
    )));
    assert!(rendered.contains(&(
        "http://example.com/bob".to_owned(),
        "http://example.com/p".to_owned(),
        "http://example.com/bob".to_owned(),
    )));
}

#[test]
fn property_closure_derives_binary_property_chain_assertions() {
    let snapshot = OwnedSnapshot::new(vec![
        (
            "http://example.com/composed",
            "http://www.w3.org/2002/07/owl#propertyChainAxiom",
            "http://example.com/chain-head",
        ),
        (
            "http://example.com/chain-head",
            "http://www.w3.org/1999/02/22-rdf-syntax-ns#first",
            "http://example.com/parentOf",
        ),
        (
            "http://example.com/chain-head",
            "http://www.w3.org/1999/02/22-rdf-syntax-ns#rest",
            "http://example.com/chain-tail",
        ),
        (
            "http://example.com/chain-tail",
            "http://www.w3.org/1999/02/22-rdf-syntax-ns#first",
            "http://example.com/locatedIn",
        ),
        (
            "http://example.com/chain-tail",
            "http://www.w3.org/1999/02/22-rdf-syntax-ns#rest",
            "http://www.w3.org/1999/02/22-rdf-syntax-ns#nil",
        ),
        (
            "http://example.com/alice",
            "http://example.com/parentOf",
            "http://example.com/bob",
        ),
        (
            "http://example.com/bob",
            "http://example.com/locatedIn",
            "http://example.com/paris",
        ),
    ]);

    let index = IndexedDataset::from_snapshot(&snapshot);
    let taxonomy = PropertyTaxonomyIndex::from_edges(index.subproperty_edges());
    let equality = EqualityIndex::build(&index);
    let property_chain_plan = PropertyChainPlan::build(&index);
    let closure = PropertyClosure::build(
        &index,
        &taxonomy,
        &equality,
        &property_chain_plan,
        &test_support::default_rules_mvp_policy(),
    );

    let rendered = closure
        .derived_assertions()
        .iter()
        .filter_map(|(subject_id, predicate_id, object_id)| {
            Some((
                index.symbols().resolve(*subject_id)?.to_owned(),
                index.symbols().resolve(*predicate_id)?.to_owned(),
                index.symbols().resolve(*object_id)?.to_owned(),
            ))
        })
        .collect::<BTreeSet<_>>();

    assert!(rendered.contains(&(
        "http://example.com/alice".to_owned(),
        "http://example.com/composed".to_owned(),
        "http://example.com/paris".to_owned(),
    )));
}
