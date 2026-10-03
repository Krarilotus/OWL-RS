//! NRESE's reasoner, with the configuration and profile the server exposes. The store
//! runs it: the batch executor materialises after loads and at startup, the delta
//! executor maintains the inferred stack on every commit.
//!
//! - [`ir`]: the rule IR and its text syntax; rules are data
//! - [`rulesets`]: the built-in rulesets (`rdfs`, `owl2-rl`, ...), with the W3C rule names
//! - [`lists`]: list axioms (chains, keys, intersections, ...) compiled to fixed-arity rules
//! - `naive`: the naive reference evaluator, the oracle for the fast executors (tests
//!   and the `oracle` feature only)
//! - [`eval`]: rule evaluation over any [`eval::Source`]: grounding, planning, joins
//! - [`batch`]: the batch executor: schema grounding and parallel semi-naive evaluation
//! - [`delta`]: the delta executor: maintenance under inserts and deletes (DRed)
//! - [`explain`], [`graph_sets`]: justifications and the graphs each inference needs
//! - [`classify`]: OWL 2 EL classification
//!
//! Everything works on ids through a [`ir::Vocabulary`] that interns the rules' constants.

pub mod batch;
pub mod capability;
pub mod classify;
pub mod config;
pub mod delta;
pub mod eval;
pub mod explain;
pub mod graph_sets;
pub mod ir;
pub mod lists;
pub mod n3;
#[cfg(any(test, feature = "oracle"))]
pub mod naive;
pub mod output;
mod pie;
pub mod profile;
pub mod program;
mod recursion;
pub mod representatives;
pub mod rulesets;
pub mod service;
#[cfg(test)]
mod tests;
pub mod unnamed;
pub mod vocabulary;

pub use capability::{CapabilityMaturity, ReasonerCapability, ReasonerFeature, ReasonerRunStatus};
pub use config::{ConfigError, ReasonerConfig, ReasoningMode};
pub use output::{RejectEvidence, RejectExplanation};
pub use profile::{ReasonerProfile, mode_name, profile_for_config, profile_for_mode};
pub use program::{RuleFormat, RuleProgram, UserRules};
pub use service::ReasonerService;
