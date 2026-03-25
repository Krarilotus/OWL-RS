#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FeatureMode {
    Disabled,
    Enabled,
}

impl FeatureMode {
    pub const fn is_enabled(self) -> bool {
        matches!(self, Self::Enabled)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum UnsupportedConstructBehavior {
    Ignore,
    Diagnose,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct RulesMvpFeaturePolicy {
    pub rdfs_subclass_closure: FeatureMode,
    pub rdfs_subproperty_closure: FeatureMode,
    pub rdfs_type_propagation: FeatureMode,
    pub rdfs_domain_range_typing: FeatureMode,
    pub owl_property_assertion_closure: FeatureMode,
    pub owl_equality_reasoning: FeatureMode,
    pub owl_consistency_check: FeatureMode,
    pub unsupported_constructs: UnsupportedConstructBehavior,
}

impl RulesMvpFeaturePolicy {
    pub const fn industry_default() -> Self {
        Self {
            rdfs_subclass_closure: FeatureMode::Enabled,
            rdfs_subproperty_closure: FeatureMode::Enabled,
            rdfs_type_propagation: FeatureMode::Enabled,
            rdfs_domain_range_typing: FeatureMode::Enabled,
            owl_property_assertion_closure: FeatureMode::Enabled,
            owl_equality_reasoning: FeatureMode::Enabled,
            owl_consistency_check: FeatureMode::Enabled,
            unsupported_constructs: UnsupportedConstructBehavior::Diagnose,
        }
    }

    pub const fn all_disabled() -> Self {
        Self {
            rdfs_subclass_closure: FeatureMode::Disabled,
            rdfs_subproperty_closure: FeatureMode::Disabled,
            rdfs_type_propagation: FeatureMode::Disabled,
            rdfs_domain_range_typing: FeatureMode::Disabled,
            owl_property_assertion_closure: FeatureMode::Disabled,
            owl_equality_reasoning: FeatureMode::Disabled,
            owl_consistency_check: FeatureMode::Disabled,
            unsupported_constructs: UnsupportedConstructBehavior::Ignore,
        }
    }

    pub const fn cache_key(self) -> u64 {
        let mut key = 0x4e52_4553_455f_5255u64;
        key ^= flag(self.rdfs_subclass_closure, 0);
        key ^= flag(self.rdfs_subproperty_closure, 1);
        key ^= flag(self.rdfs_type_propagation, 2);
        key ^= flag(self.rdfs_domain_range_typing, 3);
        key ^= flag(self.owl_property_assertion_closure, 4);
        key ^= flag(self.owl_equality_reasoning, 5);
        key ^= flag(self.owl_consistency_check, 6);
        key ^= match self.unsupported_constructs {
            UnsupportedConstructBehavior::Ignore => 0,
            UnsupportedConstructBehavior::Diagnose => 1 << 7,
        };
        key
    }

    pub const fn rdfs_subclass_closure_enabled(self) -> bool {
        self.rdfs_subclass_closure.is_enabled()
    }

    pub const fn rdfs_subproperty_closure_enabled(self) -> bool {
        self.rdfs_subproperty_closure.is_enabled()
    }

    pub const fn rdfs_type_propagation_enabled(self) -> bool {
        self.rdfs_type_propagation.is_enabled()
    }

    pub const fn rdfs_domain_range_typing_enabled(self) -> bool {
        self.rdfs_domain_range_typing.is_enabled()
    }

    pub const fn owl_property_assertion_closure_enabled(self) -> bool {
        self.owl_property_assertion_closure.is_enabled()
    }

    pub const fn owl_equality_reasoning_enabled(self) -> bool {
        self.owl_equality_reasoning.is_enabled()
    }

    pub const fn owl_consistency_check_enabled(self) -> bool {
        self.owl_consistency_check.is_enabled()
    }

    pub const fn unsupported_construct_diagnostics_enabled(self) -> bool {
        matches!(
            self.unsupported_constructs,
            UnsupportedConstructBehavior::Diagnose
        )
    }

    pub const fn needs_class_taxonomy(self) -> bool {
        self.rdfs_subclass_closure_enabled()
            || self.rdfs_type_propagation_enabled()
            || self.owl_consistency_check_enabled()
    }

    pub const fn needs_property_taxonomy(self) -> bool {
        self.rdfs_subproperty_closure_enabled()
            || self.owl_property_assertion_closure_enabled()
            || self.rdfs_domain_range_typing_enabled()
            || self.owl_equality_reasoning_enabled()
            || self.owl_consistency_check_enabled()
    }

    pub const fn needs_effective_types(self) -> bool {
        self.rdfs_type_propagation_enabled()
            || self.rdfs_domain_range_typing_enabled()
            || self.owl_consistency_check_enabled()
    }

    pub const fn needs_property_closure(self) -> bool {
        self.rdfs_subproperty_closure_enabled()
            || self.rdfs_domain_range_typing_enabled()
            || self.owl_equality_reasoning_enabled()
            || self.owl_consistency_check_enabled()
    }
}

const fn flag(mode: FeatureMode, bit: u64) -> u64 {
    match mode {
        FeatureMode::Disabled => 0,
        FeatureMode::Enabled => 1 << bit,
    }
}

#[cfg(test)]
mod tests {
    use super::{FeatureMode, RulesMvpFeaturePolicy, UnsupportedConstructBehavior};

    #[test]
    fn policy_cache_key_changes_when_behavior_changes() {
        let baseline = RulesMvpFeaturePolicy::industry_default();
        let changed = RulesMvpFeaturePolicy {
            owl_equality_reasoning: FeatureMode::Disabled,
            ..baseline
        };

        assert_ne!(baseline.cache_key(), changed.cache_key());
    }

    #[test]
    fn all_disabled_policy_turns_off_diagnostics() {
        let policy = RulesMvpFeaturePolicy::all_disabled();

        assert!(!policy.owl_consistency_check_enabled());
        assert!(!policy.unsupported_construct_diagnostics_enabled());
    }

    #[test]
    fn policy_helpers_reflect_diagnose_mode() {
        let policy = RulesMvpFeaturePolicy {
            unsupported_constructs: UnsupportedConstructBehavior::Diagnose,
            ..RulesMvpFeaturePolicy::all_disabled()
        };

        assert!(policy.unsupported_construct_diagnostics_enabled());
    }
}
