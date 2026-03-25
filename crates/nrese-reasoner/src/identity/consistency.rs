use crate::class_consistency::ConsistencyViolation;
use crate::dataset_index::IndexedDataset;
use crate::explanation::assertion_evidence;
use crate::output::RejectBlame;
use crate::vocabulary::{OWL_DIFFERENT_FROM, OWL_SAME_AS};

use super::EqualityIndex;

pub(crate) fn detect_different_from_conflicts(
    index: &IndexedDataset,
    equality: &EqualityIndex,
) -> Vec<ConsistencyViolation> {
    index.different_from_pairs()
        .iter()
        .filter_map(|&(left_id, right_id)| {
            if !equality.are_equivalent(left_id, right_id) {
                return None;
            }

            let left = index.symbols().resolve(left_id)?;
            let right = index.symbols().resolve(right_id)?;
            let equality_left = index.symbols().resolve(equality.canonical_of(left_id))?;
            let equality_right = index.symbols().resolve(equality.canonical_of(right_id))?;
            let different_from_axiom = format!("<{left}> <{OWL_DIFFERENT_FROM}> <{right}>");

            Some(ConsistencyViolation {
                message: format!(
                    "Consistency violation: resources {left} and {right} are declared owl:differentFrom but also belong to the same effective equality set"
                ),
                violated_constraint: "owl:differentFrom".to_owned(),
                focus_resource: left.to_owned(),
                left_type: left.to_owned(),
                right_type: right.to_owned(),
                left_origin: "asserted owl:differentFrom axiom".to_owned(),
                right_origin: format!(
                    "effective equality cluster canonicalized as {equality_left} / {equality_right}"
                ),
                blame: RejectBlame {
                    heuristic: "prefer-direct-different-from-axiom",
                    principal_contributor: different_from_axiom.clone(),
                    principal_origin: "asserted owl:differentFrom axiom".to_owned(),
                    contextual_contributor: format!("<{equality_left}> <{OWL_SAME_AS}> <{equality_right}>"),
                    contextual_origin: "effective equality closure".to_owned(),
                },
                evidence: vec![
                    assertion_evidence(
                        "constraint-axiom",
                        left,
                        OWL_DIFFERENT_FROM,
                        right,
                        "asserted owl:differentFrom axiom",
                    ),
                    assertion_evidence(
                        "conflicting-equality",
                        equality_left,
                        OWL_SAME_AS,
                        equality_right,
                        "effective equality closure",
                    ),
                ],
            })
        })
        .collect()
}

#[cfg(test)]
#[path = "../tests/identity_consistency_tests.rs"]
mod tests;
