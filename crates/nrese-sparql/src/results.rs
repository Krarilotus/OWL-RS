//! What a query gives back, and what it runs under: results (solutions, a boolean, or
//! triples), their errors, cancellation, and the dataset a request names.
//!
//! These are the evaluator's own types (docs/design/rdf-bundle.md); their
//! methods follow the shape the store and the server use.

use std::error::Error;
use std::fmt;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use nrese_rdf::{GraphName, NamedOrBlankNode, Term, Triple, Variable};
use nrese_sparql_syntax::algebra::QueryDataset;

/// Why a query failed.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum QueryEvaluationError {
    /// The request's [`CancellationToken`] fired: a deadline or a client gone.
    #[error("the query was cancelled")]
    Cancelled,
    /// The data couldn't be read.
    #[error(transparent)]
    Dataset(Box<dyn Error + Send + Sync>),
    /// The query needed more memory than its budget, or than the server's budget for all
    /// queries had left ([`BudgetExceeded::shared`](nrese_exec::BudgetExceeded)).
    #[error(transparent)]
    MemoryLimit(#[from] nrese_exec::BudgetExceeded),
    /// A `SERVICE` call failed.
    #[error("SERVICE call failed: {0}")]
    Service(Box<dyn Error + Send + Sync>),
    /// The query uses something the evaluator doesn't implement.
    #[error("not supported: {0}")]
    Unsupported(String),
    /// A part of the query has a value it can't take (an extension's option, a malformed
    /// literal it needs).
    #[error("{0}")]
    Argument(String),
    #[error("{0}")]
    Unexpected(String),
}

/// A flag a running query checks; cancelling it stops the query at its next check.
#[derive(Debug, Clone, Default)]
pub struct CancellationToken(Arc<AtomicBool>);

impl CancellationToken {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn cancel(&self) {
        self.0.store(true, Ordering::Release);
    }

    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }

    /// The flag itself, for loops that check it without knowing the token.
    pub fn flag(&self) -> Arc<AtomicBool> {
        self.0.clone()
    }
}

/// The dataset a request names (the protocol's `default-graph-uri` and `named-graph-uri`,
/// or a query's `FROM` and `FROM NAMED`): `None` leaves that part as the store has it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct QueryDatasetSpecification {
    default: Option<Vec<GraphName>>,
    named: Option<Vec<NamedOrBlankNode>>,
}

impl QueryDatasetSpecification {
    pub fn new() -> Self {
        Self::default()
    }

    /// The graphs whose merge is the default graph.
    pub fn set_default_graph(&mut self, graphs: Vec<GraphName>) {
        self.default = Some(graphs);
    }

    /// The named graphs `GRAPH` may read.
    pub fn set_available_named_graphs(&mut self, graphs: Vec<NamedOrBlankNode>) {
        self.named = Some(graphs);
    }

    pub fn default_graph_graphs(&self) -> Option<&[GraphName]> {
        self.default.as_deref()
    }

    pub fn available_named_graphs(&self) -> Option<&[NamedOrBlankNode]> {
        self.named.as_deref()
    }

    /// Whether it leaves the store's dataset as it is.
    pub fn is_default_dataset(&self) -> bool {
        self.default.is_none() && self.named.is_none()
    }
}

impl From<QueryDataset> for QueryDatasetSpecification {
    fn from(dataset: QueryDataset) -> Self {
        Self {
            default: Some(dataset.default.into_iter().map(Into::into).collect()),
            named: dataset
                .named
                .map(|named| named.into_iter().map(Into::into).collect()),
        }
    }
}

/// One solution: a value, or none, for each of the result's variables.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuerySolution {
    variables: Arc<[Variable]>,
    values: Vec<Option<Term>>,
}

/// What names a variable of a solution: its position, its name, or the variable.
pub trait VariableIndex {
    fn position(&self, variables: &[Variable]) -> Option<usize>;
}

impl VariableIndex for usize {
    fn position(&self, variables: &[Variable]) -> Option<usize> {
        (*self < variables.len()).then_some(*self)
    }
}

impl VariableIndex for &str {
    fn position(&self, variables: &[Variable]) -> Option<usize> {
        variables.iter().position(|v| v.as_str() == *self)
    }
}

impl VariableIndex for &Variable {
    fn position(&self, variables: &[Variable]) -> Option<usize> {
        variables.iter().position(|v| v == *self)
    }
}

impl VariableIndex for Variable {
    fn position(&self, variables: &[Variable]) -> Option<usize> {
        (&self).position(variables)
    }
}

impl QuerySolution {
    pub fn new(variables: Arc<[Variable]>, values: Vec<Option<Term>>) -> Self {
        debug_assert_eq!(variables.len(), values.len());
        Self { variables, values }
    }

    /// The value of a variable, if the solution binds it.
    pub fn get(&self, index: impl VariableIndex) -> Option<&Term> {
        self.values.get(index.position(&self.variables)?)?.as_ref()
    }

    /// The bound variables with their values.
    pub fn iter(&self) -> impl Iterator<Item = (&Variable, &Term)> {
        self.variables
            .iter()
            .zip(&self.values)
            .filter_map(|(v, t)| Some((v, t.as_ref()?)))
    }

    pub fn variables(&self) -> &[Variable] {
        &self.variables
    }

    pub fn values(&self) -> &[Option<Term>] {
        &self.values
    }

    /// How many variables the solution binds.
    pub fn len(&self) -> usize {
        self.values.iter().filter(|v| v.is_some()).count()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// The value of a variable by position; panics if the solution doesn't bind it.
impl std::ops::Index<usize> for QuerySolution {
    type Output = Term;

    fn index(&self, index: usize) -> &Term {
        self.get(index).expect("the solution binds the variable")
    }
}

/// The value of a variable by name; panics if the solution doesn't bind it.
impl std::ops::Index<&str> for QuerySolution {
    type Output = Term;

    fn index(&self, name: &str) -> &Term {
        self.get(name).expect("the solution binds the variable")
    }
}

impl<'a> IntoIterator for &'a QuerySolution {
    type Item = (&'a Variable, &'a Term);
    type IntoIter = Box<dyn Iterator<Item = (&'a Variable, &'a Term)> + 'a>;

    fn into_iter(self) -> Self::IntoIter {
        Box::new(self.iter())
    }
}

type Rows<'a> = Box<dyn Iterator<Item = Result<Vec<Option<Term>>, QueryEvaluationError>> + 'a>;

/// The solutions of a SELECT, as they are computed.
pub struct QuerySolutionIter<'a> {
    variables: Arc<[Variable]>,
    rows: Rows<'a>,
    /// Reservations live as long as the retained result, including an early drop.
    budget: Option<Arc<nrese_exec::Budget>>,
}

impl<'a> QuerySolutionIter<'a> {
    /// Solutions from rows of values, one per variable.
    pub fn new(
        variables: Arc<[Variable]>,
        rows: impl Iterator<Item = Result<Vec<Option<Term>>, QueryEvaluationError>> + 'a,
    ) -> Self {
        Self {
            variables,
            rows: Box::new(rows),
            budget: None,
        }
    }

    pub fn variables(&self) -> &[Variable] {
        &self.variables
    }

    pub(crate) fn with_budget(mut self, budget: Arc<nrese_exec::Budget>) -> Self {
        self.budget = Some(budget);
        self
    }
}

impl Iterator for QuerySolutionIter<'_> {
    type Item = Result<QuerySolution, QueryEvaluationError>;

    fn next(&mut self) -> Option<Self::Item> {
        let row = self.rows.next()?;
        Some(row.map(|values| QuerySolution::new(Arc::clone(&self.variables), values)))
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        self.rows.size_hint()
    }
}

impl fmt::Debug for QuerySolutionIter<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("QuerySolutionIter")
            .field("variables", &self.variables)
            .finish_non_exhaustive()
    }
}

/// The triples of a CONSTRUCT or DESCRIBE, as they are computed.
pub struct QueryTripleIter<'a> {
    triples: Box<dyn Iterator<Item = Result<Triple, QueryEvaluationError>> + 'a>,
    budget: Option<Arc<nrese_exec::Budget>>,
}

impl<'a> QueryTripleIter<'a> {
    pub fn new(triples: impl Iterator<Item = Result<Triple, QueryEvaluationError>> + 'a) -> Self {
        Self {
            triples: Box::new(triples),
            budget: None,
        }
    }

    pub(crate) fn with_budget(mut self, budget: Arc<nrese_exec::Budget>) -> Self {
        self.budget = Some(budget);
        self
    }
}

impl Iterator for QueryTripleIter<'_> {
    type Item = Result<Triple, QueryEvaluationError>;

    fn next(&mut self) -> Option<Self::Item> {
        self.triples.next()
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        self.triples.size_hint()
    }
}

impl fmt::Debug for QueryTripleIter<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("QueryTripleIter").finish_non_exhaustive()
    }
}

/// The results of a query: solutions (SELECT), a boolean (ASK), or triples (CONSTRUCT,
/// DESCRIBE).
#[derive(Debug)]
pub enum QueryResults<'a> {
    Solutions(QuerySolutionIter<'a>),
    Boolean(bool),
    Graph(QueryTripleIter<'a>),
}

#[cfg(test)]
mod tests {
    use super::*;
    use nrese_rdf::NamedNode;

    #[test]
    fn solutions_answer_by_position_name_and_variable() {
        let variables: Arc<[Variable]> =
            vec![Variable::new_unchecked("a"), Variable::new_unchecked("b")].into();
        let a: Term = NamedNode::new_unchecked("http://e/a").into();
        let solution = QuerySolution::new(variables, vec![Some(a.clone()), None]);
        assert_eq!(solution.get(0), Some(&a));
        assert_eq!(solution.get("a"), Some(&a));
        assert_eq!(solution.get(Variable::new_unchecked("a")), Some(&a));
        assert_eq!(solution.get("b"), None);
        assert_eq!(solution.get("c"), None);
        assert_eq!(solution.iter().count(), 1);
        assert_eq!(solution.len(), 1);
    }

    #[test]
    fn a_cancelled_token_stays_cancelled_in_its_clones() {
        let token = CancellationToken::new();
        let clone = token.clone();
        assert!(!clone.is_cancelled());
        token.cancel();
        assert!(clone.is_cancelled());
    }
}
