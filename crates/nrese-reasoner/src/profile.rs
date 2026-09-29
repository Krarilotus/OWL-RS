use nrese_core::{CapabilityMaturity, ReasonerCapability, ReasonerFeature};

use crate::config::{ReasonerConfig, ReasoningMode};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReasonerProfile {
    pub name: &'static str,
    pub mode: &'static str,
    pub semantic_tier: &'static str,
    pub capabilities: Vec<ReasonerCapability>,
}

pub fn profile_for_mode(mode: ReasoningMode) -> ReasonerProfile {
    profile_for_config(&ReasonerConfig::for_mode(mode))
}

pub fn profile_for_config(config: &ReasonerConfig) -> ReasonerProfile {
    let mode = config.mode();
    let capability = |feature, enabled_by_default| ReasonerCapability {
        feature,
        maturity: CapabilityMaturity::Mvp,
        enabled_by_default,
    };
    let owl = mode == ReasoningMode::Owl2Rl;
    let capabilities = match mode {
        ReasoningMode::Disabled => Vec::new(),
        ReasoningMode::Rdfs | ReasoningMode::Owl2Rl => vec![
            capability(ReasonerFeature::RdfsSubclassClosure, true),
            capability(ReasonerFeature::RdfsSubpropertyClosure, true),
            capability(ReasonerFeature::RdfsTypePropagation, true),
            capability(ReasonerFeature::RdfsDomainRangeTyping, true),
            capability(ReasonerFeature::OwlEqualityReasoning, owl),
            capability(ReasonerFeature::OwlPropertyChainAxioms, owl),
            capability(ReasonerFeature::OwlConsistencyCheck, owl),
            capability(ReasonerFeature::IncrementalRefresh, true),
        ],
    };
    ReasonerProfile {
        name: match mode {
            ReasoningMode::Disabled => "nrese-disabled",
            ReasoningMode::Rdfs | ReasoningMode::Owl2Rl => "nrese-v2",
        },
        mode: mode_name(mode),
        semantic_tier: mode.as_str(),
        capabilities,
    }
}

pub const fn mode_name(mode: ReasoningMode) -> &'static str {
    mode.as_str()
}
