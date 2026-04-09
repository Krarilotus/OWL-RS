mod class_consistency;
mod closure_index;
pub mod config;
mod dataset_index;
mod effective_types;
mod explanation;
mod identity;
mod materialization;
pub mod output;
pub mod profile;
mod property_chain;
mod property_closure;
mod property_consistency;
mod property_taxonomy;
mod rules;
mod rules_mvp_cache;
mod rules_mvp_policy;
mod rules_mvp_preset;
pub mod service;
mod support;
mod symbols;
mod taxonomy;
#[cfg(test)]
pub(crate) mod test_support;
mod vocabulary;

pub use config::{ReasonerConfig, ReasonerProfileConfig, ReasoningMode, RulesMvpConfig};
pub use output::{
    InferenceDelta, ReasoningCacheStats, ReasoningStats, RejectBlame, RejectEvidence,
    RejectExplanation,
};
pub use profile::{ReasonerProfile, mode_name, profile_for_config};
pub use rules_mvp_policy::{FeatureMode, RulesMvpFeaturePolicy, UnsupportedConstructBehavior};
pub use rules_mvp_preset::{RulesMvpPreset, RulesMvpPresetDescriptor};
pub use service::ReasonerService;
