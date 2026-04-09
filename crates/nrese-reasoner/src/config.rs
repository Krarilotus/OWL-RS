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
    pub profile: ReasonerProfileConfig,
}

impl ReasonerConfig {
    pub fn for_mode(mode: ReasoningMode) -> Self {
        Self {
            profile: match mode {
                ReasoningMode::Disabled => ReasonerProfileConfig::Disabled,
                ReasoningMode::RulesMvp => {
                    ReasonerProfileConfig::RulesMvp(RulesMvpConfig::default())
                }
                ReasoningMode::OwlDlTarget => ReasonerProfileConfig::OwlDlTarget,
            },
        }
    }

    pub const fn mode(&self) -> ReasoningMode {
        match self.profile {
            ReasonerProfileConfig::Disabled => ReasoningMode::Disabled,
            ReasonerProfileConfig::RulesMvp(_) => ReasoningMode::RulesMvp,
            ReasonerProfileConfig::OwlDlTarget => ReasoningMode::OwlDlTarget,
        }
    }

    pub const fn rules_mvp(&self) -> Option<&RulesMvpConfig> {
        match &self.profile {
            ReasonerProfileConfig::RulesMvp(config) => Some(config),
            _ => None,
        }
    }

    pub fn validate(&self) -> Result<(), &'static str> {
        if let Some(config) = self.rules_mvp() {
            config.validate()?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReasonerProfileConfig {
    Disabled,
    RulesMvp(RulesMvpConfig),
    OwlDlTarget,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RulesMvpConfig {
    pub preset: RulesMvpPreset,
    pub feature_policy: RulesMvpFeaturePolicy,
}

impl RulesMvpConfig {
    pub const fn for_preset(preset: RulesMvpPreset) -> Self {
        Self {
            preset,
            feature_policy: preset.feature_policy(),
        }
    }

    pub const fn from_feature_policy(feature_policy: RulesMvpFeaturePolicy) -> Self {
        Self {
            preset: feature_policy.preset(),
            feature_policy,
        }
    }

    pub fn validate(&self) -> Result<(), &'static str> {
        if self.feature_policy.cache_key() == 0 {
            return Err("rules-mvp feature policy cache key must remain stable and non-zero");
        }
        Ok(())
    }
}

impl Default for RulesMvpConfig {
    fn default() -> Self {
        Self::for_preset(RulesMvpPreset::BoundedOwl)
    }
}

impl Default for ReasonerConfig {
    fn default() -> Self {
        Self {
            profile: ReasonerProfileConfig::Disabled,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{ReasonerConfig, ReasonerProfileConfig, ReasoningMode, RulesMvpConfig};
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

        assert_eq!(config.mode(), ReasoningMode::RulesMvp);
        assert_eq!(
            config.rules_mvp().expect("rules-mvp config").feature_policy,
            RulesMvpFeaturePolicy::industry_default()
        );
        assert_eq!(
            config.rules_mvp().expect("rules-mvp config").preset,
            RulesMvpPreset::BoundedOwl
        );
    }

    #[test]
    fn rules_mvp_config_accepts_explicit_policy() {
        let config = ReasonerConfig {
            profile: ReasonerProfileConfig::RulesMvp(RulesMvpConfig {
                preset: RulesMvpPreset::Custom,
                feature_policy: RulesMvpFeaturePolicy {
                    unsupported_constructs: UnsupportedConstructBehavior::Ignore,
                    owl_equality_reasoning: FeatureMode::Disabled,
                    ..RulesMvpFeaturePolicy::industry_default()
                },
            }),
        };

        assert!(config.validate().is_ok());
    }
}
