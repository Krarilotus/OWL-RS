//! Query execution: parse once ([`PreparedQuery`]), evaluate on a read view (L2), and
//! serialise the results into any writer as they are produced.
//!
//! Streaming: results are written row by row, so memory stays bounded by the evaluator's
//! own needs (sorting, grouping), not by the result size. A transport that forwards the
//! writer's output as it arrives streams end to end.
//!
//! Cancellation: the token is checked by the evaluator on every quad it reads and here on
//! every result row. Between two quad reads, spareval's in-memory join and aggregation loops
//! can't be interrupted (0.7 s measured on a 10¹⁰-row cross product); the native executor
//! (Pf3) closes that gap.

use std::io::Write;

use nrese_sparql::{
    CancellationToken, QueryDatasetSpecification, QueryEvaluationError, QueryOptions, QueryResults,
    ReadView, evaluate_query,
};
use oxrdf::{GraphName, NamedNode, NamedOrBlankNode};
use oxrdfio::RdfSerializer;
use sparesults::{QueryResultsFormat, QueryResultsSerializer};
use spargebra::{Query, SparqlParser};

use crate::error::{StoreError, StoreResult};
use crate::query::{
    GraphResultFormat, QueryResultKind, SerializedQueryResult, SolutionsResultFormat,
    SparqlQueryRequest,
};

impl SolutionsResultFormat {
    fn results_format(self) -> QueryResultsFormat {
        match self {
            Self::Json => QueryResultsFormat::Json,
            Self::Xml => QueryResultsFormat::Xml,
            Self::Csv => QueryResultsFormat::Csv,
            Self::Tsv => QueryResultsFormat::Tsv,
        }
    }
}

/// A parsed query with its protocol dataset and output format. Preparing is cheap and
/// reports syntax errors before any result is produced, so a transport can still choose
/// the response status.
#[derive(Debug, Clone)]
pub struct PreparedQuery {
    query: Query,
    dataset: Option<QueryDatasetSpecification>,
    solutions_format: SolutionsResultFormat,
    graph_format: GraphResultFormat,
}

impl PreparedQuery {
    pub fn parse(request: &SparqlQueryRequest) -> StoreResult<Self> {
        Ok(Self {
            query: SparqlParser::new().parse_query(&request.query)?,
            dataset: protocol_dataset(request)?,
            solutions_format: request.solutions_format,
            graph_format: request.graph_format,
        })
    }

    pub fn kind(&self) -> QueryResultKind {
        match self.query {
            Query::Select { .. } => QueryResultKind::Solutions,
            Query::Ask { .. } => QueryResultKind::Boolean,
            Query::Construct { .. } | Query::Describe { .. } => QueryResultKind::Graph,
        }
    }

    pub fn media_type(&self) -> &'static str {
        match self.kind() {
            QueryResultKind::Solutions | QueryResultKind::Boolean => {
                self.solutions_format.media_type()
            }
            QueryResultKind::Graph => self.graph_format.media_type(),
        }
    }
}

/// SPARQL 1.1 Protocol: if `default-graph-uri` or `named-graph-uri` is given, together
/// they replace the query's `FROM` / `FROM NAMED` clauses.
fn protocol_dataset(
    request: &SparqlQueryRequest,
) -> StoreResult<Option<QueryDatasetSpecification>> {
    if request.default_graphs.is_empty() && request.named_graphs.is_empty() {
        return Ok(None);
    }
    let iri = |value: &String| {
        NamedNode::new(value.as_str()).map_err(|_| StoreError::InvalidGraphIri(value.clone()))
    };
    let mut dataset = QueryDatasetSpecification::new();
    dataset.set_default_graph(
        request
            .default_graphs
            .iter()
            .map(|value| iri(value).map(GraphName::from))
            .collect::<StoreResult<_>>()?,
    );
    dataset.set_available_named_graphs(
        request
            .named_graphs
            .iter()
            .map(|value| iri(value).map(NamedOrBlankNode::from))
            .collect::<StoreResult<_>>()?,
    );
    Ok(Some(dataset))
}

/// Evaluates `prepared` on `view` and writes the serialised results to `out` as they are
/// produced. On error the output is incomplete and must be discarded by the caller.
pub(crate) fn run_query(
    view: &impl ReadView,
    prepared: &PreparedQuery,
    cancellation: &CancellationToken,
    out: impl Write,
) -> StoreResult<()> {
    let options = QueryOptions {
        dataset: prepared.dataset.clone(),
        cancellation: Some(cancellation.clone()),
        ..QueryOptions::default()
    };
    let alive = || match cancellation.is_cancelled() {
        true => Err(StoreError::SparqlEvaluation(
            QueryEvaluationError::Cancelled,
        )),
        false => Ok(()),
    };
    match evaluate_query(view, &prepared.query, &options)? {
        QueryResults::Boolean(value) => {
            QueryResultsSerializer::from_format(prepared.solutions_format.results_format())
                .serialize_boolean_to_writer(out, value)?;
        }
        QueryResults::Solutions(solutions) => {
            let mut writer =
                QueryResultsSerializer::from_format(prepared.solutions_format.results_format())
                    .serialize_solutions_to_writer(out, solutions.variables().to_vec())?;
            for solution in solutions {
                alive()?;
                writer.serialize(&solution?)?;
            }
            writer.finish()?;
        }
        QueryResults::Graph(triples) => {
            let mut writer =
                RdfSerializer::from_format(prepared.graph_format.rdf_format()).for_writer(out);
            for triple in triples {
                alive()?;
                writer.serialize_triple(&triple?)?;
            }
            writer.finish()?;
        }
    }
    Ok(())
}

/// Runs a query to completion into memory. For tools, tests and small results; the HTTP
/// endpoint streams through `StoreService::run_query`.
pub fn execute_query(
    view: &impl ReadView,
    request: &SparqlQueryRequest,
) -> StoreResult<SerializedQueryResult> {
    let prepared = PreparedQuery::parse(request)?;
    let mut payload = Vec::new();
    run_query(view, &prepared, &CancellationToken::new(), &mut payload)?;
    Ok(SerializedQueryResult {
        kind: prepared.kind(),
        media_type: prepared.media_type(),
        payload,
    })
}
