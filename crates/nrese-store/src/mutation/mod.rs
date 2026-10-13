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
mod record;
mod ticket;

pub use attribution::{RejectAttribution, RejectAttributionCandidate};
pub use command::{
    DeltaPlanner, MutationCommand, MutationCommitReport, MutationKind, UpdateContext,
};
pub use error::{MutationError, MutationReject};
pub use pipeline::MutationPipeline;
pub use record::ReasoningRunRecord;
pub use ticket::MutationTicket;
