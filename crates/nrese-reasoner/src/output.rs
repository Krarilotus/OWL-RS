//! Reject reports: why a commit violated a consistency rule.

/// One fact that took part in a violation: a premise of the consistency rule.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RejectEvidence {
    /// The fact's role in the rule (`premise`).
    pub role: &'static str,
    pub subject: String,
    pub predicate: String,
    pub object: String,
    /// Where the fact comes from: `asserted` or `inferred`.
    pub origin: String,
}

/// A consistency violation, decoded for a reject report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RejectExplanation {
    pub summary: String,
    /// The OWL 2 RL rule (`cax-dw`, `prp-irp`, ...).
    pub violated_constraint: String,
    /// The resource the violation is about (the rule's first bound term).
    pub focus_resource: String,
    pub evidence: Vec<RejectEvidence>,
}
