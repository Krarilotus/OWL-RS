//! The mutation pipeline: the one place that decides whether and how a write is committed.
//!
//! Transport layers build a [`MutationCommand`], create a [`MutationTicket`] (to be able to
//! cancel), call [`MutationPipeline::apply`] and map [`MutationError`] to their protocol.
//! A front-end other than SPARQL and the RDF protocols writes through
//! [`MutationCommand::Delta`] with a [`DeltaPlanner`] of its own. Every public write goes
//! through the pipeline; the store's own writes outside it are crate-private (its system
//! graphs, scratch stores) or explicit bulk and replica operations. See
//! `docs/ARCHITECTURE.md` section 2.2.

mod attribution;
pub(crate) mod command;
mod error;
mod pipeline;

/// What a committer who may not read every graph is told of a rejection by the consistency
/// gates, RL and DL alike: the same whatever caused it, and whichever gate did.
pub(crate) const RESTRICTED_REJECTION: &str =
    "the commit conflicts with the repository's rules; details are available to an administrator";

/// The explanation of [`RESTRICTED_REJECTION`]: its words, nothing else.
pub(crate) fn restricted_explanation() -> nrese_reasoner::RejectExplanation {
    nrese_reasoner::RejectExplanation {
        summary: RESTRICTED_REJECTION.to_owned(),
        violated_constraint: String::new(),
        focus_resource: String::new(),
        evidence: Vec::new(),
    }
}
mod record;
mod ticket;

pub use attribution::{RejectAttribution, RejectAttributionCandidate};
pub use command::{
    DeltaPlanner, MutationCommand, MutationCommitReport, MutationKind, UpdateContext,
};
pub use error::{MutationError, MutationReject};
pub use pipeline::{MutationPipeline, RestrictedRejections};
pub use record::ReasoningRunRecord;
pub use ticket::MutationTicket;
