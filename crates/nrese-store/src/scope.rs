//! Whose read or write it is (graph-level access control). Every read of the store takes a
//! [`ReadScope`], every write a [`Requester`] (what it may read in its `WHERE` clauses and
//! patterns, and what it may change), so the store, not each handler that calls it,
//! decides which graphs a request sees and changes. There is no read or write without one:
//! everything is said at the call site (`ReadScope::All`, `Requester::all()`, for
//! administrators and the server's own work).

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

/// The graphs a write may change.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WriteScope {
    /// Every graph.
    All,
    /// The graphs of the set.
    Graphs(Arc<GraphAccess>),
}

impl WriteScope {
    /// The scope of a graph access set, where there is one (`None`: every graph).
    pub fn of(access: Option<Arc<GraphAccess>>) -> Self {
        match access {
            Some(access) => Self::Graphs(access),
            None => Self::All,
        }
    }

    /// The graph access set the write is restricted to, if any.
    pub fn access(&self) -> Option<&Arc<GraphAccess>> {
        match self {
            Self::All => None,
            Self::Graphs(access) => Some(access),
        }
    }

    /// Fails unless the write may change `graph`.
    pub(crate) fn check(&self, graph: &nrese_rdf::GraphName) -> StoreResult<()> {
        match self.access() {
            Some(access) if !access.allows_graph(graph) => Err(StoreError::Forbidden(format!(
                "the request would change {}, which the requester may not write",
                match graph {
                    nrese_rdf::GraphName::DefaultGraph => "the default graph".to_owned(),
                    graph => graph.to_string(),
                }
            ))),
            _ => Ok(()),
        }
    }

    /// Fails unless the write may change every graph: for operations over the whole
    /// dataset (restores).
    pub(crate) fn require_all(&self, what: &str) -> StoreResult<()> {
        match self {
            Self::All => Ok(()),
            Self::Graphs(_) => Err(StoreError::Forbidden(format!(
                "{what} changes every graph, and the requester may change only some"
            ))),
        }
    }
}

/// Who a write is for: what its `WHERE` clauses and patterns may read, and what it may
/// change. Every write entry point of the store takes one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Requester {
    pub read: ReadScope,
    pub write: WriteScope,
}

impl Requester {
    /// Reads and changes every graph: administrators and the server's own work.
    pub fn all() -> Self {
        Self {
            read: ReadScope::All,
            write: WriteScope::All,
        }
    }

    pub fn new(read: ReadScope, write: WriteScope) -> Self {
        Self { read, write }
    }
}

/// One read: whose it is ([`ReadScope`]), whether inferred statements count, on which data
/// (the latest, or as a client transaction's pending changes would leave it), and what
/// stops it. The store's statement reads take one, so a new dimension of a read is a field
/// here, not another variant of every method.
#[derive(Clone)]
pub struct ReadContext<'a> {
    pub scope: ReadScope,
    /// Whether inferred statements are read (where the scope sees them).
    pub infer: bool,
    /// Pending changes the read sees, if any.
    pub pending: Option<&'a crate::StatementsRequest>,
    pub cancel: nrese_sparql::CancellationToken,
}

impl<'a> ReadContext<'a> {
    /// A read in `scope` of the latest data, inferred statements included.
    pub fn new(scope: ReadScope) -> Self {
        Self {
            scope,
            infer: true,
            pending: None,
            cancel: nrese_sparql::CancellationToken::new(),
        }
    }

    /// A read of every graph (the server's own work, tests).
    pub fn all() -> Self {
        Self::new(ReadScope::All)
    }

    /// With or without the inferred statements.
    pub fn infer(mut self, infer: bool) -> Self {
        self.infer = infer;
        self
    }

    /// On the data as `pending` would leave it.
    pub fn on(mut self, pending: Option<&'a crate::StatementsRequest>) -> Self {
        self.pending = pending;
        self
    }

    /// Stopped by `cancel`.
    pub fn cancelled_by(mut self, cancel: nrese_sparql::CancellationToken) -> Self {
        self.cancel = cancel;
        self
    }

    /// The statements read: inferred ones only where both the read and its scope want them.
    pub fn model(&self) -> nrese_engine::ReadModel {
        match self.infer && self.scope.sees_inferred() {
            true => nrese_engine::ReadModel::Materialised,
            false => nrese_engine::ReadModel::Asserted,
        }
    }
}
