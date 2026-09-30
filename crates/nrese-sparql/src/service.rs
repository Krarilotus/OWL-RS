//! SPARQL 1.1 Federated Query: who answers `SERVICE` calls.
//!
//! The executor sends the `SERVICE` block as a `SELECT *` query to the endpoint through a
//! [`ServiceClient`], which the application supplies (the server's speaks the SPARQL
//! protocol over HTTP, to the endpoints its configuration allows). Without a client,
//! `SERVICE` is an error, and `SERVICE SILENT` one solution without bindings.
//!
//! A `SERVICE` joined to what the query has already bound is a bind join: the distinct
//! values of the variables the two share go along as `VALUES`, in chunks of
//! [`BIND_CHUNK`] rows, so the endpoint returns only what can join.

use std::error::Error;
use std::sync::Arc;

use crate::results::CancellationToken;
use oxrdf::{Term, Variable};

/// Rows of bound values per `SERVICE` request of a bind join.
pub const BIND_CHUNK: usize = 200;

/// Beyond this many distinct bound values, the `SERVICE` block is sent once, unbound.
pub const BIND_LIMIT: usize = 20_000;

/// The answer of an endpoint: its variables and, per row, a value per variable.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ServiceResults {
    pub variables: Vec<Variable>,
    pub rows: Vec<Vec<Option<Term>>>,
}

/// Runs `SELECT` queries at remote SPARQL endpoints.
pub trait ServiceClient: Send + Sync {
    /// Runs `query` at `endpoint` (an IRI). Implementations should stop when
    /// `cancellation` fires.
    fn select(
        &self,
        endpoint: &str,
        query: &str,
        cancellation: Option<&CancellationToken>,
    ) -> Result<ServiceResults, Box<dyn Error + Send + Sync>>;
}

/// A shared [`ServiceClient`], as the query options carry it.
#[derive(Clone)]
pub struct Services(pub Arc<dyn ServiceClient>);

impl std::fmt::Debug for Services {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Services")
    }
}
