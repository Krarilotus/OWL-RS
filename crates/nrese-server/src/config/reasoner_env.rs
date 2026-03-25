use anyhow::{Result, bail};
use nrese_reasoner::{
    FeatureMode, ReasonerConfig, ReasoningMode, RulesMvpConfig, RulesMvpFeaturePolicy,
    RulesMvpPreset, UnsupportedConstructBehavior,
};

use super::env_names as names;
use super::source::ConfigSource;

pub(super) fn parse_reasoner_config(source: &dyn ConfigSource) -> Result<ReasonerConfig> {
    let mode = parse_reasoning_mode(source.get(names::REASONING_MODE).as_deref());
    let preset = parse_rules_mvp_preset(source.get(names::REASONER_RULES_MVP_PRESET).as_deref())?;
    let feature_policy =
        if let Some(features_input) = source.get(names::REASONER_RULES_MVP_FEATURES) {
            parse_rules_mvp_feature_policy(Some(features_input.as_str()))?
        } else {
            policy_for_preset(preset)
        };
    let rules_mvp = RulesMvpConfig {
        preset: feature_policy.preset(),
        feature_policy,
    };
    let config = ReasonerConfig { mode, rules_mvp };
    config
        .validate()
        .map_err(|message| anyhow::anyhow!(message))?;
    Ok(config)
}

fn parse_rules_mvp_preset(input: Option<&str>) -> Result<RulesMvpPreset> {
    let Some(raw) = input.map(str::trim).filter(|value| !value.is_empty()) else {
        return Ok(RulesMvpPreset::BoundedOwl);
    };

    match raw.to_ascii_lowercase().as_str() {
        "rdfs-core" | "rdfs_core" | "rdfscore" => Ok(RulesMvpPreset::RdfsCore),
        "bounded-owl" | "bounded_owl" | "boundedowl" | "default" => Ok(RulesMvpPreset::BoundedOwl),
        "custom" => Ok(RulesMvpPreset::Custom),
        unknown => bail!(
            "unsupported value '{unknown}' in {}",
            names::REASONER_RULES_MVP_PRESET
        ),
    }
}

fn policy_for_preset(preset: RulesMvpPreset) -> RulesMvpFeaturePolicy {
    match preset {
        RulesMvpPreset::RdfsCore => RulesMvpFeaturePolicy::rdfs_core(),
        RulesMvpPreset::BoundedOwl | RulesMvpPreset::Custom => RulesMvpFeaturePolicy::bounded_owl(),
    }
}

fn parse_rules_mvp_feature_policy(input: Option<&str>) -> Result<RulesMvpFeaturePolicy> {
    let Some(raw) = input.map(str::trim).filter(|value| !value.is_empty()) else {
        return Ok(RulesMvpFeaturePolicy::industry_default());
    };

    if raw.eq_ignore_ascii_case("default") || raw.eq_ignore_ascii_case("all") {
        return Ok(RulesMvpFeaturePolicy::industry_default());
    }
    if raw.eq_ignore_ascii_case("none") {
        return Ok(RulesMvpFeaturePolicy::all_disabled());
    }

    let mut policy = RulesMvpFeaturePolicy::all_disabled();
    for token in raw
        .split(',')
        .map(str::trim)
        .filter(|token| !token.is_empty())
    {
        match token.to_ascii_lowercase().as_str() {
            "rdfs-subclass-closure" => policy.rdfs_subclass_closure = FeatureMode::Enabled,
            "rdfs-subproperty-closure" => {
                policy.rdfs_subproperty_closure = FeatureMode::Enabled;
            }
            "rdfs-type-propagation" => policy.rdfs_type_propagation = FeatureMode::Enabled,
            "rdfs-domain-range-typing" => policy.rdfs_domain_range_typing = FeatureMode::Enabled,
            "owl-property-assertion-closure" => {
                policy.owl_property_assertion_closure = FeatureMode::Enabled;
            }
            "owl-equality-reasoning" => policy.owl_equality_reasoning = FeatureMode::Enabled,
            "owl-consistency-check" => policy.owl_consistency_check = FeatureMode::Enabled,
            "unsupported-diagnostics" => {
                policy.unsupported_constructs = UnsupportedConstructBehavior::Diagnose;
            }
            unknown => {
                bail!(
                    "unsupported value '{unknown}' in {}",
                    names::REASONER_RULES_MVP_FEATURES
                );
            }
        }
    }

    Ok(policy)
}

fn parse_reasoning_mode(input: Option<&str>) -> ReasoningMode {
    match input.unwrap_or("disabled").to_ascii_lowercase().as_str() {
        "rulesmvp" | "rules_mvp" | "rules-mvp" => ReasoningMode::RulesMvp,
        "owldltarget" | "owl_dl_target" | "owl-dl-target" => ReasoningMode::OwlDlTarget,
        _ => ReasoningMode::Disabled,
    }
}

#[cfg(test)]
mod tests {
    use nrese_reasoner::{FeatureMode, UnsupportedConstructBehavior};

    use super::{parse_rules_mvp_feature_policy, parse_rules_mvp_preset, policy_for_preset};

    #[test]
    fn rules_mvp_parser_accepts_none() {
        let policy = parse_rules_mvp_feature_policy(Some("none")).expect("policy should parse");

        assert_eq!(policy.owl_consistency_check, FeatureMode::Disabled);
        assert_eq!(
            policy.unsupported_constructs,
            UnsupportedConstructBehavior::Ignore
        );
    }

    #[test]
    fn rules_mvp_parser_accepts_explicit_feature_list() {
        let policy = parse_rules_mvp_feature_policy(Some(
            "rdfs-subclass-closure,owl-equality-reasoning,unsupported-diagnostics",
        ))
        .expect("policy should parse");

        assert_eq!(policy.rdfs_subclass_closure, FeatureMode::Enabled);
        assert_eq!(policy.owl_equality_reasoning, FeatureMode::Enabled);
        assert_eq!(
            policy.unsupported_constructs,
            UnsupportedConstructBehavior::Diagnose
        );
        assert_eq!(policy.owl_consistency_check, FeatureMode::Disabled);
    }

    #[test]
    fn rules_mvp_parser_accepts_preset() {
        let preset = parse_rules_mvp_preset(Some("rdfs-core")).expect("preset should parse");
        let policy = policy_for_preset(preset);

        assert_eq!(policy.rdfs_subclass_closure, FeatureMode::Enabled);
        assert_eq!(policy.owl_equality_reasoning, FeatureMode::Disabled);
        assert_eq!(policy.preset(), nrese_reasoner::RulesMvpPreset::RdfsCore);
    }
}
