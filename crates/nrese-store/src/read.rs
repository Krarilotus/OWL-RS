//! Whose read it is: every read of the store takes a [`ReadScope`], so the store, not each
//! handler that calls it, decides which graphs a read sees (graph-level access control).
//! There is no read without one: reading everything is said at the call site
//! (`ReadScope::All`, for administrators and the server's own work).

use std::sync::Arc;

use nrese_sparql::GraphAccess;

use crate::error::{StoreError, StoreResult};

/// The graphs a read may see.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReadScope {
    /// Every graph and the inferred statements.
    All,
    /// The graphs of the set (and the inferred statements where it says so).
    Graphs(Arc<GraphAccess>),
}

impl ReadScope {
    /// The scope of a graph access set, where there is one (`None`: every graph).
    pub fn of(access: Option<Arc<GraphAccess>>) -> Self {
        match access {
            Some(access) => Self::Graphs(access),
            None => Self::All,
        }
    }

    /// The graph access set the read is restricted to, if any.
    pub fn access(&self) -> Option<&Arc<GraphAccess>> {
        match self {
            Self::All => None,
            Self::Graphs(access) => Some(access),
        }
    }

    /// Whether the read sees the inferred statements.
    pub fn sees_inferred(&self) -> bool {
        self.access().is_none_or(|access| access.inferred)
    }

    /// Whether the read sees every graph.
    pub fn reads_everything(&self) -> bool {
        matches!(self, Self::All)
    }

    /// Fails unless the read sees every graph: for operations over the whole dataset
    /// (autocompletion, classification, validation, exports).
    pub(crate) fn require_all(&self, what: &str) -> StoreResult<()> {
        match self {
            Self::All => Ok(()),
            Self::Graphs(_) => Err(StoreError::Forbidden(format!(
                "{what} reads every graph, and the requester may read only some"
            ))),
        }
    }
}
