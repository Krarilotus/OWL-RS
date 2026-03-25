#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ReasonerFeature {
    RdfsSubclassClosure,
    RdfsSubpropertyClosure,
    RdfsTypePropagation,
    RdfsDomainRangeTyping,
    OwlEqualityReasoning,
    OwlClassSatisfiability,
    OwlConsistencyCheck,
    IncrementalRefresh,
    ExplanationTrace,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CapabilityMaturity {
    Experimental,
    Mvp,
    Target,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ReasonerCapability {
    pub feature: ReasonerFeature,
    pub maturity: CapabilityMaturity,
    pub enabled_by_default: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ExecutionKind {
    FullMaterialization,
    IncrementalRefresh,
    ValidationOnly,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ReasonerExecutionPlan {
    pub revision: u64,
    pub kind: ExecutionKind,
    pub changed_asserted_triples: Option<u64>,
}

impl ReasonerExecutionPlan {
    pub fn full_materialization(revision: u64) -> Self {
        Self {
            revision,
            kind: ExecutionKind::FullMaterialization,
            changed_asserted_triples: None,
        }
    }

    pub fn validation_only(revision: u64) -> Self {
        Self {
            revision,
            kind: ExecutionKind::ValidationOnly,
            changed_asserted_triples: None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ReasonerRunStatus {
    Completed,
    Skipped,
    Rejected,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Hash)]
pub struct ReasonerRunMetrics {
    pub asserted_triples_seen: u64,
    pub inferred_triples_produced: u64,
    pub consistency_violations: u64,
    pub elapsed_millis: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReasonerRunReport {
    pub profile: &'static str,
    pub mode: &'static str,
    pub revision: u64,
    pub status: ReasonerRunStatus,
    pub metrics: ReasonerRunMetrics,
    pub notes: Vec<&'static str>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReasoningOutput<T> {
    pub inferred: T,
    pub report: ReasonerRunReport,
}
