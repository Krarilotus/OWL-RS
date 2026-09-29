use nrese_core::ReasonerCapability;

use crate::config::{ReasonerConfig, ReasoningMode};
use crate::profile::{ReasonerProfile, profile_for_config};

/// The configured reasoner: its mode and profile. The reasoning itself runs in the store
/// (`nrese_store::reasoning`): the batch executor after loads, the delta executor on
/// every commit.
#[derive(Debug, Clone)]
pub struct ReasonerService {
    config: ReasonerConfig,
    profile: ReasonerProfile,
}

impl ReasonerService {
    pub fn new(config: ReasonerConfig) -> Self {
        let profile = profile_for_config(&config);
        Self { config, profile }
    }

    pub fn config(&self) -> &ReasonerConfig {
        &self.config
    }

    pub fn mode(&self) -> ReasoningMode {
        self.config.mode()
    }

    pub fn profile_name(&self) -> &'static str {
        self.profile.name
    }

    pub fn mode_name(&self) -> &'static str {
        self.profile.mode
    }

    pub fn read_model_name(&self) -> &'static str {
        self.config.read_model_name()
    }

    pub fn semantic_tier(&self) -> &'static str {
        self.profile.semantic_tier
    }

    pub fn capabilities(&self) -> &[ReasonerCapability] {
        &self.profile.capabilities
    }

    pub fn resolved_profile(&self) -> &ReasonerProfile {
        &self.profile
    }
}
