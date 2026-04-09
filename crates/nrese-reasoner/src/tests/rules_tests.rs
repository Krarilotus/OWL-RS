use crate::rules::execute_rules_mvp;
use crate::test_support::OwnedSnapshot;
use nrese_core::SnapshotCoverageStats;

const RDF_TYPE: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#type";
const OWL_EQUIVALENT_CLASS: &str = "http://www.w3.org/2002/07/owl#equivalentClass";
const OWL_EQUIVALENT_PROPERTY: &str = "http://www.w3.org/2002/07/owl#equivalentProperty";
const OWL_INVERSE_OF: &str = "http://www.w3.org/2002/07/owl#inverseOf";
const OWL_ALL_DIFFERENT: &str = "http://www.w3.org/2002/07/owl#AllDifferent";
const OWL_ALL_DISJOINT_CLASSES: &str = "http://www.w3.org/2002/07/owl#AllDisjointClasses";
const OWL_ALL_DISJOINT_PROPERTIES: &str = "http://www.w3.org/2002/07/owl#AllDisjointProperties";
const OWL_DIFFERENT_FROM: &str = "http://www.w3.org/2002/07/owl#differentFrom";
const OWL_MEMBERS: &str = "http://www.w3.org/2002/07/owl#members";
const OWL_SAME_AS: &str = "http://www.w3.org/2002/07/owl#sameAs";
const OWL_REFLEXIVE_PROPERTY: &str = "http://www.w3.org/2002/07/owl#ReflexiveProperty";
const OWL_SYMMETRIC_PROPERTY: &str = "http://www.w3.org/2002/07/owl#SymmetricProperty";
const OWL_TRANSITIVE_PROPERTY: &str = "http://www.w3.org/2002/07/owl#TransitiveProperty";
const RDF_FIRST: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#first";
const RDF_REST: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#rest";
const RDF_NIL: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#nil";

#[test]
fn rules_mvp_derives_subclass_and_type_closure() {
    let snapshot = OwnedSnapshot::new(vec![
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
            RDF_TYPE,
            "http://example.com/Child",
        ),
    ]);

    let output = execute_rules_mvp(&snapshot);

    assert_eq!(output.inferred_triples, 3);
    assert!(output.derived_triples.contains(&(
        "http://example.com/Child".to_owned(),
        "http://www.w3.org/2000/01/rdf-schema#subClassOf".to_owned(),
        "http://example.com/Ancestor".to_owned()
    )));
    assert!(output.derived_triples.contains(&(
        "http://example.com/alice".to_owned(),
        RDF_TYPE.to_owned(),
        "http://example.com/Parent".to_owned()
    )));
    assert!(output.derived_triples.contains(&(
        "http://example.com/alice".to_owned(),
        RDF_TYPE.to_owned(),
        "http://example.com/Ancestor".to_owned()
    )));
    assert_eq!(output.stats.supported_asserted_triples, 3);
    assert_eq!(output.stats.unsupported_asserted_triples, 0);
    assert_eq!(output.stats.unsupported_blank_node_subjects, 0);
    assert_eq!(output.stats.unsupported_blank_node_objects, 0);
    assert_eq!(output.stats.unsupported_literal_objects, 0);
    assert_eq!(output.stats.flattened_named_graph_quads, 0);
    assert_eq!(output.stats.subclass_edge_count, 2);
    assert_eq!(output.stats.subproperty_edge_count, 0);
    assert_eq!(output.stats.type_assertion_count, 1);
    assert_eq!(output.stats.property_assertion_count, 0);
    assert_eq!(output.stats.equality_assertion_count, 0);
    assert_eq!(output.stats.equality_cluster_count, 0);
    assert_eq!(output.stats.domain_assertion_count, 0);
    assert_eq!(output.stats.range_assertion_count, 0);
    assert_eq!(output.stats.taxonomy_node_count, 3);
    assert_eq!(output.stats.property_taxonomy_node_count, 0);
}

#[test]
fn rules_mvp_reports_skipped_triples_in_diagnostics() {
    let snapshot = OwnedSnapshot::with_revision_and_coverage(
        1,
        vec![(
            "http://example.com/alice",
            RDF_TYPE,
            "http://example.com/Child",
        )],
        SnapshotCoverageStats {
            unsupported_triples: 2,
            unsupported_blank_node_subjects: 1,
            unsupported_literal_objects: 1,
            ..SnapshotCoverageStats::default()
        },
    );

    let output = execute_rules_mvp(&snapshot);

    assert_eq!(output.inferred_triples, 0);
    assert_eq!(output.stats.unsupported_asserted_triples, 2);
    assert_eq!(output.stats.unsupported_blank_node_subjects, 1);
    assert_eq!(output.stats.unsupported_blank_node_objects, 0);
    assert_eq!(output.stats.unsupported_literal_objects, 1);
    assert!(
        output
            .diagnostics
            .iter()
            .any(|message| message.contains("2 asserted triples were skipped"))
    );
    assert!(
        output
            .diagnostics
            .iter()
            .any(|message| message.contains("blank-node subjects"))
    );
    assert!(
        output
            .diagnostics
            .iter()
            .any(|message| message.contains("literal objects"))
    );
}

#[test]
fn rules_mvp_reports_remaining_unsupported_construct_diagnostics() {
    let snapshot = OwnedSnapshot::new(vec![(
        "http://example.com/restriction",
        "http://www.w3.org/2002/07/owl#allValuesFrom",
        "http://example.com/Target",
    )]);

    let output = execute_rules_mvp(&snapshot);

    assert_eq!(output.consistency_violations, 0);
    assert!(
        output
            .diagnostics
            .iter()
            .any(|message| message.contains("owl:allValuesFrom"))
    );
}

#[test]
fn rules_mvp_derives_binary_property_chain_axiom_assertions() {
    let snapshot = OwnedSnapshot::new(vec![
        (
            "http://example.com/ancestorLocatedIn",
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

    let output = execute_rules_mvp(&snapshot);

    assert!(output.derived_triples.contains(&(
        "http://example.com/alice".to_owned(),
        "http://example.com/ancestorLocatedIn".to_owned(),
        "http://example.com/paris".to_owned()
    )));
    assert!(
        output
            .diagnostics
            .iter()
            .all(|message| !message.contains("owl:propertyChainAxiom"))
    );
}

#[test]
fn rules_mvp_reports_unsupported_longer_property_chains() {
    let snapshot = OwnedSnapshot::new(vec![
        (
            "http://example.com/p",
            "http://www.w3.org/2002/07/owl#propertyChainAxiom",
            "http://example.com/chain-1",
        ),
        (
            "http://example.com/chain-1",
            "http://www.w3.org/1999/02/22-rdf-syntax-ns#first",
            "http://example.com/p1",
        ),
        (
            "http://example.com/chain-1",
            "http://www.w3.org/1999/02/22-rdf-syntax-ns#rest",
            "http://example.com/chain-2",
        ),
        (
            "http://example.com/chain-2",
            "http://www.w3.org/1999/02/22-rdf-syntax-ns#first",
            "http://example.com/p2",
        ),
        (
            "http://example.com/chain-2",
            "http://www.w3.org/1999/02/22-rdf-syntax-ns#rest",
            "http://example.com/chain-3",
        ),
        (
            "http://example.com/chain-3",
            "http://www.w3.org/1999/02/22-rdf-syntax-ns#first",
            "http://example.com/p3",
        ),
        (
            "http://example.com/chain-3",
            "http://www.w3.org/1999/02/22-rdf-syntax-ns#rest",
            "http://www.w3.org/1999/02/22-rdf-syntax-ns#nil",
        ),
    ]);

    let output = execute_rules_mvp(&snapshot);

    assert_eq!(output.consistency_violations, 0);
    assert!(
        output
            .diagnostics
            .iter()
            .any(|message| message.contains("owl:propertyChainAxiom"))
    );
}

#[test]
fn rules_mvp_supports_same_as_without_unsupported_diagnostic() {
    let snapshot = OwnedSnapshot::new(vec![
        (
            "http://example.com/alice",
            OWL_SAME_AS,
            "http://example.com/alicia",
        ),
        (
            "http://example.com/alicia",
            RDF_TYPE,
            "http://example.com/Person",
        ),
        (
            "http://example.com/alice",
            "http://example.com/knows",
            "http://example.com/bob",
        ),
    ]);

    let output = execute_rules_mvp(&snapshot);

    assert_eq!(output.consistency_violations, 0);
    assert_eq!(output.stats.equality_assertion_count, 1);
    assert_eq!(output.stats.equality_cluster_count, 1);
    assert!(output.derived_triples.contains(&(
        "http://example.com/alice".to_owned(),
        RDF_TYPE.to_owned(),
        "http://example.com/Person".to_owned(),
    )));
    assert!(output.derived_triples.contains(&(
        "http://example.com/alicia".to_owned(),
        "http://example.com/knows".to_owned(),
        "http://example.com/bob".to_owned(),
    )));
    assert!(
        !output
            .diagnostics
            .iter()
            .any(|message| message.contains("owl:sameAs"))
    );
}

#[test]
fn rules_mvp_detects_disjoint_type_inconsistency() {
    let snapshot = OwnedSnapshot::new(vec![
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
            RDF_TYPE,
            "http://example.com/Child",
        ),
        (
            "http://example.com/alice",
            RDF_TYPE,
            "http://example.com/Other",
        ),
    ]);

    let output = execute_rules_mvp(&snapshot);

    assert_eq!(output.consistency_violations, 1);
    assert!(output.primary_reject.is_some());
    assert!(
        output
            .diagnostics
            .iter()
            .any(|message| message.contains("owl:disjointWith"))
    );
}

#[test]
fn rules_mvp_detects_irreflexive_property_inconsistency() {
    let snapshot = OwnedSnapshot::new(vec![
        (
            "http://example.com/p",
            RDF_TYPE,
            "http://www.w3.org/2002/07/owl#IrreflexiveProperty",
        ),
        (
            "http://example.com/alice",
            "http://example.com/p",
            "http://example.com/alice",
        ),
    ]);

    let output = execute_rules_mvp(&snapshot);

    assert_eq!(output.consistency_violations, 1);
    assert!(
        output
            .diagnostics
            .iter()
            .any(|message| message.contains("owl:IrreflexiveProperty"))
    );
}

#[test]
fn rules_mvp_detects_asymmetric_property_inconsistency() {
    let snapshot = OwnedSnapshot::new(vec![
        (
            "http://example.com/p",
            RDF_TYPE,
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

    let output = execute_rules_mvp(&snapshot);

    assert_eq!(output.consistency_violations, 1);
    assert!(
        output
            .diagnostics
            .iter()
            .any(|message| message.contains("owl:AsymmetricProperty"))
    );
}

#[test]
fn rules_mvp_detects_property_disjointness_inconsistency() {
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

    let output = execute_rules_mvp(&snapshot);

    assert_eq!(output.consistency_violations, 1);
    assert!(
        output
            .diagnostics
            .iter()
            .any(|message| message.contains("owl:propertyDisjointWith"))
    );
}

#[test]
fn rules_mvp_derives_subproperty_transitive_closure() {
    let snapshot = OwnedSnapshot::new(vec![
        (
            "http://example.com/p1",
            "http://www.w3.org/2000/01/rdf-schema#subPropertyOf",
            "http://example.com/p2",
        ),
        (
            "http://example.com/p2",
            "http://www.w3.org/2000/01/rdf-schema#subPropertyOf",
            "http://example.com/p3",
        ),
    ]);

    let output = execute_rules_mvp(&snapshot);

    assert!(output.derived_triples.contains(&(
        "http://example.com/p1".to_owned(),
        "http://www.w3.org/2000/01/rdf-schema#subPropertyOf".to_owned(),
        "http://example.com/p3".to_owned(),
    )));
}

#[test]
fn rules_mvp_propagates_property_assertions_over_subproperty_hierarchy() {
    let snapshot = OwnedSnapshot::new(vec![
        (
            "http://example.com/p1",
            "http://www.w3.org/2000/01/rdf-schema#subPropertyOf",
            "http://example.com/p2",
        ),
        (
            "http://example.com/p2",
            "http://www.w3.org/2000/01/rdf-schema#subPropertyOf",
            "http://example.com/p3",
        ),
        (
            "http://example.com/alice",
            "http://example.com/p1",
            "http://example.com/bob",
        ),
    ]);

    let output = execute_rules_mvp(&snapshot);

    assert!(output.derived_triples.contains(&(
        "http://example.com/alice".to_owned(),
        "http://example.com/p2".to_owned(),
        "http://example.com/bob".to_owned(),
    )));
    assert!(output.derived_triples.contains(&(
        "http://example.com/alice".to_owned(),
        "http://example.com/p3".to_owned(),
        "http://example.com/bob".to_owned(),
    )));
}

#[test]
fn rules_mvp_infers_domain_and_range_types() {
    let snapshot = OwnedSnapshot::new(vec![
        (
            "http://example.com/p1",
            "http://www.w3.org/2000/01/rdf-schema#subPropertyOf",
            "http://example.com/p2",
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
            "http://example.com/spec",
        ),
    ]);

    let output = execute_rules_mvp(&snapshot);

    assert!(output.derived_triples.contains(&(
        "http://example.com/alice".to_owned(),
        RDF_TYPE.to_owned(),
        "http://example.com/Person".to_owned(),
    )));
    assert!(output.derived_triples.contains(&(
        "http://example.com/spec".to_owned(),
        RDF_TYPE.to_owned(),
        "http://example.com/Document".to_owned(),
    )));
}

#[test]
fn rules_mvp_contract_equivalent_class_propagates_types_both_directions() {
    let snapshot = OwnedSnapshot::new(vec![
        (
            "http://example.com/A",
            OWL_EQUIVALENT_CLASS,
            "http://example.com/B",
        ),
        ("http://example.com/alice", RDF_TYPE, "http://example.com/A"),
    ]);

    let output = execute_rules_mvp(&snapshot);

    assert!(output.derived_triples.contains(&(
        "http://example.com/alice".to_owned(),
        RDF_TYPE.to_owned(),
        "http://example.com/B".to_owned(),
    )));
}

#[test]
fn rules_mvp_contract_equivalent_property_propagates_assertions() {
    let snapshot = OwnedSnapshot::new(vec![
        (
            "http://example.com/p1",
            OWL_EQUIVALENT_PROPERTY,
            "http://example.com/p2",
        ),
        (
            "http://example.com/alice",
            "http://example.com/p1",
            "http://example.com/bob",
        ),
    ]);

    let output = execute_rules_mvp(&snapshot);

    assert!(output.derived_triples.contains(&(
        "http://example.com/alice".to_owned(),
        "http://example.com/p2".to_owned(),
        "http://example.com/bob".to_owned(),
    )));
}

#[test]
fn rules_mvp_contract_inverse_of_derives_inverse_assertion() {
    let snapshot = OwnedSnapshot::new(vec![
        (
            "http://example.com/p1",
            OWL_INVERSE_OF,
            "http://example.com/p2",
        ),
        (
            "http://example.com/alice",
            "http://example.com/p1",
            "http://example.com/bob",
        ),
    ]);

    let output = execute_rules_mvp(&snapshot);

    assert!(output.derived_triples.contains(&(
        "http://example.com/bob".to_owned(),
        "http://example.com/p2".to_owned(),
        "http://example.com/alice".to_owned(),
    )));
}

#[test]
fn rules_mvp_contract_symmetric_property_derives_reflected_assertion() {
    let snapshot = OwnedSnapshot::new(vec![
        ("http://example.com/knows", RDF_TYPE, OWL_SYMMETRIC_PROPERTY),
        (
            "http://example.com/alice",
            "http://example.com/knows",
            "http://example.com/bob",
        ),
    ]);

    let output = execute_rules_mvp(&snapshot);

    assert!(output.derived_triples.contains(&(
        "http://example.com/bob".to_owned(),
        "http://example.com/knows".to_owned(),
        "http://example.com/alice".to_owned(),
    )));
}

#[test]
fn rules_mvp_contract_transitive_property_derives_composed_assertion() {
    let snapshot = OwnedSnapshot::new(vec![
        (
            "http://example.com/ancestorOf",
            RDF_TYPE,
            OWL_TRANSITIVE_PROPERTY,
        ),
        (
            "http://example.com/alice",
            "http://example.com/ancestorOf",
            "http://example.com/bob",
        ),
        (
            "http://example.com/bob",
            "http://example.com/ancestorOf",
            "http://example.com/carol",
        ),
    ]);

    let output = execute_rules_mvp(&snapshot);

    assert!(output.derived_triples.contains(&(
        "http://example.com/alice".to_owned(),
        "http://example.com/ancestorOf".to_owned(),
        "http://example.com/carol".to_owned(),
    )));
}

#[test]
fn rules_mvp_contract_reflexive_property_derives_self_loops_for_observed_resources() {
    let snapshot = OwnedSnapshot::new(vec![
        ("http://example.com/p", RDF_TYPE, OWL_REFLEXIVE_PROPERTY),
        (
            "http://example.com/alice",
            "http://example.com/knows",
            "http://example.com/bob",
        ),
    ]);

    let output = execute_rules_mvp(&snapshot);

    assert!(output.derived_triples.contains(&(
        "http://example.com/alice".to_owned(),
        "http://example.com/p".to_owned(),
        "http://example.com/alice".to_owned(),
    )));
    assert!(output.derived_triples.contains(&(
        "http://example.com/bob".to_owned(),
        "http://example.com/p".to_owned(),
        "http://example.com/bob".to_owned(),
    )));
}

#[test]
fn rules_mvp_rejects_different_from_conflict_against_same_as() {
    let snapshot = OwnedSnapshot::new(vec![
        (
            "http://example.com/alice",
            OWL_SAME_AS,
            "http://example.com/alicia",
        ),
        (
            "http://example.com/alice",
            OWL_DIFFERENT_FROM,
            "http://example.com/alicia",
        ),
    ]);

    let output = execute_rules_mvp(&snapshot);

    assert_eq!(output.consistency_violations, 1);
    assert!(output.primary_reject.is_some());
    assert!(
        output
            .diagnostics
            .iter()
            .any(|message| message.contains("owl:differentFrom"))
    );
}

#[test]
fn rules_mvp_derives_same_as_from_functional_property() {
    let snapshot = OwnedSnapshot::new(vec![
        (
            "http://example.com/p",
            RDF_TYPE,
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

    let output = execute_rules_mvp(&snapshot);

    assert_eq!(output.consistency_violations, 0);
    assert!(output.primary_reject.is_none());
    assert_eq!(output.stats.inferred_equality_link_count, 1);
    assert!(output.derived_triples.contains(&(
        "http://example.com/one".to_owned(),
        OWL_SAME_AS.to_owned(),
        "http://example.com/two".to_owned(),
    )));
}

#[test]
fn rules_mvp_derives_same_as_from_inverse_functional_property() {
    let snapshot = OwnedSnapshot::new(vec![
        (
            "http://example.com/p",
            RDF_TYPE,
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

    let output = execute_rules_mvp(&snapshot);

    assert_eq!(output.consistency_violations, 0);
    assert!(output.primary_reject.is_none());
    assert_eq!(output.stats.inferred_equality_link_count, 1);
    assert!(output.derived_triples.contains(&(
        "http://example.com/alice".to_owned(),
        OWL_SAME_AS.to_owned(),
        "http://example.com/bob".to_owned(),
    )));
}

#[test]
fn rules_mvp_reuses_inferred_same_as_for_follow_on_type_propagation() {
    let snapshot = OwnedSnapshot::new(vec![
        (
            "http://example.com/p",
            RDF_TYPE,
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
        (
            "http://example.com/alicia",
            RDF_TYPE,
            "http://example.com/Person",
        ),
    ]);

    let output = execute_rules_mvp(&snapshot);

    assert_eq!(output.consistency_violations, 0);
    assert_eq!(output.stats.inferred_equality_link_count, 1);
    assert!(output.derived_triples.contains(&(
        "http://example.com/alice".to_owned(),
        RDF_TYPE.to_owned(),
        "http://example.com/Person".to_owned(),
    )));
}

#[test]
fn rules_mvp_rejects_different_from_conflict_after_functional_equality_entailment() {
    let snapshot = OwnedSnapshot::new(vec![
        (
            "http://example.com/p",
            RDF_TYPE,
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
            "http://example.com/one",
            OWL_DIFFERENT_FROM,
            "http://example.com/two",
        ),
    ]);

    let output = execute_rules_mvp(&snapshot);

    assert_eq!(output.consistency_violations, 1);
    assert!(output.primary_reject.is_some());
    assert!(
        output
            .diagnostics
            .iter()
            .any(|message| message.contains("owl:differentFrom"))
    );
}

#[test]
fn rules_mvp_rejects_instances_of_classes_closing_to_owl_nothing() {
    let snapshot = OwnedSnapshot::new(vec![
        (
            "http://example.com/Impossible",
            "http://www.w3.org/2000/01/rdf-schema#subClassOf",
            "http://www.w3.org/2002/07/owl#Nothing",
        ),
        (
            "http://example.com/alice",
            RDF_TYPE,
            "http://example.com/Impossible",
        ),
    ]);

    let output = execute_rules_mvp(&snapshot);

    assert_eq!(output.consistency_violations, 1);
    assert!(output.primary_reject.is_some());
    assert!(
        output
            .diagnostics
            .iter()
            .any(|message| message.contains("owl#Nothing"))
    );
}

#[test]
fn rules_mvp_rejects_all_different_members_that_collapse_via_equality() {
    let snapshot = OwnedSnapshot::new(vec![
        ("http://example.com/group", RDF_TYPE, OWL_ALL_DIFFERENT),
        (
            "http://example.com/group",
            OWL_MEMBERS,
            "http://example.com/group-head",
        ),
        (
            "http://example.com/group-head",
            RDF_FIRST,
            "http://example.com/alice",
        ),
        (
            "http://example.com/group-head",
            RDF_REST,
            "http://example.com/group-tail",
        ),
        (
            "http://example.com/group-tail",
            RDF_FIRST,
            "http://example.com/alicia",
        ),
        ("http://example.com/group-tail", RDF_REST, RDF_NIL),
        (
            "http://example.com/alice",
            OWL_SAME_AS,
            "http://example.com/alicia",
        ),
    ]);

    let output = execute_rules_mvp(&snapshot);

    assert_eq!(output.consistency_violations, 1);
    let reject = output.primary_reject.expect("reject explanation");
    assert_eq!(reject.violated_constraint, "owl:differentFrom");
    assert!(
        output
            .diagnostics
            .iter()
            .any(|message| message.contains("owl:differentFrom"))
    );
}

#[test]
fn rules_mvp_rejects_all_disjoint_classes_members_on_same_instance() {
    let snapshot = OwnedSnapshot::new(vec![
        (
            "http://example.com/group",
            RDF_TYPE,
            OWL_ALL_DISJOINT_CLASSES,
        ),
        (
            "http://example.com/group",
            OWL_MEMBERS,
            "http://example.com/group-head",
        ),
        (
            "http://example.com/group-head",
            RDF_FIRST,
            "http://example.com/Child",
        ),
        (
            "http://example.com/group-head",
            RDF_REST,
            "http://example.com/group-tail",
        ),
        (
            "http://example.com/group-tail",
            RDF_FIRST,
            "http://example.com/Other",
        ),
        ("http://example.com/group-tail", RDF_REST, RDF_NIL),
        (
            "http://example.com/alice",
            RDF_TYPE,
            "http://example.com/Child",
        ),
        (
            "http://example.com/alice",
            RDF_TYPE,
            "http://example.com/Other",
        ),
    ]);

    let output = execute_rules_mvp(&snapshot);

    assert_eq!(output.consistency_violations, 1);
    let reject = output.primary_reject.expect("reject explanation");
    assert_eq!(reject.violated_constraint, "owl:disjointWith");
}

#[test]
fn rules_mvp_rejects_all_disjoint_properties_for_same_assertion_pair() {
    let snapshot = OwnedSnapshot::new(vec![
        (
            "http://example.com/group",
            RDF_TYPE,
            OWL_ALL_DISJOINT_PROPERTIES,
        ),
        (
            "http://example.com/group",
            OWL_MEMBERS,
            "http://example.com/group-head",
        ),
        (
            "http://example.com/group-head",
            RDF_FIRST,
            "http://example.com/p1",
        ),
        (
            "http://example.com/group-head",
            RDF_REST,
            "http://example.com/group-tail",
        ),
        (
            "http://example.com/group-tail",
            RDF_FIRST,
            "http://example.com/p2",
        ),
        ("http://example.com/group-tail", RDF_REST, RDF_NIL),
        (
            "http://example.com/alice",
            "http://example.com/p1",
            "http://example.com/bob",
        ),
        (
            "http://example.com/alice",
            "http://example.com/p2",
            "http://example.com/bob",
        ),
    ]);

    let output = execute_rules_mvp(&snapshot);

    assert_eq!(output.consistency_violations, 1);
    let reject = output.primary_reject.expect("reject explanation");
    assert_eq!(reject.violated_constraint, "owl:propertyDisjointWith");
}

#[test]
fn rules_mvp_reports_malformed_group_axiom_lists_deterministically() {
    let snapshot = OwnedSnapshot::new(vec![
        ("http://example.com/group", RDF_TYPE, OWL_ALL_DIFFERENT),
        (
            "http://example.com/group",
            OWL_MEMBERS,
            "http://example.com/group-head",
        ),
        (
            "http://example.com/group-head",
            RDF_FIRST,
            "http://example.com/alice",
        ),
    ]);

    let output = execute_rules_mvp(&snapshot);

    assert!(
        output
            .diagnostics
            .iter()
            .any(|message| message.contains("owl:AllDifferent")),
        "expected group-axiom diagnostic, got {:?}",
        output.diagnostics
    );
}
