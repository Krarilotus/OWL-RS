use nrese_core::{CapabilityMaturity, ReasonerCapability, ReasonerFeature};

use crate::config::{ReasonerConfig, ReasonerProfileConfig, ReasoningMode, RulesMvpConfig};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReasonerProfile {
    pub name: &'static str,
    pub mode: &'static str,
    pub semantic_tier: &'static str,
    pub capabilities: Vec<ReasonerCapability>,
}

const DISABLED_CAPABILITIES: [ReasonerCapability; 0] = [];

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

pub fn profile_for_config(config: &ReasonerConfig) -> ReasonerProfile {
    match &config.profile {
        ReasonerProfileConfig::Disabled => ReasonerProfile {
            name: "nrese-disabled",
            mode: "disabled",
            semantic_tier: "disabled",
            capabilities: DISABLED_CAPABILITIES.to_vec(),
        },
        ReasonerProfileConfig::RulesMvp(rules_mvp) => ReasonerProfile {
            name: "nrese-rules-mvp",
            mode: "rules-mvp",
            semantic_tier: rules_mvp.preset.semantic_tier(),
            capabilities: rules_mvp_capabilities(rules_mvp),
        },
        ReasonerProfileConfig::OwlDlTarget => ReasonerProfile {
            name: "nrese-owl-dl-target",
            mode: "owl-dl-target",
            semantic_tier: "owl-dl-target",
            capabilities: OWL_DL_TARGET_CAPABILITIES.to_vec(),
        },
    }
}

fn rules_mvp_capabilities(config: &RulesMvpConfig) -> Vec<ReasonerCapability> {
    let policy = config.feature_policy;

    vec![
        ReasonerCapability {
            feature: ReasonerFeature::RdfsSubclassClosure,
            maturity: CapabilityMaturity::Mvp,
            enabled_by_default: policy.rdfs_subclass_closure_enabled(),
        },
        ReasonerCapability {
            feature: ReasonerFeature::RdfsSubpropertyClosure,
            maturity: CapabilityMaturity::Mvp,
            enabled_by_default: policy.rdfs_subproperty_closure_enabled(),
        },
        ReasonerCapability {
            feature: ReasonerFeature::RdfsTypePropagation,
            maturity: CapabilityMaturity::Mvp,
            enabled_by_default: policy.rdfs_type_propagation_enabled(),
        },
        ReasonerCapability {
            feature: ReasonerFeature::RdfsDomainRangeTyping,
            maturity: CapabilityMaturity::Mvp,
            enabled_by_default: policy.rdfs_domain_range_typing_enabled(),
        },
        ReasonerCapability {
            feature: ReasonerFeature::OwlEqualityReasoning,
            maturity: CapabilityMaturity::Mvp,
            enabled_by_default: policy.owl_equality_reasoning_enabled(),
        },
        ReasonerCapability {
            feature: ReasonerFeature::OwlPropertyChainAxioms,
            maturity: CapabilityMaturity::Mvp,
            enabled_by_default: policy.owl_property_chain_axioms_enabled(),
        },
        ReasonerCapability {
            feature: ReasonerFeature::OwlConsistencyCheck,
            maturity: CapabilityMaturity::Mvp,
            enabled_by_default: policy.owl_consistency_check_enabled(),
        },
    ]
}

pub const fn mode_name(mode: ReasoningMode) -> &'static str {
    match mode {
        ReasoningMode::Disabled => "disabled",
        ReasoningMode::RulesMvp => "rules-mvp",
        ReasoningMode::OwlDlTarget => "owl-dl-target",
    }
}
