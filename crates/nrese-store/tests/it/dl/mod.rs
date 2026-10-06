//! The `owl2-dl` mode (docs/design/owl2-dl.md §8), through the store's normal interfaces.

mod bounds;
mod classification;
mod consistency;
mod entailment;
mod explain;
mod gates;
mod mode;
mod queries;
mod review;
mod routing;

use std::sync::Arc;

use nrese_reasoner::{ReasonerConfig, ReasonerService, ReasoningMode};
use nrese_store::{
    DlConfig, MutationCommand, MutationError, MutationPipeline, MutationTicket, Requester,
    SparqlUpdateRequest, StoreConfig, StoreService,
};

pub const PREFIXES: &str = "PREFIX : <http://example.com/> \
     PREFIX owl: <http://www.w3.org/2002/07/owl#> \
     PREFIX rdfs: <http://www.w3.org/2000/01/rdf-schema#> \
     PREFIX xsd: <http://www.w3.org/2001/XMLSchema#> ";

/// A store running the `owl2-dl` mode with `dl`.
pub fn pipeline_with(dl: DlConfig) -> MutationPipeline {
    let config = StoreConfig {
        dl,
        ..crate::support::in_memory_store_config()
    };
    let store = StoreService::new(config).expect("store");
    MutationPipeline::new(
        Arc::new(store),
        Arc::new(ReasonerService::new(ReasonerConfig::for_mode(
            ReasoningMode::Owl2Dl,
        ))),
    )
}

pub fn pipeline() -> MutationPipeline {
    pipeline_with(DlConfig::default())
}

/// Commits `INSERT DATA { data }` (Turtle-style, with [`PREFIXES`]).
pub fn insert(pipeline: &MutationPipeline, data: &str) -> Result<u64, MutationError> {
    update(pipeline, &format!("INSERT DATA {{ {data} }}"))
}

/// Commits a SPARQL update (with [`PREFIXES`]); the revision after it.
pub fn update(pipeline: &MutationPipeline, update: &str) -> Result<u64, MutationError> {
    pipeline
        .apply(
            MutationCommand::Update(SparqlUpdateRequest::new(format!("{PREFIXES}{update}"))),
            &Requester::all(),
            &MutationTicket::new(),
        )
        .map(|report| match report {
            nrese_store::MutationCommitReport::Applied { revision } => revision,
            other => panic!("an update reports Applied, not {other:?}"),
        })
}

/// Whether `ASK { pattern }` holds over what the store's reads see.
pub fn ask(pipeline: &MutationPipeline, pattern: &str) -> bool {
    let result = pipeline
        .store()
        .execute_query_str(&format!("{PREFIXES}ASK {{ {pattern} }}"))
        .expect("ask");
    String::from_utf8(result.payload)
        .expect("utf8")
        .contains("true")
}
