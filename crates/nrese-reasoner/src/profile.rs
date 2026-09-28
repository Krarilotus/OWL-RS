use nrese_core::{CapabilityMaturity, ReasonerCapability, ReasonerFeature};

use crate::config::{ReasonerConfig, ReasonerProfileConfig, ReasoningMode, RulesMvpConfig};
use crate::v2::rulesets::Ruleset;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReasonerProfile {
    pub name: &'static str,
    pub mode: &'static str,
    pub semantic_tier: &'static str,
    pub capabilities: Vec<ReasonerCapability>,
}

const DISABLED_CAPABILITIES: [ReasonerCapability; 0] = [];

pub fn profile_for_mode(mode: ReasoningMode) -> ReasonerProfile {
    profile_for_config(&ReasonerConfig::for_mode(mode))
}

pub fn profile_for_config(config: &ReasonerConfig) -> ReasonerProfile {
    match &config.profile {
        ReasonerProfileConfig::Disabled => ReasonerProfile {
            name: "nrese-disabled",
            mode: mode_name(ReasoningMode::Disabled),
            semantic_tier: "disabled",
            capabilities: DISABLED_CAPABILITIES.to_vec(),
        },
        ReasonerProfileConfig::RulesMvp(rules_mvp) => ReasonerProfile {
            name: "nrese-rules-mvp",
            mode: mode_name(ReasoningMode::RulesMvp),
            semantic_tier: rules_mvp.preset.semantic_tier(),
            capabilities: rules_mvp_capabilities(rules_mvp),
        },
        ReasonerProfileConfig::Materialise(ruleset) => ReasonerProfile {
            name: "nrese-v2",
            mode: mode_name(config.mode()),
            semantic_tier: ruleset.name(),
            capabilities: v2_capabilities(*ruleset),
        },
    }
}

fn v2_capabilities(ruleset: Ruleset) -> Vec<ReasonerCapability> {
    let owl = ruleset == Ruleset::Owl2Rl;
    let capability = |feature, enabled_by_default| ReasonerCapability {
        feature,
        maturity: CapabilityMaturity::Mvp,
        enabled_by_default,
    };
    vec![
        capability(ReasonerFeature::RdfsSubclassClosure, true),
        capability(ReasonerFeature::RdfsSubpropertyClosure, true),
        capability(ReasonerFeature::RdfsTypePropagation, true),
        capability(ReasonerFeature::RdfsDomainRangeTyping, true),
        capability(ReasonerFeature::OwlEqualityReasoning, owl),
        capability(ReasonerFeature::OwlPropertyChainAxioms, owl),
        capability(ReasonerFeature::OwlConsistencyCheck, owl),
    ]
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
        ReasoningMode::Rdfs => "rdfs",
        ReasoningMode::Owl2Rl => "owl2-rl",
    }
}

#[cfg(test)]
mod tests {
    use super::{mode_name, profile_for_config, profile_for_mode};
    use crate::{ReasonerConfig, ReasoningMode};

    #[test]
    fn profile_for_mode_matches_config_resolution() {
        let from_mode = profile_for_mode(ReasoningMode::RulesMvp);
        let from_config = profile_for_config(&ReasonerConfig::for_mode(ReasoningMode::RulesMvp));

        assert_eq!(from_mode, from_config);
        assert_eq!(from_mode.mode, mode_name(ReasoningMode::RulesMvp));
    }
}
