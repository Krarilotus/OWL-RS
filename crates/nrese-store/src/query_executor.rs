//! Query execution: parse once ([`PreparedQuery`]), evaluate on a read view (L2), and
//! serialise the results into any writer as they are produced.
//!
//! Streaming: results are written row by row, so memory stays bounded by the evaluator's
//! own needs (sorting, grouping), not by the result size. A transport that forwards the
//! writer's output as it arrives streams end to end.
//!
//! Cancellation: the token is checked by the executor before every operator, in scans and
//! in its chunked join and grouping loops, and here on every result row.

use std::io::Write;

use nrese_engine::ReadModel;

use nrese_rdf::{GraphName, NamedNode, NamedOrBlankNode};
use nrese_rdf_io::RdfSerializer;
use nrese_sparql::{
    CancellationToken, Explanation, QueryDatasetSpecification, QueryEvaluationError, QueryOptions,
    QueryResults, ReadView, ResultsFormat, WriteResultsError, evaluate_query, explain_query,
    write_results,
};
use nrese_sparql_results::{QueryResultsFormat, QueryResultsSerializer};
use nrese_sparql_syntax::Query;
use nrese_sparql_syntax::algebra::QueryDataset;

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

/// What the store adds to every query it runs.
#[derive(Debug, Clone, Default)]
pub(crate) struct StoreSettings {
    /// [`StoreConfig::union_default_graph`](crate::StoreConfig).
    pub union_default_graph: bool,
    /// The budget for all running queries together, if the store has one.
    pub query_memory: Option<std::sync::Arc<nrese_sparql::SharedBudget>>,
    /// Who answers `SERVICE` calls, once the application installs a client
    /// ([`StoreService::set_service_client`](crate::StoreService::set_service_client)).
    pub services: std::sync::Arc<std::sync::OnceLock<nrese_sparql::Services>>,
    /// The inferred stack is current under a ruleset with equality reasoning
    /// ([`QueryOptions::equality_closed`](nrese_sparql::QueryOptions)).
    pub equality_closed: std::sync::Arc<std::sync::atomic::AtomicBool>,
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
    as_written: bool,
    solutions_format: SolutionsResultFormat,
    graph_format: GraphResultFormat,
}

impl PreparedQuery {
    pub fn parse(request: &SparqlQueryRequest) -> StoreResult<Self> {
        let mut query = nrese_sparql::compat::parse_query(&request.query)?;
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
            dataset: protocol_dataset(&request.default_graphs, &request.named_graphs)?,
            read_model: request
                .read_model
                .or(from_protocol)
                .or(from_query)
                .unwrap_or_default(),
            memory_limit: request.memory_limit,
            as_written: request.as_written,
            solutions_format: request.solutions_format,
            graph_format: request.graph_format,
        })
    }

    /// Sets the output formats. A transport negotiates them once it knows the query form
    /// ([`Self::kind`]), which parsing tells.
    pub fn set_formats(&mut self, solutions: SolutionsResultFormat, graph: GraphResultFormat) {
        self.solutions_format = solutions;
        self.graph_format = graph;
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
/// The dataset a request's protocol parameters describe (`default-graph-uri` and
/// `named-graph-uri` for queries, `using-graph-uri` and `using-named-graph-uri` for
/// updates); `None` if it gives none.
pub(crate) fn protocol_dataset(
    default_graphs: &[String],
    named_graphs: &[String],
) -> StoreResult<Option<QueryDatasetSpecification>> {
    if default_graphs.is_empty() && named_graphs.is_empty() {
        return Ok(None);
    }
    let iri = |value: &String| {
        NamedNode::new(value.as_str()).map_err(|_| StoreError::InvalidGraphIri(value.clone()))
    };
    let mut dataset = QueryDatasetSpecification::new();
    dataset.set_default_graph(
        default_graphs
            .iter()
            .map(|value| iri(value).map(GraphName::from))
            .collect::<StoreResult<_>>()?,
    );
    dataset.set_available_named_graphs(
        named_graphs
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
    store: &StoreSettings,
    cancellation: &CancellationToken,
    out: impl Write,
) -> StoreResult<()> {
    let options = QueryOptions {
        dataset: prepared.dataset.clone(),
        cancellation: Some(cancellation.clone()),
        read_model: prepared.read_model,
        memory_limit: prepared.memory_limit,
        as_written: prepared.as_written,
        shared_memory: store.query_memory.clone(),
        union_default_graph: store.union_default_graph,
        services: store.services.get().cloned(),
        equality_closed: store
            .equality_closed
            .load(std::sync::atomic::Ordering::Acquire),
        pre_bound: None,
        cross_chunk_rows: None,
    };
    let alive = || match cancellation.is_cancelled() {
        true => Err(StoreError::SparqlEvaluation(
            QueryEvaluationError::Cancelled,
        )),
        false => Ok(()),
    };
    let mut out = out;
    let format = match prepared.solutions_format {
        SolutionsResultFormat::Json => Some(ResultsFormat::Json),
        SolutionsResultFormat::Tsv => Some(ResultsFormat::Tsv),
        SolutionsResultFormat::Csv => Some(ResultsFormat::Csv),
        SolutionsResultFormat::Xml => None,
    };
    if let Some(format) = format
        && matches!(prepared.query, Query::Select { .. } | Query::Ask { .. })
        && let Some(written) = write_results(view, &prepared.query, &options, format, &mut out)
    {
        return written.map_err(|error| match error {
            WriteResultsError::Evaluation(error) => StoreError::SparqlEvaluation(error),
            WriteResultsError::Io(error) => StoreError::Io(error),
        });
    }
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
    store: &StoreSettings,
    cancellation: &CancellationToken,
) -> StoreResult<Explanation> {
    let options = QueryOptions {
        dataset: prepared.dataset.clone(),
        cancellation: Some(cancellation.clone()),
        read_model: prepared.read_model,
        memory_limit: prepared.memory_limit,
        as_written: prepared.as_written,
        shared_memory: store.query_memory.clone(),
        union_default_graph: store.union_default_graph,
        services: store.services.get().cloned(),
        equality_closed: store
            .equality_closed
            .load(std::sync::atomic::Ordering::Acquire),
        pre_bound: None,
        cross_chunk_rows: None,
    };
    Ok(explain_query(view, &prepared.query, &options)?)
}
