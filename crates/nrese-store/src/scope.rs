//! Whose read or write it is (graph-level access control). Every read of the store takes a
//! [`ReadScope`], every write a [`Requester`] (what it may read in its `WHERE` clauses and
//! patterns, and what it may change), so the store, not each handler that calls it,
//! decides which graphs a request sees and changes. There is no read or write without one:
//! everything is said at the call site (`ReadScope::All`, `Requester::all()`, for
//! administrators and the server's own work).

use std::sync::Arc;

use nrese_sparql::GraphAccess;

use crate::error::{Refusal, StoreResult};

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
            Self::Graphs(_) => Err(Refusal::ReadAll(what.to_owned()).into()),
        }
    }

    /// `result` as this read's requester may be told it, the one boundary every read and
    /// update entry point returns through, naming itself (`_entry`: each has a row in the
    /// table that checks this boundary, [`EntryPoint`]). A reader of every graph is told
    /// everything; a reader of some, a memory refusal's kind and limit but not the bytes held
    /// and asked for, which a scan or a view of graphs it may not read can size (round 13,
    /// R13-diag).
    pub(crate) fn told<T>(&self, _entry: EntryPoint, result: StoreResult<T>) -> StoreResult<T> {
        match result {
            Err(error) if !self.reads_everything() => Err(error.without_amounts()),
            result => result,
        }
    }
}

/// Declares [`EntryPoint`] and [`EntryPoint::ALL`] from one list, so that the list holds
/// every entry point.
macro_rules! entry_points {
    ($($(#[doc = $doc:literal])+ $name:ident,)+) => {
        /// The read and update entry points that return through [`ReadScope::told`], each
        /// naming itself there. The test of that boundary (`scope_tests`,
        /// `memory_refusals_tell_a_restricted_requester_nothing_of_hidden_sizes`) builds its
        /// rows by a match over these, so one without a row doesn't compile.
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        pub enum EntryPoint {
            $($(#[doc = $doc])+ $name,)+
        }

        impl EntryPoint {
            /// Every entry point.
            pub const ALL: &'static [Self] = &[$(Self::$name,)+];
        }
    };
}

entry_points! {
    /// A query run to completion ([`crate::StoreService::run_query`] and the others that
    /// run one).
    Run,
    /// A query run on a transaction's pending operations
    /// ([`crate::StoreService::run_query_pending`]).
    RunPending,
    /// EXPLAIN ANALYZE ([`crate::StoreService::explain_query`]).
    Explain,
    /// EXPLAIN, without running ([`crate::StoreService::plan_query`]).
    Plan,
    /// An update's `WHERE` ([`crate::MutationCommand::Update`]).
    UpdateWhere,
    /// Graph Store Protocol `GET` ([`crate::StoreService::write_graph`],
    /// [`crate::StoreService::execute_graph_read`]).
    GraphRead,
    /// Graph Store Protocol `HEAD` ([`crate::StoreService::graph_exists`]).
    GraphExists,
    /// RDF4J's `GET /statements` ([`crate::StoreService::write_statements`],
    /// [`crate::StoreService::statements`]).
    Statements,
    /// RDF4J's `/size` ([`crate::StoreService::count`]).
    Size,
    /// Why the rules derive a statement ([`crate::StoreService::explain_statement`]).
    ExplainStatement,
    /// A statement's justifications ([`crate::StoreService::justify_statement`]).
    JustifyStatement,
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
            Some(access) if !access.allows_graph(graph) => {
                Err(Refusal::Write(graph.clone()).into())
            }
            _ => Ok(()),
        }
    }

    /// Fails unless the write may change every graph: for operations over the whole
    /// dataset (restores).
    pub(crate) fn require_all(&self, what: &str) -> StoreResult<()> {
        match self {
            Self::All => Ok(()),
            Self::Graphs(_) => Err(Refusal::WriteAll(what.to_owned()).into()),
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
