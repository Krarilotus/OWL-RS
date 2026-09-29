#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ReasonerFeature {
    RdfsSubclassClosure,
    RdfsSubpropertyClosure,
    RdfsTypePropagation,
    RdfsDomainRangeTyping,
    OwlEqualityReasoning,
    OwlPropertyChainAxioms,
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
pub enum ReasonerRunStatus {
    Completed,
    Skipped,
    Rejected,
}
