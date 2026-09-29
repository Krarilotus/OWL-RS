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

use nrese_engine::ReadModel;

use nrese_sparql::{
    CancellationToken, Explanation, QueryDatasetSpecification, QueryEvaluationError, QueryOptions,
    QueryResults, ReadView, evaluate_query, explain_query,
};
use oxrdf::{GraphName, NamedNode, NamedOrBlankNode};
use oxrdfio::RdfSerializer;
use sparesults::{QueryResultsFormat, QueryResultsSerializer};
use spargebra::algebra::QueryDataset;
use spargebra::{Query, SparqlParser};

use crate::error::{StoreError, StoreResult};
use crate::query::{GraphResultFormat, QueryResultKind, SolutionsResultFormat, SparqlQueryRequest};

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
    /// The request's query text, as sent (the cache key: parsing names aggregate variables
    /// randomly, so the parsed form differs between parses of the same text).
    text: String,
    dataset: Option<QueryDatasetSpecification>,
    read_model: ReadModel,
    memory_limit: Option<usize>,
    solutions_format: SolutionsResultFormat,
    graph_format: GraphResultFormat,
}

impl PreparedQuery {
    pub fn parse(request: &SparqlQueryRequest) -> StoreResult<Self> {
        let mut query = SparqlParser::new().parse_query(&request.query)?;
        let mut default_graphs = request.default_graphs.clone();
        let from_protocol = pseudo_graph_strings(&mut default_graphs);
        let from_query = query_dataset(&mut query)
            .as_mut()
            .and_then(pseudo_graph_iris);
        // A query whose `FROM` held only pseudo-graphs reads the store's default dataset.
        if let Some(dataset) = query_dataset(&mut query)
            && dataset.default.is_empty()
            && dataset.named.as_ref().is_none_or(Vec::is_empty)
        {
            *query_dataset(&mut query) = None;
        }
        let request = SparqlQueryRequest {
            default_graphs,
            ..request.clone()
        };
        Ok(Self {
            query,
            text: request.query.clone(),
            dataset: protocol_dataset(&request)?,
            read_model: request
                .read_model
                .or(from_protocol)
                .or(from_query)
                .unwrap_or_default(),
            memory_limit: request.memory_limit,
            solutions_format: request.solutions_format,
            graph_format: request.graph_format,
        })
    }

    /// Which statements the query reads.
    pub fn read_model(&self) -> ReadModel {
        self.read_model
    }

    /// Everything the serialised result depends on besides the data: the query text, the
    /// dataset parameters, the read model and the output formats.
    pub(crate) fn cache_request(&self) -> String {
        format!(
            "{}\u{0}{:?}\u{0}{:?}\u{0}{:?}\u{0}{:?}",
            self.text, self.dataset, self.read_model, self.solutions_format, self.graph_format
        )
    }

    /// Whether repeating the query may give another answer (see `query_cache::volatile`).
    pub(crate) fn volatile(&self) -> bool {
        crate::query_cache::volatile(&self.text)
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

/// GraphDB's pseudo-graphs: `FROM onto:explicit` reads asserted statements only,
/// `FROM onto:implicit` inferred ones, both together everything.
const EXPLICIT: &str = "http://www.ontotext.com/explicit";
const IMPLICIT: &str = "http://www.ontotext.com/implicit";

fn pseudo_graph_model(explicit: bool, implicit: bool) -> Option<ReadModel> {
    match (explicit, implicit) {
        (true, false) => Some(ReadModel::Asserted),
        (false, true) => Some(ReadModel::Inferred),
        (true, true) => Some(ReadModel::Materialised),
        (false, false) => None,
    }
}

/// Removes the pseudo-graphs from `FROM`; returns the read model they select.
fn pseudo_graph_iris(dataset: &mut QueryDataset) -> Option<ReadModel> {
    let before = dataset.default.clone();
    dataset
        .default
        .retain(|g| g.as_str() != EXPLICIT && g.as_str() != IMPLICIT);
    let has = |iri: &str| before.iter().any(|g| g.as_str() == iri);
    pseudo_graph_model(has(EXPLICIT), has(IMPLICIT))
}

/// Removes the pseudo-graphs from protocol `default-graph-uri` values.
fn pseudo_graph_strings(graphs: &mut Vec<String>) -> Option<ReadModel> {
    let has = |graphs: &[String], iri: &str| graphs.iter().any(|g| g == iri);
    let model = pseudo_graph_model(has(graphs, EXPLICIT), has(graphs, IMPLICIT));
    graphs.retain(|g| g != EXPLICIT && g != IMPLICIT);
    model
}

fn query_dataset(query: &mut Query) -> &mut Option<QueryDataset> {
    match query {
        Query::Select { dataset, .. }
        | Query::Construct { dataset, .. }
        | Query::Describe { dataset, .. }
        | Query::Ask { dataset, .. } => dataset,
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
        read_model: prepared.read_model,
        memory_limit: prepared.memory_limit,
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

/// Runs `prepared` on `view` as [`run_query`] would, consuming the results instead of
/// serialising them, and reports how it ran (EXPLAIN ANALYZE).
pub(crate) fn explain_prepared(
    view: &impl ReadView,
    prepared: &PreparedQuery,
    cancellation: &CancellationToken,
) -> StoreResult<Explanation> {
    let options = QueryOptions {
        dataset: prepared.dataset.clone(),
        cancellation: Some(cancellation.clone()),
        read_model: prepared.read_model,
        memory_limit: prepared.memory_limit,
        ..QueryOptions::default()
    };
    Ok(explain_query(view, &prepared.query, &options)?)
}
