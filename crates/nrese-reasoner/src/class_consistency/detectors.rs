use std::collections::BTreeSet;

use crate::dataset_index::IndexedDataset;
use crate::effective_types::{EffectiveTypeOrigin, EffectiveTypeSet, effective_origin_rank};
use crate::explanation::{assertion_evidence, type_assertion_evidence};
use crate::output::RejectBlame;
use crate::vocabulary::{OWL_DISJOINT_WITH, OWL_NOTHING};

use super::{ConsistencyViolation, PreparedClassConsistency};

pub(crate) fn detect_class_consistency_conflicts(
    index: &IndexedDataset,
    effective_types: &EffectiveTypeSet,
    prepared: &PreparedClassConsistency,
) -> Vec<ConsistencyViolation> {
    let mut conflicts = detect_nothing_type_conflicts(index, effective_types, prepared);
    conflicts.extend(detect_disjoint_type_conflicts(
        index,
        effective_types,
        prepared,
    ));
    conflicts
}

fn detect_nothing_type_conflicts(
    index: &IndexedDataset,
    effective_types: &EffectiveTypeSet,
    prepared: &PreparedClassConsistency,
) -> Vec<ConsistencyViolation> {
    effective_types
        .instances()
        .filter_map(|(&instance_id, class_map)| {
            let (supporting_class_id, supporting_origin) = class_map
                .iter()
                .filter(|(class_id, _)| prepared.unsatisfiable_classes().contains(class_id))
                .min_by_key(|(class_id, origin)| (effective_origin_rank(origin), **class_id))?;

            let nothing_origin = class_map.get(&prepared.owl_nothing_id())?;
            let instance = index.symbols().resolve(instance_id)?;
            let supporting_class = index.symbols().resolve(*supporting_class_id)?;
            let supporting_origin_text = EffectiveTypeSet::describe_origin(index, supporting_origin)?;
            let nothing_origin_text = EffectiveTypeSet::describe_origin(index, nothing_origin)?;

            Some(ConsistencyViolation {
                message: if *supporting_class_id == prepared.owl_nothing_id() {
                    format!(
                        "Consistency violation: {instance} has effective type {OWL_NOTHING} ({nothing_origin_text})"
                    )
                } else {
                    format!(
                        "Consistency violation: {instance} has effective type {supporting_class} ({supporting_origin_text}), and {supporting_class} closes to {OWL_NOTHING}"
                    )
                },
                violated_constraint: "owl:Nothing".to_owned(),
                focus_resource: instance.to_owned(),
                left_type: supporting_class.to_owned(),
                right_type: OWL_NOTHING.to_owned(),
                left_origin: supporting_origin_text.clone(),
                right_origin: nothing_origin_text.clone(),
                blame: build_nothing_blame(
                    supporting_class,
                    supporting_origin,
                    &supporting_origin_text,
                    &nothing_origin_text,
                ),
                evidence: vec![
                    type_assertion_evidence(
                        "conflicting-type",
                        instance,
                        supporting_class,
                        supporting_origin_text,
                    ),
                    type_assertion_evidence(
                        "conflicting-type",
                        instance,
                        OWL_NOTHING,
                        nothing_origin_text,
                    ),
                ],
            })
        })
        .collect()
}

fn detect_disjoint_type_conflicts(
    index: &IndexedDataset,
    effective_types: &EffectiveTypeSet,
    prepared: &PreparedClassConsistency,
) -> Vec<ConsistencyViolation> {
    let mut conflicts = BTreeSet::new();

    for (&instance_id, class_map) in effective_types.instances() {
        for &class_id in class_map.keys() {
            let Some(disjoint_targets) = prepared.effective_disjoint_targets(class_id) else {
                continue;
            };

            for &other_id in disjoint_targets {
                if class_map.contains_key(&other_id) {
                    let ordered = ordered_pair(class_id, other_id);
                    conflicts.insert((instance_id, ordered.0, ordered.1));
                }
            }
        }
    }

    conflicts
        .into_iter()
        .filter_map(|(instance_id, left_id, right_id)| {
            let instance = index.symbols().resolve(instance_id)?;
            let left = index.symbols().resolve(left_id)?;
            let right = index.symbols().resolve(right_id)?;
            let class_map = effective_types.classes_for(instance_id)?;
            let left_origin_entry = class_map.get(&left_id)?;
            let right_origin_entry = class_map.get(&right_id)?;
            let left_origin = EffectiveTypeSet::describe_origin(index, left_origin_entry)?;
            let right_origin = EffectiveTypeSet::describe_origin(index, right_origin_entry)?;
            let blame = build_disjoint_blame(
                left,
                left_origin_entry,
                &left_origin,
                right,
                right_origin_entry,
                &right_origin,
            );

            Some(ConsistencyViolation {
                message: format!(
                    "Consistency violation: {instance} has disjoint effective types {left} ({left_origin}) and {right} ({right_origin}) via owl:disjointWith"
                ),
                violated_constraint: "owl:disjointWith".to_owned(),
                focus_resource: instance.to_owned(),
                left_type: left.to_owned(),
                right_type: right.to_owned(),
                left_origin,
                right_origin,
                blame,
                evidence: vec![
                    assertion_evidence(
                        "constraint-axiom",
                        left,
                        OWL_DISJOINT_WITH,
                        right,
                        "asserted disjointness axiom",
                    ),
                    type_assertion_evidence(
                        "conflicting-type",
                        instance,
                        left,
                        origin_text(left_origin_entry, index)?,
                    ),
                    type_assertion_evidence(
                        "conflicting-type",
                        instance,
                        right,
                        origin_text(right_origin_entry, index)?,
                    ),
                ],
            })
        })
        .collect()
}

fn origin_text(origin: &EffectiveTypeOrigin, index: &IndexedDataset) -> Option<String> {
    EffectiveTypeSet::describe_origin(index, origin)
}

fn build_disjoint_blame(
    left_type: &str,
    left_origin: &EffectiveTypeOrigin,
    left_origin_text: &str,
    right_type: &str,
    right_origin: &EffectiveTypeOrigin,
    right_origin_text: &str,
) -> RejectBlame {
    let left_key = blame_key(left_origin, left_type, left_origin_text);
    let right_key = blame_key(right_origin, right_type, right_origin_text);
    let (principal_type, principal_origin, contextual_type, contextual_origin) =
        if left_key <= right_key {
            (left_type, left_origin_text, right_type, right_origin_text)
        } else {
            (right_type, right_origin_text, left_type, left_origin_text)
        };

    RejectBlame {
        heuristic: "prefer-more-direct-effective-type-origin",
        principal_contributor: principal_type.to_owned(),
        principal_origin: principal_origin.to_owned(),
        contextual_contributor: contextual_type.to_owned(),
        contextual_origin: contextual_origin.to_owned(),
    }
}

fn build_nothing_blame(
    supporting_class: &str,
    supporting_origin: &EffectiveTypeOrigin,
    supporting_origin_text: &str,
    nothing_origin_text: &str,
) -> RejectBlame {
    let supporting_key = blame_key(supporting_origin, supporting_class, supporting_origin_text);
    let nothing_key = (u8::MAX, OWL_NOTHING, nothing_origin_text);
    let (principal_contributor, principal_origin, contextual_contributor, contextual_origin) =
        if supporting_key <= nothing_key {
            (
                supporting_class.to_owned(),
                supporting_origin_text.to_owned(),
                OWL_NOTHING.to_owned(),
                nothing_origin_text.to_owned(),
            )
        } else {
            (
                OWL_NOTHING.to_owned(),
                nothing_origin_text.to_owned(),
                supporting_class.to_owned(),
                supporting_origin_text.to_owned(),
            )
        };

    RejectBlame {
        heuristic: "prefer-more-direct-effective-type-origin",
        principal_contributor,
        principal_origin,
        contextual_contributor,
        contextual_origin,
    }
}

fn blame_key<'a>(
    origin: &EffectiveTypeOrigin,
    class_iri: &'a str,
    origin_text: &'a str,
) -> (u8, &'a str, &'a str) {
    (effective_origin_rank(origin), class_iri, origin_text)
}

fn ordered_pair(left: u32, right: u32) -> (u32, u32) {
    if left <= right {
        (left, right)
    } else {
        (right, left)
    }
}
