//! v1 (`rules-mvp`) test scenarios on reasoner v2, extracted from the v1 unit tests
//! before v1 was removed: each dataset, the inferences it expected and whether it
//! expected a consistency violation. v1-specific details (statistics, heuristic blame,
//! "unsupported construct" diagnostics) have no v2 counterpart. Scenarios that v1 tested
//! twice under two names are one here; each runs on the naive, batch and grouped
//! executors ([`super::scenario`]).

use super::scenario;

#[test]
fn consistency_disjoint_type_conflicts_detect_asserted_and_inferred_types() {
    scenario(
        "ex:Child rdfs:subClassOf ex:Parent
         ex:Parent owl:disjointWith ex:Other
         ex:p rdfs:domain ex:Other
         ex:alice ex:p ex:spec
         ex:alice rdf:type ex:Child",
        "",
        Some(true),
    );
}

#[test]
fn consistency_owl_nothing_conflicts_detect_explicit_nothing_type() {
    scenario("ex:alice rdf:type owl:Nothing", "", Some(true));
}

#[test]
fn consistency_owl_nothing_conflicts_detect_named_class_that_closes_to_nothing() {
    scenario(
        "ex:Impossible rdfs:subClassOf owl:Nothing
         ex:alice rdf:type ex:Impossible",
        "",
        Some(true),
    );
}

#[test]
fn equality_same_as_closes_transitively_and_symmetrically() {
    scenario(
        "ex:alice owl:sameAs ex:alicia
         ex:alicia owl:sameAs ex:ally",
        "ex:alice owl:sameAs ex:ally
         ex:ally owl:sameAs ex:alice
         ex:alicia owl:sameAs ex:alice",
        Some(false),
    );
}

#[test]
fn identity_consistency_different_from_conflicts_with_asserted_same_as() {
    scenario(
        "ex:alice owl:sameAs ex:alicia
         ex:alice owl:differentFrom ex:alicia",
        "",
        Some(true),
    );
}

#[test]
fn identity_consistency_different_from_conflicts_with_transitive_same_as_closure() {
    scenario(
        "ex:alice owl:sameAs ex:alicia
         ex:alicia owl:sameAs ex:ally
         ex:alice owl:differentFrom ex:ally",
        "",
        Some(true),
    );
}

/// A functional property's two objects are equal, which is no inconsistency.
#[test]
fn identity_inference_functional_property_implies_equality_between_objects() {
    scenario(
        "ex:p rdf:type owl:FunctionalProperty
         ex:alice ex:p ex:one
         ex:alice ex:p ex:two",
        "ex:one owl:sameAs ex:two",
        Some(false),
    );
}

/// An inverse-functional property's two subjects are equal, which is no inconsistency.
#[test]
fn identity_inference_inverse_functional_property_implies_equality_between_subjects() {
    scenario(
        "ex:p rdf:type owl:InverseFunctionalProperty
         ex:alice ex:p ex:id
         ex:bob ex:p ex:id",
        "ex:alice owl:sameAs ex:bob",
        Some(false),
    );
}

#[test]
fn identity_inference_functional_property_merges_multiple_objects_into_one_cluster() {
    scenario(
        "ex:p rdf:type owl:FunctionalProperty
         ex:alice ex:p ex:one
         ex:alice ex:p ex:two
         ex:alice ex:p ex:three",
        "ex:one owl:sameAs ex:two
         ex:two owl:sameAs ex:three
         ex:three owl:sameAs ex:one",
        Some(false),
    );
}

#[test]
fn rules_derives_binary_property_chain_axiom_assertions() {
    scenario(
        "ex:ancestorLocatedIn owl:propertyChainAxiom ex:chain-head
         ex:chain-head rdf:first ex:parentOf
         ex:chain-head rdf:rest ex:chain-tail
         ex:chain-tail rdf:first ex:locatedIn
         ex:chain-tail rdf:rest rdf:nil
         ex:alice ex:parentOf ex:bob
         ex:bob ex:locatedIn ex:paris",
        "ex:alice ex:ancestorLocatedIn ex:paris",
        Some(false),
    );
}

/// v1 reported chains longer than two as unsupported; v2 composes them.
#[test]
fn rules_derives_longer_property_chain_axiom_assertions() {
    scenario(
        "ex:composed owl:propertyChainAxiom ex:chain-1
         ex:chain-1 rdf:first ex:p1
         ex:chain-1 rdf:rest ex:chain-2
         ex:chain-2 rdf:first ex:p2
         ex:chain-2 rdf:rest ex:chain-3
         ex:chain-3 rdf:first ex:p3
         ex:chain-3 rdf:rest rdf:nil
         ex:alice ex:p1 ex:bob
         ex:bob ex:p2 ex:carol
         ex:carol ex:p3 ex:dave",
        "ex:alice ex:composed ex:dave",
        Some(false),
    );
}

#[test]
fn property_chain_property_chain_plan_reports_malformed_list() {
    scenario(
        "ex:composed owl:propertyChainAxiom ex:chain-head
         ex:chain-head rdf:first ex:p1",
        "",
        Some(false),
    );
}

#[test]
fn property_consistency_detects_irreflexive_property_self_loop() {
    scenario(
        "ex:p rdf:type owl:IrreflexiveProperty
         ex:alice ex:p ex:alice",
        "",
        Some(true),
    );
}

#[test]
fn property_consistency_detects_asymmetric_property_reverse_pair() {
    scenario(
        "ex:p rdf:type owl:AsymmetricProperty
         ex:alice ex:p ex:bob
         ex:bob ex:p ex:alice",
        "",
        Some(true),
    );
}

#[test]
fn property_consistency_detects_property_disjointness_collision() {
    scenario(
        "ex:p owl:propertyDisjointWith ex:q
         ex:alice ex:p ex:bob
         ex:alice ex:q ex:bob",
        "",
        Some(true),
    );
}

#[test]
fn property_consistency_detects_property_disjointness_across_effective_subproperties() {
    scenario(
        "ex:p1 rdfs:subPropertyOf ex:p
         ex:q1 rdfs:subPropertyOf ex:q
         ex:p owl:propertyDisjointWith ex:q
         ex:alice ex:p1 ex:bob
         ex:alice ex:q1 ex:bob",
        "",
        Some(true),
    );
}

#[test]
fn property_consistency_detects_property_disjointness_stated_both_ways() {
    scenario(
        "ex:p owl:propertyDisjointWith ex:q
         ex:q owl:propertyDisjointWith ex:p
         ex:alice ex:p ex:bob
         ex:alice ex:q ex:bob",
        "",
        Some(true),
    );
}

#[test]
fn rules_derives_subclass_and_type_closure() {
    scenario(
        "ex:Child rdfs:subClassOf ex:Parent
         ex:Parent rdfs:subClassOf ex:Ancestor
         ex:alice rdf:type ex:Child",
        "ex:Child rdfs:subClassOf ex:Ancestor
         ex:alice rdf:type ex:Parent
         ex:alice rdf:type ex:Ancestor",
        Some(false),
    );
}

/// A restriction without its class or property is no inconsistency (v1 reported it as an
/// unsupported construct).
#[test]
fn rules_accept_an_incomplete_restriction() {
    scenario(
        "ex:restriction owl:allValuesFrom ex:Target",
        "",
        Some(false),
    );
}

#[test]
fn rules_supports_same_as_without_unsupported_diagnostic() {
    scenario(
        "ex:alice owl:sameAs ex:alicia
         ex:alicia rdf:type ex:Person
         ex:alice ex:knows ex:bob",
        "ex:alice rdf:type ex:Person
         ex:alicia ex:knows ex:bob",
        Some(false),
    );
}

#[test]
fn rules_detects_disjoint_type_inconsistency() {
    scenario(
        "ex:Parent owl:disjointWith ex:Other
         ex:Child rdfs:subClassOf ex:Parent
         ex:alice rdf:type ex:Child
         ex:alice rdf:type ex:Other",
        "",
        Some(true),
    );
}

#[test]
fn rules_derives_subproperty_transitive_closure() {
    scenario(
        "ex:p1 rdfs:subPropertyOf ex:p2
         ex:p2 rdfs:subPropertyOf ex:p3",
        "ex:p1 rdfs:subPropertyOf ex:p3",
        Some(false),
    );
}

#[test]
fn rules_propagates_property_assertions_over_subproperty_hierarchy() {
    scenario(
        "ex:p1 rdfs:subPropertyOf ex:p2
         ex:p2 rdfs:subPropertyOf ex:p3
         ex:alice ex:p1 ex:bob",
        "ex:alice ex:p2 ex:bob
         ex:alice ex:p3 ex:bob",
        Some(false),
    );
}

#[test]
fn rules_infers_domain_and_range_types() {
    scenario(
        "ex:p1 rdfs:subPropertyOf ex:p2
         ex:p2 rdfs:domain ex:Person
         ex:p2 rdfs:range ex:Document
         ex:alice ex:p1 ex:spec",
        "ex:alice rdf:type ex:Person
         ex:spec rdf:type ex:Document",
        Some(false),
    );
}

#[test]
fn rules_contract_equivalent_class_propagates_types_both_directions() {
    scenario(
        "ex:A owl:equivalentClass ex:B
         ex:alice rdf:type ex:A",
        "ex:alice rdf:type ex:B",
        Some(false),
    );
    scenario(
        "ex:A owl:equivalentClass ex:B
         ex:bob rdf:type ex:B",
        "ex:bob rdf:type ex:A",
        Some(false),
    );
}

#[test]
fn rules_contract_equivalent_property_propagates_assertions_both_directions() {
    scenario(
        "ex:p1 owl:equivalentProperty ex:p2
         ex:alice ex:p1 ex:bob",
        "ex:alice ex:p2 ex:bob",
        Some(false),
    );
    scenario(
        "ex:p1 owl:equivalentProperty ex:p2
         ex:carol ex:p2 ex:dave",
        "ex:carol ex:p1 ex:dave",
        Some(false),
    );
}

#[test]
fn rules_contract_inverse_of_derives_inverse_assertions_both_directions() {
    scenario(
        "ex:p1 owl:inverseOf ex:p2
         ex:alice ex:p1 ex:bob",
        "ex:bob ex:p2 ex:alice",
        Some(false),
    );
    scenario(
        "ex:p1 owl:inverseOf ex:p2
         ex:carol ex:p2 ex:dave",
        "ex:dave ex:p1 ex:carol",
        Some(false),
    );
}

#[test]
fn rules_contract_symmetric_property_derives_reflected_assertion() {
    scenario(
        "ex:knows rdf:type owl:SymmetricProperty
         ex:alice ex:knows ex:bob",
        "ex:bob ex:knows ex:alice",
        Some(false),
    );
}

#[test]
fn rules_contract_transitive_property_derives_composed_assertion() {
    scenario(
        "ex:ancestorOf rdf:type owl:TransitiveProperty
         ex:alice ex:ancestorOf ex:bob
         ex:bob ex:ancestorOf ex:carol",
        "ex:alice ex:ancestorOf ex:carol",
        Some(false),
    );
}

/// Deliberate difference (design §6.3): v1 derived `x p x` for observed resources of an
/// owl:ReflexiveProperty; OWL 2 RL has no rule for it (reflexive properties are outside
/// the RL profile), and v2 follows the W3C rules.
#[test]
fn rules_reflexive_property_is_outside_owl2_rl() {
    scenario(
        "ex:p rdf:type owl:ReflexiveProperty
         ex:alice ex:knows ex:bob",
        "",
        Some(false),
    );
}

#[test]
fn rules_reuses_inferred_same_as_for_follow_on_type_propagation() {
    scenario(
        "ex:p rdf:type owl:InverseFunctionalProperty
         ex:alice ex:p ex:id
         ex:alicia ex:p ex:id
         ex:alicia rdf:type ex:Person",
        "ex:alice rdf:type ex:Person",
        Some(false),
    );
}

#[test]
fn rules_rejects_different_from_conflict_after_functional_equality_entailment() {
    scenario(
        "ex:p rdf:type owl:FunctionalProperty
         ex:alice ex:p ex:one
         ex:alice ex:p ex:two
         ex:one owl:differentFrom ex:two",
        "",
        Some(true),
    );
}

#[test]
fn rules_rejects_all_different_members_that_collapse_via_equality() {
    scenario(
        "ex:group rdf:type owl:AllDifferent
         ex:group owl:members ex:group-head
         ex:group-head rdf:first ex:alice
         ex:group-head rdf:rest ex:group-tail
         ex:group-tail rdf:first ex:alicia
         ex:group-tail rdf:rest rdf:nil
         ex:alice owl:sameAs ex:alicia",
        "",
        Some(true),
    );
}

#[test]
fn rules_rejects_all_disjoint_classes_members_on_same_instance() {
    scenario(
        "ex:group rdf:type owl:AllDisjointClasses
         ex:group owl:members ex:group-head
         ex:group-head rdf:first ex:Child
         ex:group-head rdf:rest ex:group-tail
         ex:group-tail rdf:first ex:Other
         ex:group-tail rdf:rest rdf:nil
         ex:alice rdf:type ex:Child
         ex:alice rdf:type ex:Other",
        "",
        Some(true),
    );
}

#[test]
fn rules_rejects_all_disjoint_properties_for_same_assertion_pair() {
    scenario(
        "ex:group rdf:type owl:AllDisjointProperties
         ex:group owl:members ex:group-head
         ex:group-head rdf:first ex:p1
         ex:group-head rdf:rest ex:group-tail
         ex:group-tail rdf:first ex:p2
         ex:group-tail rdf:rest rdf:nil
         ex:alice ex:p1 ex:bob
         ex:alice ex:p2 ex:bob",
        "",
        Some(true),
    );
}

#[test]
fn rules_reports_malformed_group_axiom_lists_deterministically() {
    scenario(
        "ex:group rdf:type owl:AllDifferent
         ex:group owl:members ex:group-head
         ex:group-head rdf:first ex:alice",
        "",
        Some(false),
    );
}
