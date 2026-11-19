use std::collections::BTreeMap;
use std::sync::Arc;

use nrese_engine::{Engine, EngineConfig};
use nrese_rdf::Term;
use nrese_rdf_io::{RdfFormat, RdfParser};
use nrese_reasoner::rulesets::Ruleset;
use nrese_sparql::{
    Explanation, QueryEvaluationError, QueryOptions, QueryResults, ServiceCall, ServiceClient,
    ServiceResults, Services, evaluate_query,
};
use nrese_sparql_results::{
    QueryResultsFormat, QueryResultsParser, ReaderQueryResultsParserOutput,
};
use nrese_sparql_syntax::SparqlParser;
use nrese_store::{
    BulkLoadRequest, CancellationToken, GraphTarget, InvalidStatements, QlRewritingMode,
    SparqlQueryRequest, StoreConfig, StoreService,
};

use crate::generator::Case;

pub const ENDPOINT: &str = "http://remote.example/sparql";
pub type Row = BTreeMap<String, String>;
pub type Rows = Vec<Row>;

fn row<'a>(bindings: impl Iterator<Item = (&'a nrese_rdf::Variable, &'a Term)>) -> Row {
    bindings
        .map(|(v, t)| (v.as_str().into(), t.to_string()))
        .collect()
}

fn engine(data: &str) -> Engine {
    let engine = Engine::new(EngineConfig::default()).unwrap();
    let mut tx = engine.transaction();
    for quad in RdfParser::from_format(RdfFormat::TriG).for_reader(data.as_bytes()) {
        tx.insert(quad.unwrap().as_ref());
    }
    tx.commit().unwrap();
    engine
}

// Evaluate the actual query sent by the federator, including any VALUES batch.
struct Endpoint(Engine);

impl ServiceClient for Endpoint {
    fn select(
        &self,
        endpoint: &str,
        text: &str,
        _call: ServiceCall<'_>,
    ) -> Result<ServiceResults, QueryEvaluationError> {
        assert_eq!(endpoint, ENDPOINT);
        let query = SparqlParser::new()
            .parse_query(text)
            .map_err(|e| QueryEvaluationError::Service(e.into()))?;
        let snapshot = self.0.snapshot();
        let QueryResults::Solutions(solutions) =
            evaluate_query(&snapshot, &query, &QueryOptions::default())?
        else {
            panic!("endpoint expected SELECT: {text}");
        };
        let variables = solutions.variables().to_vec();
        let rows = solutions
            .map(|solution| {
                let solution = solution?;
                Ok(variables.iter().map(|v| solution.get(v).cloned()).collect())
            })
            .collect::<Result<_, QueryEvaluationError>>()?;
        Ok(ServiceResults { variables, rows })
    }
}

pub struct Routes {
    native: Engine,
    endpoint: Option<Arc<Endpoint>>,
    off: StoreService,
    on: StoreService,
}

impl Routes {
    pub fn open(case: &Case) -> Self {
        let data = case.data.join("\n");
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("data.trig");
        std::fs::write(&file, &data).unwrap();
        let endpoint = case.remote.map(|data| Arc::new(Endpoint(engine(data))));
        let store = |ql_rewriting| {
            let store = StoreService::new(StoreConfig {
                ql_rewriting,
                query_cache_bytes: 1 << 20,
                ..StoreConfig::in_memory()
            })
            .unwrap();
            if let Some(endpoint) = &endpoint {
                store.set_service_client(endpoint.clone());
            }
            store
                .bulk_load(&BulkLoadRequest {
                    files: vec![file.clone()],
                    replace: false,
                    graph: GraphTarget::DefaultGraph,
                    invalid: Some(InvalidStatements::Strict),
                })
                .unwrap();
            store.rematerialise(Ruleset::Owl2Ql).unwrap();
            store
        };
        Self {
            native: engine(&data),
            off: store(QlRewritingMode::Off),
            on: store(QlRewritingMode::On),
            endpoint,
        }
    }

    pub fn native(&self, text: &str, as_written: bool) -> Rows {
        self.native_with(text, as_written, None)
    }

    pub fn native_with(&self, text: &str, as_written: bool, stream_rows: Option<usize>) -> Rows {
        let query = SparqlParser::new()
            .parse_query(text)
            .unwrap_or_else(|e| panic!("{e}: {text}"));
        let options = QueryOptions {
            as_written,
            stream_rows,
            services: self
                .endpoint
                .as_ref()
                .map(|remote| Services(remote.clone())),
            ..QueryOptions::default()
        };
        let snapshot = self.native.snapshot();
        let QueryResults::Solutions(solutions) =
            evaluate_query(&snapshot, &query, &options).unwrap_or_else(|e| panic!("{e}: {text}"))
        else {
            panic!("expected SELECT: {text}");
        };
        let variables = solutions.variables().to_vec();
        solutions
            .map(|r| {
                let r = r.unwrap_or_else(|e| panic!("{e}: {text}"));
                row(variables.iter().filter_map(|v| r.get(v).map(|t| (v, t))))
            })
            .collect()
    }

    pub fn store(&self, route: StoreRoute) -> &StoreService {
        match route {
            StoreRoute::QlOff => &self.off,
            StoreRoute::QlOn => &self.on,
        }
    }
}

pub fn execute_bool(
    store: &StoreService,
    request: &SparqlQueryRequest,
) -> Result<bool, nrese_store::StoreError> {
    let result = store.execute_query(request)?;
    let ReaderQueryResultsParserOutput::Boolean(value) =
        QueryResultsParser::from_format(QueryResultsFormat::Json)
            .for_reader(result.payload.as_slice())
            .unwrap()
    else {
        panic!("expected ASK: {}", request.query);
    };
    Ok(value)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StoreRoute {
    QlOff,
    QlOn,
}

pub const STORES: [StoreRoute; 2] = [StoreRoute::QlOff, StoreRoute::QlOn];

pub fn execute(store: &StoreService, request: &SparqlQueryRequest) -> Rows {
    let result = store
        .execute_query(request)
        .unwrap_or_else(|e| panic!("{e}: {}", request.query));
    let ReaderQueryResultsParserOutput::Solutions(solutions) =
        QueryResultsParser::from_format(QueryResultsFormat::Json)
            .for_reader(result.payload.as_slice())
            .unwrap()
    else {
        panic!("expected SELECT: {}", request.query);
    };
    let variables = solutions.variables().to_vec();
    solutions
        .map(|r| {
            let r = r.unwrap();
            row(variables.iter().filter_map(|v| r.get(v).map(|t| (v, t))))
        })
        .collect()
}

// EXPLAIN after a run of the same query would report a cache read; clear the
// cache so it plans and evaluates the query.
pub fn try_explain(
    store: &StoreService,
    request: &SparqlQueryRequest,
) -> Result<Explanation, nrese_store::StoreError> {
    store.clear_query_cache();
    let prepared = store.prepare_query(request)?;
    store.explain_query(&prepared, &CancellationToken::new())
}

pub fn explain(store: &StoreService, request: &SparqlQueryRequest) -> Explanation {
    try_explain(store, request).unwrap_or_else(|e| panic!("{e}: {}", request.query))
}

// The rewrites an EXPLAIN reports.
pub fn rewrites(explanation: &Explanation) -> &[&'static str] {
    &explanation.rewrites
}
