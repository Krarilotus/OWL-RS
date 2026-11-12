use nrese_reasoner::RejectExplanation;
use thiserror::Error;

use super::attribution::RejectAttribution;
use super::command::MutationKind;
use crate::error::StoreError;

/// Why a mutation was not committed. Transport layers map these to protocol responses;
/// they never need to inspect error strings.
#[derive(Debug, Error)]
pub enum MutationError {
    /// The request itself is invalid or could not be applied (parse errors, bad graph IRIs,
    /// storage failures). `kind` tells the transport which entry point failed.
    #[error("{source}")]
    Store {
        kind: MutationKind,
        #[source]
        source: StoreError,
    },
    /// A validation gate rejected the resulting dataset state.
    #[error("{}", .0.detail)]
    Rejected(Box<MutationReject>),
    /// The caller cancelled (for example on timeout) before the commit started.
    #[error("mutation was cancelled before commit")]
    Cancelled,
    /// A gate failed internally (not a validation outcome).
    #[error("validation gate failure: {0}")]
    Gate(String),
    #[error("the write slot is poisoned by an earlier panic")]
    Poisoned,
    /// The repository was reconfigured (other rules) while the write waited: it may be
    /// sent again.
    #[error("the repository was reconfigured while the write waited; send it again")]
    Retired,
}

/// Details of a gate rejection, shared by HTTP problem responses and operator diagnostics.
/// A committer that may read only some graphs is told only what its own view shows
/// ([`super::evidence`], the one place that makes a rejection).
#[derive(Debug, Clone)]
pub struct MutationReject {
    /// What kind of reason the committer is shown: a restricted committer is shown its own
    /// view's first reason, whichever gate rejected ([`super::evidence`]).
    pub kind: RejectKind,
    pub detail: String,
    pub explanation: Option<RejectExplanation>,
    pub attribution: Option<RejectAttribution>,
    pub(super) _projected: super::evidence::Projected,
}

/// The kind of reason a rejected committer is shown, for a transport to label it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RejectKind {
    /// The commit makes the data inconsistent (an OWL 2 RL consistency rule, or OWL 2 DL).
    Inconsistent,
    /// The commit introduces SHACL results the gate rejects for.
    ShapesViolated,
    /// The committer's graphs show no reason by themselves: the same for every gate.
    NotShown,
}
