//! NRESE's reasoner: reasoner v2 ([`v2`]), with the configuration and profile the server
//! exposes. The store runs it: the batch executor materialises after loads and at
//! startup, the delta executor maintains the inferred stack on every commit.

pub mod config;
pub mod output;
pub mod profile;
pub mod service;
pub mod v2;

pub use config::{ConfigError, ReasonerConfig, ReasoningMode};
pub use output::{RejectEvidence, RejectExplanation};
pub use profile::{ReasonerProfile, mode_name, profile_for_config, profile_for_mode};
pub use service::ReasonerService;
pub use v2::program::{RuleProgram, UserRules};
