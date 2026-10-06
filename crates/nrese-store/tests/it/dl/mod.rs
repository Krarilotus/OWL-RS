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
mod versions;

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

/// A DL answer's status as the tests read it: the shared status
/// ([`nrese_sparql::Completeness`]) with what decided it ([`nrese_store::DlDetail`]).
#[derive(Debug)]
pub struct Status {
    pub shared: nrese_sparql::Completeness,
    pub paths: Vec<&'static str>,
    pub bounds: Option<Counts>,
}

/// The bounds' counts: the shared status's, with the candidates proved and refuted.
#[derive(Debug, Clone, Copy)]
pub struct Counts {
    pub lower: u64,
    pub upper: Option<u64>,
    pub proved: u64,
    pub refuted: u64,
    pub unresolved: u64,
}

impl Status {
    pub fn is_complete(&self) -> bool {
        self.shared.complete
    }

    /// The reasons' texts.
    pub fn reasons(&self) -> Vec<String> {
        self.shared.reasons.iter().map(|r| r.text.clone()).collect()
    }
}

/// Runs `prepared`, writing the answers to `out`; its status (with what decided it under
/// `owl2-dl`), `None` where no reasoning path can leave answers out.
pub fn run_dl(
    store: &StoreService,
    prepared: &nrese_store::PreparedQuery,
    out: &mut Vec<u8>,
) -> Result<Option<Status>, nrese_store::StoreError> {
    let reported = store.run_query_dl(prepared, &nrese_store::CancellationToken::new(), out)?;
    Ok(reported.map(|(shared, detail)| Status {
        bounds: shared.bounds.map(|b| Counts {
            lower: b.lower,
            upper: Some(b.upper),
            proved: detail.proved,
            refuted: detail.refuted,
            unresolved: b.unresolved,
        }),
        paths: detail.paths,
        shared,
    }))
}
