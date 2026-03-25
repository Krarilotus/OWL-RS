use std::collections::BTreeSet;

use crate::class_consistency::ConsistencyViolation;
use crate::dataset_index::IndexedDataset;
use crate::explanation::{assertion_evidence, declaration_evidence};
use crate::vocabulary::{OWL_ASYMMETRIC_PROPERTY, OWL_PROPERTY_DISJOINT_WITH};

use super::builders::{
    AssertionCollision, build_assertion_collision_violation, build_irreflexive_property_violation,
    ordered_pair, render_assertion,
};
use super::plan::PropertyCharacteristicPlan;
use super::prepared::PreparedPropertyAssertions;

pub(crate) fn detect_property_characteristic_conflicts_prepared(
    index: &IndexedDataset,
    plan: &PropertyCharacteristicPlan,
    prepared: &PreparedPropertyAssertions,
) -> Vec<ConsistencyViolation> {
    let mut violations = detect_irreflexive_property_conflicts(index, plan, prepared);
    violations.extend(detect_asymmetric_property_conflicts(index, plan, prepared));
    violations.extend(detect_disjoint_property_conflicts(index, plan, prepared));
    violations
}

fn detect_irreflexive_property_conflicts(
    index: &IndexedDataset,
    plan: &PropertyCharacteristicPlan,
    prepared: &PreparedPropertyAssertions,
) -> Vec<ConsistencyViolation> {
    prepared
        .self_loops()
        .iter()
        .filter(|(_, predicate_id)| plan.irreflexive_properties().contains(predicate_id))
        .filter_map(|(subject_id, predicate_id)| {
            let subject = index.symbols().resolve(*subject_id)?;
            let property = index.symbols().resolve(*predicate_id)?;
            Some(build_irreflexive_property_violation(subject, property))
        })
        .collect()
}

fn detect_asymmetric_property_conflicts(
    index: &IndexedDataset,
    plan: &PropertyCharacteristicPlan,
    prepared: &PreparedPropertyAssertions,
) -> Vec<ConsistencyViolation> {
    let mut seen = BTreeSet::new();
    let mut violations = Vec::new();

    for (&(predicate_id, subject_id), object_ids) in prepared.outgoing_objects_by_property_subject()
    {
        if !plan.asymmetric_properties().contains(&predicate_id) {
            continue;
        }

        for &object_id in object_ids {
            if subject_id == object_id {
                continue;
            }

            let Some(reverse_objects) = prepared.objects_for(predicate_id, object_id) else {
                continue;
            };
            if !reverse_objects.contains(&subject_id) {
                continue;
            }

            let ordered = ordered_pair(subject_id, object_id);
            if !seen.insert((predicate_id, ordered.0, ordered.1)) {
                continue;
            }

            let Some(subject) = index.symbols().resolve(subject_id) else {
                continue;
            };
            let Some(object) = index.symbols().resolve(object_id) else {
                continue;
            };
            let Some(property) = index.symbols().resolve(predicate_id) else {
                continue;
            };

            let forward = render_assertion(subject, property, object);
            let reverse = render_assertion(object, property, subject);
            violations.push(build_assertion_collision_violation(
                "owl:AsymmetricProperty",
                format!(
                    "Consistency violation: property {property} is declared owl:AsymmetricProperty but both {forward} and {reverse} are present"
                ),
                AssertionCollision {
                    focus_resource: subject,
                    left_type: property,
                    right_type: object,
                    principal_assertion: &forward,
                    contextual_assertion: &reverse,
                    heuristic: "pairwise-asymmetry-assertion-collision",
                    contextual_origin_label: "effective reverse assertion",
                    evidence: vec![
                        declaration_evidence(
                            "constraint-declaration",
                            property,
                            OWL_ASYMMETRIC_PROPERTY,
                            "property declaration",
                        ),
                        assertion_evidence(
                            "conflicting-assertion",
                            subject,
                            property,
                            object,
                            "effective property assertion",
                        ),
                        assertion_evidence(
                            "conflicting-assertion",
                            object,
                            property,
                            subject,
                            "effective reverse assertion",
                        ),
                    ],
                },
            ));
        }
    }

    violations
}

fn detect_disjoint_property_conflicts(
    index: &IndexedDataset,
    plan: &PropertyCharacteristicPlan,
    prepared: &PreparedPropertyAssertions,
) -> Vec<ConsistencyViolation> {
    let mut violations = Vec::new();

    for (&(subject_id, object_id), predicate_ids) in prepared.predicates_by_subject_object() {
        for &predicate_id in predicate_ids {
            let Some(disjoint_predicates) = plan.property_disjoint_pairs().get(&predicate_id)
            else {
                continue;
            };

            for &other_predicate_id in disjoint_predicates {
                if !predicate_ids.contains(&other_predicate_id) {
                    continue;
                }

                if predicate_id > other_predicate_id {
                    continue;
                }

                let Some(subject) = index.symbols().resolve(subject_id) else {
                    continue;
                };
                let Some(object) = index.symbols().resolve(object_id) else {
                    continue;
                };
                let Some(left_property) = index.symbols().resolve(predicate_id) else {
                    continue;
                };
                let Some(right_property) = index.symbols().resolve(other_predicate_id) else {
                    continue;
                };

                let left_assertion = render_assertion(subject, left_property, object);
                let right_assertion = render_assertion(subject, right_property, object);
                violations.push(build_assertion_collision_violation(
                    "owl:propertyDisjointWith",
                    format!(
                        "Consistency violation: properties {left_property} and {right_property} are declared owl:propertyDisjointWith but both {left_assertion} and {right_assertion} are present"
                    ),
                    AssertionCollision {
                        focus_resource: subject,
                        left_type: left_property,
                        right_type: right_property,
                        principal_assertion: &left_assertion,
                        contextual_assertion: &right_assertion,
                        heuristic: "pairwise-property-disjointness-assertion-collision",
                        contextual_origin_label: "effective assertion",
                        evidence: vec![
                            assertion_evidence(
                                "constraint-axiom",
                                left_property,
                                OWL_PROPERTY_DISJOINT_WITH,
                                right_property,
                                "asserted property disjointness axiom",
                            ),
                            assertion_evidence(
                                "conflicting-assertion",
                                subject,
                                left_property,
                                object,
                                "effective property assertion",
                            ),
                            assertion_evidence(
                                "conflicting-assertion",
                                subject,
                                right_property,
                                object,
                                "effective assertion",
                            ),
                        ],
                    },
                ));
            }
        }
    }

    violations
}
