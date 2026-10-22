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

use std::sync::Arc;

use nrese_exec::{Budget, ChargedVec, Reservation, Stop, StopCheck, Ticker};
use nrese_rdf::{NamedOrBlankNode, Term, Triple, Variable};

use crate::{CancellationToken, QueryEvaluationError};

mod json;

/// Rows of bound values per `SERVICE` request of a bind join.
pub const BIND_CHUNK: usize = 200;

/// Beyond this many distinct bound values, the `SERVICE` block is sent once, unbound.
pub const BIND_LIMIT: usize = 20_000;

/// The answer of an endpoint: its variables and, per row, a value per variable.
pub struct ServiceResults {
    variables: Vec<Variable>,
    // Payloads precede their reservations in drop order. The outer capacity has
    // ChargedVec's own token; payload holds inner slots and recursive term heap.
    rows: ChargedVec<Vec<Option<Term>>>,
    payload: Reservation,
}

impl ServiceResults {
    /// An empty answer on the caller's actual budget. Variable metadata follows
    /// the evaluator's existing query-size treatment; no row storage is allocated.
    pub fn new(variables: Vec<Variable>, budget: &Arc<Budget>) -> Self {
        Self {
            variables,
            rows: ChargedVec::new(Arc::clone(budget)),
            payload: Reservation::new(Arc::clone(budget)),
        }
    }

    pub fn variables(&self) -> &[Variable] {
        &self.variables
    }

    pub fn rows(&self) -> &[Vec<Option<Term>>] {
        &self.rows
    }

    /// O(cells + recursive term nodes + copied bytes). The source row stays
    /// borrowed; every destination capacity/payload is admitted before copying.
    pub fn push_row(
        &mut self,
        row: &[Option<Term>],
        stop: &dyn StopCheck,
    ) -> Result<(), QueryEvaluationError> {
        let mut ticker = Ticker::new(Check(stop)).map_err(QueryEvaluationError::from)?;
        if row.len() != self.variables.len() {
            return Err(QueryEvaluationError::Service(
                "a SERVICE row has the wrong width".into(),
            ));
        }
        let mut bytes = 0;
        for value in row {
            ticker.tick(1).map_err(QueryEvaluationError::from)?;
            if let Some(term) = value {
                bytes = add(bytes, clone_heap(term, &mut ticker)?)?;
            }
        }
        self.rows.reserve(1)?;
        let budget = Arc::clone(self.payload.handle());
        // Declared first: on early return/unwind the copied items drop before heap.
        let mut heap = Reservation::new(Arc::clone(&budget));
        heap.charge(bytes)?;
        let mut copied = ChargedVec::with_capacity(budget, row.len())?;
        for value in row {
            ticker.check().map_err(QueryEvaluationError::from)?;
            copied.push(value.clone())?;
            ticker.check().map_err(QueryEvaluationError::from)?;
        }
        ticker.finish().map_err(QueryEvaluationError::from)?;
        let (copied, slots) = copied.into_parts();
        self.rows.push(copied)?;
        self.payload.absorb(slots);
        self.payload.absorb(heap);
        Ok(())
    }

    /// Import borrows labels across every row. Only after that borrow ends may
    /// native evaluation take its query metadata and free the decoded response.
    pub(crate) fn into_variables(self) -> Vec<Variable> {
        let Self {
            variables,
            rows,
            payload,
        } = self;
        drop(rows);
        drop(payload);
        variables
    }
}

impl std::fmt::Debug for ServiceResults {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ServiceResults")
            .field("variables", &self.variables)
            .field("rows", &self.rows())
            .finish()
    }
}

impl PartialEq for ServiceResults {
    fn eq(&self, other: &Self) -> bool {
        self.variables == other.variables && self.rows() == other.rows()
    }
}

// The exact components String::clone/Box::clone copy, using lexical lengths,
// not spare source capacity. All triple work reaches the same stop source.
fn clone_heap(term: &Term, ticker: &mut Ticker<Check<'_>>) -> Result<usize, QueryEvaluationError> {
    ticker.tick(1).map_err(QueryEvaluationError::from)?;
    match term {
        Term::NamedNode(node) => Ok(node.as_str().len()),
        Term::BlankNode(node) => Ok(node.as_str().len()),
        Term::Literal(literal) => {
            let extra = match literal.language() {
                Some(language) => language.len(),
                None if literal.datatype() != nrese_rdf::vocab::xsd::STRING => {
                    literal.datatype().as_str().len()
                }
                None => 0,
            };
            add(literal.value().len(), extra)
        }
        Term::Triple(triple) => {
            let subject = match &triple.subject {
                NamedOrBlankNode::NamedNode(node) => node.as_str().len(),
                NamedOrBlankNode::BlankNode(node) => node.as_str().len(),
            };
            add(
                add(
                    add(size_of::<Triple>(), subject)?,
                    triple.predicate.as_str().len(),
                )?,
                clone_heap(&triple.object, ticker)?,
            )
        }
    }
}

fn add(left: usize, right: usize) -> Result<usize, QueryEvaluationError> {
    left.checked_add(right).ok_or_else(|| {
        QueryEvaluationError::Unexpected("a SERVICE response exceeds its representation".into())
    })
}

/// Borrow the exact stop implementation; no token reconstruction or owned source.
pub(crate) struct Check<'a>(pub &'a dyn StopCheck);

impl StopCheck for Check<'_> {
    fn check(&self) -> Result<(), Stop> {
        self.0.check()
    }
}

/// Runs `SELECT` queries at remote SPARQL endpoints.
pub trait ServiceClient: Send + Sync {
    /// Runs `query` at `endpoint` using the actual invocation budget and original cancellation token.
    /// Memory/cancellation errors remain operational errors under SERVICE SILENT.
    fn select(
        &self,
        endpoint: &str,
        query: &str,
        budget: &Arc<Budget>,
        cancellation: Option<&CancellationToken>,
    ) -> Result<ServiceResults, QueryEvaluationError>;
}

/// A shared [`ServiceClient`], as the query options carry it.
#[derive(Clone)]
pub struct Services(pub Arc<dyn ServiceClient>);

impl std::fmt::Debug for Services {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Services")
    }
}

#[cfg(test)]
mod tests;
