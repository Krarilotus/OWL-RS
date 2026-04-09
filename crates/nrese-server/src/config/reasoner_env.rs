use anyhow::{Result, bail};
use nrese_reasoner::{
    FeatureMode, ReasonerConfig, ReasonerProfileConfig, ReasoningMode, ReasoningReadModel,
    RulesMvpConfig, RulesMvpFeaturePolicy, RulesMvpPreset, UnsupportedConstructBehavior,
};

use super::env_names as names;
use super::source::ConfigSource;

pub(super) fn parse_reasoner_config(source: &dyn ConfigSource) -> Result<ReasonerConfig> {
    let mode = parse_reasoning_mode(source.get(names::REASONING_MODE).as_deref());
    let read_model = parse_reasoning_read_model(source.get(names::REASONER_READ_MODEL).as_deref())?;
    let profile = match mode {
        ReasoningMode::Disabled => ReasonerProfileConfig::Disabled,
        ReasoningMode::RulesMvp => ReasonerProfileConfig::RulesMvp(resolve_rules_mvp_config(
            source.get(names::REASONER_RULES_MVP_PRESET).as_deref(),
            source.get(names::REASONER_RULES_MVP_FEATURES).as_deref(),
        )?),
        ReasoningMode::OwlDlTarget => ReasonerProfileConfig::OwlDlTarget,
    };
    let config = ReasonerConfig {
        profile,
        read_model,
    };
    config
        .validate()
        .map_err(|message| anyhow::anyhow!(message))?;
    Ok(config)
}

fn parse_reasoning_read_model(input: Option<&str>) -> Result<ReasoningReadModel> {
    let Some(raw) = input.map(str::trim).filter(|value| !value.is_empty()) else {
        return Ok(ReasoningReadModel::AssertedOnly);
    };

    match raw.to_ascii_lowercase().as_str() {
        "asserted-only" | "asserted_only" | "assertedonly" => Ok(ReasoningReadModel::AssertedOnly),
        unknown => bail!(
            "unsupported value '{unknown}' in {}",
            names::REASONER_READ_MODEL
        ),
    }
}

fn resolve_rules_mvp_config(
    preset_input: Option<&str>,
    features_input: Option<&str>,
) -> Result<RulesMvpConfig> {
    let requested_preset = parse_rules_mvp_preset(preset_input)?;
    match features_input
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        Some(raw) => Ok(RulesMvpConfig::from_feature_policy(
            parse_rules_mvp_feature_policy(Some(raw))?,
        )),
        None => Ok(RulesMvpConfig::for_preset(requested_preset)),
    }
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
            "owl-property-chain-axioms" => policy.owl_property_chain_axioms = FeatureMode::Enabled,
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
    use nrese_reasoner::{FeatureMode, ReasoningReadModel, UnsupportedConstructBehavior};

    use super::{
        parse_reasoning_read_model, parse_rules_mvp_feature_policy, parse_rules_mvp_preset,
        resolve_rules_mvp_config,
    };

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
            "rdfs-subclass-closure,owl-equality-reasoning,owl-property-chain-axioms,unsupported-diagnostics",
        ))
        .expect("policy should parse");

        assert_eq!(policy.rdfs_subclass_closure, FeatureMode::Enabled);
        assert_eq!(policy.owl_equality_reasoning, FeatureMode::Enabled);
        assert_eq!(policy.owl_property_chain_axioms, FeatureMode::Enabled);
        assert_eq!(
            policy.unsupported_constructs,
            UnsupportedConstructBehavior::Diagnose
        );
        assert_eq!(policy.owl_consistency_check, FeatureMode::Disabled);
    }

    #[test]
    fn rules_mvp_parser_accepts_preset() {
        let preset = parse_rules_mvp_preset(Some("rdfs-core")).expect("preset should parse");
        let config =
            resolve_rules_mvp_config(Some("rdfs-core"), None).expect("config should resolve");

        assert_eq!(preset, nrese_reasoner::RulesMvpPreset::RdfsCore);
        assert_eq!(
            config.feature_policy.rdfs_subclass_closure,
            FeatureMode::Enabled
        );
        assert_eq!(
            config.feature_policy.owl_equality_reasoning,
            FeatureMode::Disabled
        );
        assert_eq!(config.preset, nrese_reasoner::RulesMvpPreset::RdfsCore);
    }

    #[test]
    fn read_model_defaults_to_asserted_only() {
        assert_eq!(
            parse_reasoning_read_model(None).expect("read model should parse"),
            ReasoningReadModel::AssertedOnly
        );
    }
}
