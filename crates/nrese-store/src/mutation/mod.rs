//! The mutation pipeline: the one place that decides whether and how a write is committed.
//!
//! Transport layers build a [`MutationCommand`], create a [`MutationTicket`] (to be able to
//! cancel), call [`MutationPipeline::apply`] and map [`MutationError`] to their protocol.
//! See `docs/ARCHITECTURE.md` section 2.2.

mod attribution;
mod command;
mod error;
mod pipeline;
mod record;
mod ticket;

pub use attribution::{RejectAttribution, RejectAttributionCandidate};
pub use command::{MutationCommand, MutationCommitReport, MutationKind};
pub use error::{MutationError, MutationReject};
pub use pipeline::MutationPipeline;
pub use record::ReasoningRunRecord;
pub use ticket::MutationTicket;
