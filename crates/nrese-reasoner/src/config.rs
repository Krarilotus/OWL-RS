use crate::rules_mvp_policy::RulesMvpFeaturePolicy;
use crate::rules_mvp_preset::RulesMvpPreset;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReasoningMode {
    Disabled,
    RulesMvp,
    OwlDlTarget,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReasonerConfig {
    pub mode: ReasoningMode,
    pub rules_mvp: RulesMvpConfig,
}

impl ReasonerConfig {
    pub fn for_mode(mode: ReasoningMode) -> Self {
        Self {
            mode,
            ..Self::default()
        }
    }

    pub fn validate(&self) -> Result<(), &'static str> {
        self.rules_mvp.validate()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RulesMvpConfig {
    pub preset: RulesMvpPreset,
    pub feature_policy: RulesMvpFeaturePolicy,
}

impl RulesMvpConfig {
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.feature_policy.cache_key() == 0 {
            return Err("rules-mvp feature policy cache key must remain stable and non-zero");
        }
        Ok(())
    }
}

impl Default for RulesMvpConfig {
    fn default() -> Self {
        Self {
            preset: RulesMvpPreset::BoundedOwl,
            feature_policy: RulesMvpFeaturePolicy::industry_default(),
        }
    }
}

impl Default for ReasonerConfig {
    fn default() -> Self {
        Self {
            mode: ReasoningMode::Disabled,
            rules_mvp: RulesMvpConfig::default(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{ReasonerConfig, ReasoningMode, RulesMvpConfig};
    use crate::RulesMvpPreset;
    use crate::rules_mvp_policy::{
        FeatureMode, RulesMvpFeaturePolicy, UnsupportedConstructBehavior,
    };

    #[test]
    fn default_reasoner_config_is_valid() {
        assert!(ReasonerConfig::default().validate().is_ok());
    }

    #[test]
    fn reasoner_config_for_mode_preserves_defaults() {
        let config = ReasonerConfig::for_mode(ReasoningMode::RulesMvp);

        assert_eq!(config.mode, ReasoningMode::RulesMvp);
        assert_eq!(
            config.rules_mvp.feature_policy,
            RulesMvpFeaturePolicy::industry_default()
        );
        assert_eq!(config.rules_mvp.preset, RulesMvpPreset::BoundedOwl);
    }

    #[test]
    fn rules_mvp_config_accepts_explicit_policy() {
        let config = RulesMvpConfig {
            preset: RulesMvpPreset::Custom,
            feature_policy: RulesMvpFeaturePolicy {
                unsupported_constructs: UnsupportedConstructBehavior::Ignore,
                owl_equality_reasoning: FeatureMode::Disabled,
                ..RulesMvpFeaturePolicy::industry_default()
            },
        };

        assert!(config.validate().is_ok());
    }
}
