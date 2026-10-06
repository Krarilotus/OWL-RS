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
    CancellationToken, Explanation, PlannedQuery, QueryDatasetSpecification, QueryEvaluationError,
    QueryOptions, QueryResults, ReadView, ResultsFormat, WriteResultsError, evaluate_query,
    explain_query, plan_query, write_results,
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
    /// [`StoreConfig::geosparql_stated_only`](crate::StoreConfig).
    pub geosparql_stated_only: bool,
    /// The budget for all running queries together, if the store has one.
    pub query_memory: Option<std::sync::Arc<nrese_sparql::SharedBudget>>,
    /// Who answers `SERVICE` calls, once the application installs a client
    /// ([`StoreService::set_service_client`](crate::StoreService::set_service_client)).
    pub services: std::sync::Arc<std::sync::OnceLock<nrese_sparql::Services>>,
    /// The inferred stack is current under a ruleset with equality reasoning
    /// ([`QueryOptions::equality_closed`](nrese_sparql::QueryOptions)).
    pub equality_closed: std::sync::Arc<std::sync::atomic::AtomicBool>,
    /// [`StoreConfig::equality_canonical_answers`](crate::StoreConfig).
    pub equality_canonical: bool,
    /// [`StoreConfig::equality_early_expansion`](crate::StoreConfig).
    pub equality_early_expansion: bool,
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
    access: Option<std::sync::Arc<nrese_sparql::GraphAccess>>,
    /// Who sent it, for the list of running queries.
    origin: Option<String>,
    /// The repository's namespaces the query was parsed with, if it needed them (it used a
    /// prefix it doesn't declare): part of its cache key.
    implicit_prefixes: Option<crate::NamespaceMap>,
    /// Whether the results may use RDF 1.2 (triple terms): then they announce
    /// `version=1.2` ([`Self::announce_rdf12`]).
    rdf12: bool,
    /// Under `owl2-dl`: which answers this query asks for (`None`: `dl.answers`).
    dl_answers: Option<crate::DlAnswers>,
}

impl PreparedQuery {
    pub fn parse(request: &SparqlQueryRequest) -> StoreResult<Self> {
        Self::parse_with(request, None)
    }

    /// [`Self::parse`], where a prefix the query uses without declaring it means what
    /// `namespaces` bind it to (GraphDB's and RDF4J's repository namespaces; the query's own
    /// declarations win). A query that parses without them doesn't depend on them.
    pub fn parse_with(
        request: &SparqlQueryRequest,
        namespaces: Option<&crate::NamespaceMap>,
    ) -> StoreResult<Self> {
        let (mut query, implicit_prefixes) =
            match nrese_sparql::compat::parse_query(&request.query, None) {
                Ok(query) => (query, None),
                Err(error) => match namespaces.filter(|namespaces| !namespaces.is_empty()) {
                    None => return Err(error.into()),
                    Some(namespaces) => {
                        match nrese_sparql::compat::parse_query(&request.query, Some(namespaces)) {
                            Ok(query) => (query, Some(namespaces.clone())),
                            // A mistake of its own: the error without the namespaces.
                            Err(_) => return Err(error.into()),
                        }
                    }
                },
            };
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
            read_model: match !request.scope.sees_inferred() {
                // Without the inferred statements: what was asserted, whatever was asked.
                true => ReadModel::Asserted,
                false => request
                    .read_model
                    .or(from_protocol)
                    .or(from_query)
                    .unwrap_or_default(),
            },
            memory_limit: request.memory_limit,
            as_written: request.as_written,
            solutions_format: request.solutions_format,
            graph_format: request.graph_format,
            access: request.scope.access().cloned(),
            origin: None,
            implicit_prefixes,
            rdf12: false,
            dl_answers: request.dl_answers,
        })
    }

    /// Whether the query itself makes or matches RDF 1.2 terms: triple term patterns
    /// (in its pattern or template), `TRIPLE(…)`, `STRLANGDIR(…)`, or a literal with a base
    /// direction.
    pub(crate) fn uses_rdf12(&self) -> bool {
        use nrese_sparql_syntax::algebra::{Expression, Function, GraphPattern};
        use nrese_sparql_syntax::term::TermPattern;
        use nrese_sparql_syntax::visit::Node;
        let triple = |t: &nrese_sparql_syntax::term::TriplePattern| {
            matches!(t.subject, TermPattern::Triple(_))
                || matches!(t.object, TermPattern::Triple(_))
        };
        let (pattern, template) = match &self.query {
            Query::Select { pattern, .. }
            | Query::Ask { pattern, .. }
            | Query::Describe { pattern, .. } => (pattern, &[][..]),
            Query::Construct {
                pattern, template, ..
            } => (pattern, template.as_slice()),
        };
        template.iter().any(triple)
            || pattern.find(&mut |node| match node {
                Node::Pattern(GraphPattern::Bgp { patterns }) => patterns.iter().any(triple),
                Node::Pattern(GraphPattern::Path {
                    subject, object, ..
                }) => {
                    matches!(subject, TermPattern::Triple(_))
                        || matches!(object, TermPattern::Triple(_))
                }
                Node::Expression(Expression::FunctionCall(
                    Function::Triple | Function::StrLangDir,
                    _,
                )) => true,
                Node::Expression(Expression::Literal(literal)) => literal.direction().is_some(),
                _ => false,
            })
    }

    /// Announces RDF 1.2 in the results: the media type's `version` parameter, the
    /// `version` member of JSON results, and `VERSION "1.2"` in Turtle, TriG, N-Triples and
    /// N-Quads (RDF 1.2 Concepts §2.1; SPARQL 1.2 Query Results JSON §3.1.3).
    pub fn announce_rdf12(&mut self) {
        self.rdf12 = true;
    }

    /// The query's text, as sent.
    pub fn text(&self) -> &str {
        &self.text
    }

    /// Names who sent the query (shown in the list of running queries).
    pub fn set_origin(&mut self, origin: impl Into<String>) {
        self.origin = Some(origin.into());
    }

    /// The parsed query.
    pub(crate) fn query(&self) -> &Query {
        &self.query
    }

    /// Whether the query names its dataset (`FROM`, or the protocol's graphs).
    pub(crate) fn has_dataset(&self) -> bool {
        self.dataset.is_some() || self.query.dataset().is_some()
    }

    /// Under `owl2-dl`: which answers the query asks for (`None`: the store's setting).
    pub fn dl_answers(&self) -> Option<crate::DlAnswers> {
        self.dl_answers
    }

    /// The graphs the query may read; `None`: every graph.
    pub fn access(&self) -> Option<&std::sync::Arc<nrese_sparql::GraphAccess>> {
        self.access.as_ref()
    }

    /// Who sent the query, if the caller said.
    pub fn origin(&self) -> Option<&str> {
        self.origin.as_deref()
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
        // The access too: users who may read different graphs get different answers; and
        // the namespaces a query needed: the same text means another query under others.
        format!(
            "{}\u{0}{:?}\u{0}{:?}\u{0}{:?}\u{0}{:?}\u{0}{:?}\u{0}{:?}",
            self.text,
            self.dataset,
            self.read_model,
            self.solutions_format,
            self.graph_format,
            self.access,
            self.implicit_prefixes
        )
    }

    /// Whether repeating the query on the same data may give another answer: it calls
    /// `RAND`, `UUID`, `STRUUID`, `BNODE` or `NOW` (stable within one evaluation, not across
    /// them), or reads a `SERVICE`, whose data may change without this store's revision.
    /// Decided on the parsed query, so spacing (`RAND ()`) and words in strings or IRIs
    /// don't fool it.
    pub(crate) fn volatile(&self) -> bool {
        use nrese_sparql_syntax::algebra::{Expression, Function, GraphPattern};
        use nrese_sparql_syntax::visit::Node;
        let pattern = match &self.query {
            Query::Select { pattern, .. }
            | Query::Ask { pattern, .. }
            | Query::Describe { pattern, .. }
            | Query::Construct { pattern, .. } => pattern,
        };
        pattern.find(&mut |node| {
            matches!(
                node,
                Node::Pattern(GraphPattern::Service { .. })
                    | Node::Expression(Expression::FunctionCall(
                        Function::Rand
                            | Function::Uuid
                            | Function::StrUuid
                            | Function::BNode
                            | Function::Now,
                        _,
                    ))
            )
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
        match (self.kind(), self.rdf12) {
            (QueryResultKind::Solutions | QueryResultKind::Boolean, false) => {
                self.solutions_format.media_type()
            }
            (QueryResultKind::Solutions | QueryResultKind::Boolean, true) => {
                self.solutions_format.media_type_rdf12()
            }
            (QueryResultKind::Graph, false) => self.graph_format.media_type(),
            (QueryResultKind::Graph, true) => self.graph_format.media_type_rdf12(),
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
        geosparql_stated_only: store.geosparql_stated_only,
        services: store.services.get().cloned(),
        equality_closed: store
            .equality_closed
            .load(std::sync::atomic::Ordering::Acquire),
        equality_canonical: store.equality_canonical,
        equality_early_expansion: store.equality_early_expansion,
        pre_bound: None,
        cross_chunk_rows: None,
        stream_rows: None,
        access: prepared.access.clone(),
    };
    let alive = || match cancellation.is_cancelled() {
        true => Err(StoreError::SparqlEvaluation(
            QueryEvaluationError::Cancelled,
        )),
        false => Ok(()),
    };
    let mut out = out;
    let version = prepared.rdf12.then_some("1.2");
    let format = match prepared.solutions_format {
        SolutionsResultFormat::Json => Some(ResultsFormat::Json),
        SolutionsResultFormat::Tsv => Some(ResultsFormat::Tsv),
        SolutionsResultFormat::Csv => Some(ResultsFormat::Csv),
        SolutionsResultFormat::Xml => None,
    };
    if let Some(format) = format
        && matches!(prepared.query, Query::Select { .. } | Query::Ask { .. })
        && let Some(written) =
            write_results(view, &prepared.query, &options, format, version, &mut out)
    {
        return written.map_err(|error| match error {
            WriteResultsError::Evaluation(error) => StoreError::SparqlEvaluation(error),
            WriteResultsError::Io(error) => StoreError::Io(error),
        });
    }
    match evaluate_query(view, &prepared.query, &options)? {
        QueryResults::Boolean(value) => {
            results_serializer(prepared, version).serialize_boolean_to_writer(out, value)?;
        }
        QueryResults::Solutions(solutions) => {
            let mut writer = results_serializer(prepared, version)
                .serialize_solutions_to_writer(out, solutions.variables().to_vec())?;
            for solution in solutions {
                alive()?;
                writer.serialize(&solution?)?;
            }
            writer.finish()?;
        }
        QueryResults::Graph(triples) => {
            let mut serializer = RdfSerializer::from_format(prepared.graph_format.rdf_format());
            if let Some(version) = version {
                serializer = serializer.with_version(version);
            }
            let mut writer = serializer.for_writer(out);
            for triple in triples {
                alive()?;
                writer.serialize_triple(&triple?)?;
            }
            writer.finish()?;
        }
    }
    Ok(())
}

/// A query's answers, collected (the `owl2-dl` mode compares and completes them before
/// writing).
pub(crate) enum Answers {
    Boolean(bool),
    Solutions {
        variables: std::sync::Arc<[nrese_sparql_syntax::term::Variable]>,
        rows: Vec<Vec<Option<nrese_rdf::Term>>>,
    },
    Graph(Vec<nrese_rdf::Triple>),
}

/// Evaluates `prepared` on `view` and collects its answers.
pub(crate) fn evaluate_prepared(
    view: &impl ReadView,
    prepared: &PreparedQuery,
    store: &StoreSettings,
    cancellation: &CancellationToken,
) -> StoreResult<Answers> {
    let options = explain_options(prepared, store, cancellation);
    Ok(match evaluate_query(view, &prepared.query, &options)? {
        QueryResults::Boolean(value) => Answers::Boolean(value),
        QueryResults::Solutions(solutions) => {
            let variables: std::sync::Arc<[nrese_sparql_syntax::term::Variable]> =
                solutions.variables().into();
            let mut rows = Vec::new();
            for solution in solutions {
                rows.push(solution?.values().to_vec());
            }
            Answers::Solutions { variables, rows }
        }
        QueryResults::Graph(triples) => Answers::Graph(triples.collect::<Result<Vec<_>, _>>()?),
    })
}

/// Writes collected answers in `prepared`'s format.
pub(crate) fn write_answers(
    prepared: &PreparedQuery,
    answers: Answers,
    out: impl Write,
) -> StoreResult<()> {
    let version = prepared.rdf12.then_some("1.2");
    match answers {
        Answers::Boolean(value) => {
            results_serializer(prepared, version).serialize_boolean_to_writer(out, value)?;
        }
        Answers::Solutions { variables, rows } => {
            let mut writer = results_serializer(prepared, version)
                .serialize_solutions_to_writer(out, variables.to_vec())?;
            for values in rows {
                writer.serialize(&nrese_sparql::QuerySolution::new(
                    std::sync::Arc::clone(&variables),
                    values,
                ))?;
            }
            writer.finish()?;
        }
        Answers::Graph(triples) => {
            let mut serializer = RdfSerializer::from_format(prepared.graph_format.rdf_format());
            if let Some(version) = version {
                serializer = serializer.with_version(version);
            }
            let mut writer = serializer.for_writer(out);
            for triple in &triples {
                writer.serialize_triple(triple)?;
            }
            writer.finish()?;
        }
    }
    Ok(())
}

/// The results serializer of `prepared`'s format, announcing `version`.
fn results_serializer(
    prepared: &PreparedQuery,
    version: Option<&'static str>,
) -> QueryResultsSerializer {
    let serializer =
        QueryResultsSerializer::from_format(prepared.solutions_format.results_format());
    match version {
        Some(version) => serializer.with_version(version),
        None => serializer,
    }
}

/// Runs `prepared` on `view` as [`run_query`] would, consuming the results instead of
/// serialising them, and reports how it ran (EXPLAIN ANALYZE).
pub(crate) fn explain_prepared(
    view: &impl ReadView,
    prepared: &PreparedQuery,
    store: &StoreSettings,
    cancellation: &CancellationToken,
) -> StoreResult<Explanation> {
    let options = explain_options(prepared, store, cancellation);
    Ok(explain_query(view, &prepared.query, &options)?)
}

/// The plan `prepared` would run as on `view`, with estimated rows, without running it
/// (EXPLAIN).
pub(crate) fn plan_prepared(
    view: &impl ReadView,
    prepared: &PreparedQuery,
    store: &StoreSettings,
) -> StoreResult<PlannedQuery> {
    let options = explain_options(prepared, store, &CancellationToken::new());
    Ok(plan_query(view, &prepared.query, &options)?)
}

/// The options `prepared` runs with, for an explanation.
fn explain_options(
    prepared: &PreparedQuery,
    store: &StoreSettings,
    cancellation: &CancellationToken,
) -> QueryOptions {
    QueryOptions {
        dataset: prepared.dataset.clone(),
        cancellation: Some(cancellation.clone()),
        read_model: prepared.read_model,
        memory_limit: prepared.memory_limit,
        as_written: prepared.as_written,
        shared_memory: store.query_memory.clone(),
        union_default_graph: store.union_default_graph,
        geosparql_stated_only: store.geosparql_stated_only,
        services: store.services.get().cloned(),
        equality_closed: store
            .equality_closed
            .load(std::sync::atomic::Ordering::Acquire),
        equality_canonical: store.equality_canonical,
        equality_early_expansion: store.equality_early_expansion,
        pre_bound: None,
        cross_chunk_rows: None,
        stream_rows: None,
        access: prepared.access.clone(),
    }
}
