use nrese_core::{CapabilityMaturity, ReasonerCapability, ReasonerFeature};

use crate::config::ReasoningMode;

#[derive(Debug, Clone, Copy)]
pub struct ReasonerProfile {
    pub name: &'static str,
    pub mode: &'static str,
    pub capabilities: &'static [ReasonerCapability],
}

const DISABLED_CAPABILITIES: [ReasonerCapability; 0] = [];

const RULES_MVP_CAPABILITIES: [ReasonerCapability; 7] = [
    ReasonerCapability {
        feature: ReasonerFeature::RdfsSubclassClosure,
        maturity: CapabilityMaturity::Mvp,
        enabled_by_default: true,
    },
    ReasonerCapability {
        feature: ReasonerFeature::RdfsSubpropertyClosure,
        maturity: CapabilityMaturity::Mvp,
        enabled_by_default: true,
    },
    ReasonerCapability {
        feature: ReasonerFeature::RdfsTypePropagation,
        maturity: CapabilityMaturity::Mvp,
        enabled_by_default: true,
    },
    ReasonerCapability {
        feature: ReasonerFeature::RdfsDomainRangeTyping,
        maturity: CapabilityMaturity::Mvp,
        enabled_by_default: true,
    },
    ReasonerCapability {
        feature: ReasonerFeature::OwlEqualityReasoning,
        maturity: CapabilityMaturity::Mvp,
        enabled_by_default: true,
    },
    ReasonerCapability {
        feature: ReasonerFeature::OwlPropertyChainAxioms,
        maturity: CapabilityMaturity::Mvp,
        enabled_by_default: true,
    },
    ReasonerCapability {
        feature: ReasonerFeature::OwlConsistencyCheck,
        maturity: CapabilityMaturity::Mvp,
        enabled_by_default: true,
    },
];

const OWL_DL_TARGET_CAPABILITIES: [ReasonerCapability; 4] = [
    ReasonerCapability {
        feature: ReasonerFeature::OwlClassSatisfiability,
        maturity: CapabilityMaturity::Target,
        enabled_by_default: false,
    },
    ReasonerCapability {
        feature: ReasonerFeature::OwlConsistencyCheck,
        maturity: CapabilityMaturity::Target,
        enabled_by_default: false,
    },
    ReasonerCapability {
        feature: ReasonerFeature::IncrementalRefresh,
        maturity: CapabilityMaturity::Target,
        enabled_by_default: false,
    },
    ReasonerCapability {
        feature: ReasonerFeature::ExplanationTrace,
        maturity: CapabilityMaturity::Experimental,
        enabled_by_default: false,
    },
];

pub fn profile_for_mode(mode: ReasoningMode) -> ReasonerProfile {
    match mode {
        ReasoningMode::Disabled => ReasonerProfile {
            name: "nrese-disabled",
            mode: "disabled",
            capabilities: &DISABLED_CAPABILITIES,
        },
        ReasoningMode::RulesMvp => ReasonerProfile {
            name: "nrese-rules-mvp",
            mode: "rules-mvp",
            capabilities: &RULES_MVP_CAPABILITIES,
        },
        ReasoningMode::OwlDlTarget => ReasonerProfile {
            name: "nrese-owl-dl-target",
            mode: "owl-dl-target",
            capabilities: &OWL_DL_TARGET_CAPABILITIES,
        },
    }
}
