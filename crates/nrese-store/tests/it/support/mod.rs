//! Shared helpers for `nrese-store` integration tests.

use std::path::PathBuf;
use std::sync::Arc;

use nrese_reasoner::{ReasonerConfig, ReasonerService, ReasoningMode};
use nrese_store::{
    MutationCommand, MutationPipeline, MutationTicket, ReadModel, SolutionsResultFormat,
    SparqlQueryRequest, SparqlUpdateRequest, StoreConfig, StoreService,
};

pub fn catalog_fixture_path(filename: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../benches/nrese-bench-harness/fixtures/catalog-cache")
        .join(filename)
}

pub fn in_memory_store_config() -> StoreConfig {
    StoreConfig::in_memory()
}

/// An inferred statement: subject, predicate, object (IRIs without brackets, other terms
/// in N-Triples form).
pub type Inferred = Vec<(String, String, String)>;

/// The inferred statements of `store` (the inferred read model).
pub fn inferred_statements(store: &StoreService) -> Result<Inferred, Box<dyn std::error::Error>> {
    let mut request = SparqlQueryRequest::all("SELECT ?s ?p ?o WHERE { ?s ?p ?o }");
    request.read_model = Some(ReadModel::Inferred);
    request.solutions_format = SolutionsResultFormat::Tsv;
    let tsv = String::from_utf8(store.execute_query(&request)?.payload)?;
    let term = |t: &str| {
        t.strip_prefix('<')
            .and_then(|t| t.strip_suffix('>'))
            .unwrap_or(t)
            .to_owned()
    };
    Ok(tsv
        .lines()
        .skip(1)
        .filter_map(|line| {
            let mut parts = line.split('\t');
            Some((
                term(parts.next()?),
                term(parts.next()?),
                term(parts.next()?),
            ))
        })
        .collect())
}

/// Loads an official catalog ontology, applies `update` through the mutation pipeline
/// under reasoner v2 (`owl2-rl`), and returns the inferred statements.
pub fn run_owl2_rl_catalog_fixture(
    filename: &str,
    update: &str,
) -> Result<Inferred, Box<dyn std::error::Error>> {
    let store = Arc::new(StoreService::new(StoreConfig {
        ontology_path: Some(catalog_fixture_path(filename)),
        ..in_memory_store_config()
    })?);
    store.rematerialise(nrese_reasoner::rulesets::Ruleset::Owl2Rl)?;
    let pipeline = MutationPipeline::new(
        Arc::clone(&store),
        Arc::new(ReasonerService::new(ReasonerConfig::for_mode(
            ReasoningMode::Owl2Rl,
        ))),
    );
    pipeline.apply(
        MutationCommand::Update(SparqlUpdateRequest::new(update)),
        &nrese_store::Requester::all(),
        &MutationTicket::new(),
    )?;
    inferred_statements(&store)
}

pub fn assert_inferred_triple(inferred: &Inferred, subject: &str, predicate: &str, object: &str) {
    assert!(
        inferred
            .iter()
            .any(|(s, p, o)| s == subject && p == predicate && o == object),
        "expected inferred triple ({subject}, {predicate}, {object}), got {} inferred",
        inferred.len()
    );
}
