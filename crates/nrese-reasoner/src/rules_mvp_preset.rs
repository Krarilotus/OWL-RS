use crate::rules_mvp_policy::{FeatureMode, RulesMvpFeaturePolicy, UnsupportedConstructBehavior};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RulesMvpPresetDescriptor {
    pub name: &'static str,
    pub semantic_tier: &'static str,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RulesMvpPreset {
    RdfsCore,
    BoundedOwl,
    Custom,
}

const AVAILABLE_PRESET_NAMES: [&str; 2] = ["rdfs-core", "bounded-owl"];
const AVAILABLE_PRESET_DESCRIPTORS: [RulesMvpPresetDescriptor; 2] = [
    RulesMvpPresetDescriptor {
        name: "rdfs-core",
        semantic_tier: "rdfs-core",
    },
    RulesMvpPresetDescriptor {
        name: "bounded-owl",
        semantic_tier: "bounded-owl-rules",
    },
];

impl RulesMvpPreset {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::RdfsCore => "rdfs-core",
            Self::BoundedOwl => "bounded-owl",
            Self::Custom => "custom",
        }
    }

    pub const fn semantic_tier(self) -> &'static str {
        match self {
            Self::RdfsCore => "rdfs-core",
            Self::BoundedOwl => "bounded-owl-rules",
            Self::Custom => "custom",
        }
    }

    pub const fn descriptor(self) -> RulesMvpPresetDescriptor {
        RulesMvpPresetDescriptor {
            name: self.as_str(),
            semantic_tier: self.semantic_tier(),
        }
    }

    pub const fn available() -> &'static [&'static str] {
        &AVAILABLE_PRESET_NAMES
    }

    pub const fn available_descriptors() -> &'static [RulesMvpPresetDescriptor] {
        &AVAILABLE_PRESET_DESCRIPTORS
    }

    pub const fn feature_policy(self) -> RulesMvpFeaturePolicy {
        match self {
            Self::RdfsCore => RulesMvpFeaturePolicy::rdfs_core(),
            Self::BoundedOwl | Self::Custom => RulesMvpFeaturePolicy::bounded_owl(),
        }
    }
}

impl RulesMvpFeaturePolicy {
    pub const fn rdfs_core() -> Self {
        Self {
            rdfs_subclass_closure: FeatureMode::Enabled,
            rdfs_subproperty_closure: FeatureMode::Enabled,
            rdfs_type_propagation: FeatureMode::Enabled,
            rdfs_domain_range_typing: FeatureMode::Enabled,
            owl_property_assertion_closure: FeatureMode::Disabled,
            owl_equality_reasoning: FeatureMode::Disabled,
            owl_property_chain_axioms: FeatureMode::Disabled,
            owl_consistency_check: FeatureMode::Disabled,
            unsupported_constructs: UnsupportedConstructBehavior::Diagnose,
        }
    }

    pub const fn bounded_owl() -> Self {
        Self::industry_default()
    }

    pub const fn preset(self) -> RulesMvpPreset {
        if self.same_as(Self::rdfs_core()) {
            RulesMvpPreset::RdfsCore
        } else if self.same_as(Self::bounded_owl()) {
            RulesMvpPreset::BoundedOwl
        } else {
            RulesMvpPreset::Custom
        }
    }

    const fn same_as(self, other: Self) -> bool {
        self.rdfs_subclass_closure as u8 == other.rdfs_subclass_closure as u8
            && self.rdfs_subproperty_closure as u8 == other.rdfs_subproperty_closure as u8
            && self.rdfs_type_propagation as u8 == other.rdfs_type_propagation as u8
            && self.rdfs_domain_range_typing as u8 == other.rdfs_domain_range_typing as u8
            && self.owl_property_assertion_closure as u8
                == other.owl_property_assertion_closure as u8
            && self.owl_equality_reasoning as u8 == other.owl_equality_reasoning as u8
            && self.owl_property_chain_axioms as u8 == other.owl_property_chain_axioms as u8
            && self.owl_consistency_check as u8 == other.owl_consistency_check as u8
            && self.unsupported_constructs as u8 == other.unsupported_constructs as u8
    }
}

#[cfg(test)]
mod tests {
    use super::RulesMvpPreset;
    use crate::{RulesMvpFeaturePolicy, rules_mvp_preset::RulesMvpPresetDescriptor};

    #[test]
    fn recognizes_known_presets() {
        assert_eq!(
            RulesMvpFeaturePolicy::rdfs_core().preset(),
            RulesMvpPreset::RdfsCore
        );
        assert_eq!(
            RulesMvpFeaturePolicy::bounded_owl().preset(),
            RulesMvpPreset::BoundedOwl
        );
    }

    #[test]
    fn descriptors_expose_semantic_tiers() {
        assert_eq!(
            RulesMvpPreset::RdfsCore.descriptor(),
            RulesMvpPresetDescriptor {
                name: "rdfs-core",
                semantic_tier: "rdfs-core",
            }
        );
        assert_eq!(
            RulesMvpPreset::BoundedOwl.semantic_tier(),
            "bounded-owl-rules"
        );
    }
}
