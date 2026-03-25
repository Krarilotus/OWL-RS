mod detectors;
mod prepared;

use crate::output::{RejectBlame, RejectEvidence, RejectExplanation};

pub(crate) use detectors::detect_class_consistency_conflicts;
pub(crate) use prepared::PreparedClassConsistency;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConsistencyViolation {
    pub message: String,
    pub violated_constraint: String,
    pub focus_resource: String,
    pub left_type: String,
    pub right_type: String,
    pub left_origin: String,
    pub right_origin: String,
    pub blame: RejectBlame,
    pub evidence: Vec<RejectEvidence>,
}

impl ConsistencyViolation {
    pub fn reject_explanation(&self) -> RejectExplanation {
        RejectExplanation {
            summary: self.message.clone(),
            violated_constraint: self.violated_constraint.clone(),
            focus_resource: self.focus_resource.clone(),
            primary_conflicting_term: self.left_type.clone(),
            secondary_conflicting_term: self.right_type.clone(),
            blame: self.blame.clone(),
            evidence: self.evidence.clone(),
        }
    }
}

#[cfg(test)]
#[path = "../tests/consistency_tests.rs"]
mod tests;
