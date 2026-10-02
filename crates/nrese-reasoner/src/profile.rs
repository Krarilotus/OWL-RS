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
    // What the mode computes, and nothing it can't: a feature another mode offers isn't
    // listed as "disabled" here. The precise contract is `docs/spec/reasoning-semantics.md`.
    let rdfs = [
        ReasonerFeature::RdfsSubclassClosure,
        ReasonerFeature::RdfsSubpropertyClosure,
        ReasonerFeature::RdfsTypePropagation,
        ReasonerFeature::RdfsDomainRangeTyping,
        ReasonerFeature::IncrementalRefresh,
    ];
    let owl = [
        ReasonerFeature::OwlEqualityReasoning,
        ReasonerFeature::OwlPropertyChainAxioms,
        ReasonerFeature::OwlConsistencyCheck,
        ReasonerFeature::ExplanationTrace,
    ];
    let equality = [ReasonerFeature::OwlEqualityReasoning];
    let consistency = [
        ReasonerFeature::OwlConsistencyCheck,
        ReasonerFeature::ExplanationTrace,
    ];
    let features: &[ReasonerFeature] = match mode {
        ReasoningMode::Disabled => &[],
        ReasoningMode::Rdfs | ReasoningMode::RdfsFull => &rdfs,
        ReasoningMode::RdfsPlus | ReasoningMode::OwlHorst => {
            &[rdfs.as_slice(), equality.as_slice()].concat()
        }
        ReasoningMode::Owl2Ql => &[rdfs.as_slice(), consistency.as_slice()].concat(),
        ReasoningMode::Owl2Rl => &[rdfs.as_slice(), owl.as_slice()].concat(),
        // The user's rules: what they derive is theirs to say; it is maintained on commits.
        ReasoningMode::Custom => &[ReasonerFeature::IncrementalRefresh],
    };
    let capabilities = features
        .iter()
        .map(|&feature| capability(feature, true))
        .collect();
    ReasonerProfile {
        name: match mode {
            ReasoningMode::Disabled => "nrese-disabled",
            _ => "nrese-v2",
        },
        mode: mode_name(mode),
        semantic_tier: mode.as_str(),
        capabilities,
    }
}

pub const fn mode_name(mode: ReasoningMode) -> &'static str {
    mode.as_str()
}

#[cfg(test)]
mod tests {
    use nrese_core::ReasonerFeature;

    use super::profile_for_mode;
    use crate::config::ReasoningMode;

    #[test]
    fn profiles_list_only_what_their_mode_computes() {
        let features = |mode| -> Vec<ReasonerFeature> {
            profile_for_mode(mode)
                .capabilities
                .iter()
                .map(|capability| capability.feature)
                .collect()
        };
        assert!(features(ReasoningMode::Disabled).is_empty());
        let rdfs = features(ReasoningMode::Rdfs);
        assert!(rdfs.contains(&ReasonerFeature::RdfsSubclassClosure));
        assert!(!rdfs.contains(&ReasonerFeature::OwlEqualityReasoning));
        assert!(!rdfs.contains(&ReasonerFeature::OwlConsistencyCheck));
        let owl = features(ReasoningMode::Owl2Rl);
        assert!(rdfs.iter().all(|feature| owl.contains(feature)));
        assert!(owl.contains(&ReasonerFeature::OwlConsistencyCheck));
        assert!(owl.contains(&ReasonerFeature::ExplanationTrace));
        assert!(!owl.contains(&ReasonerFeature::OwlClassSatisfiability));
        for mode in ReasoningMode::REASONING {
            assert!(
                profile_for_mode(mode)
                    .capabilities
                    .iter()
                    .all(|c| c.enabled_by_default)
            );
        }
    }
}
