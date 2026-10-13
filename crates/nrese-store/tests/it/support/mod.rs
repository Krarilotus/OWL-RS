//! Shared helpers for `nrese-store` integration tests.

use std::path::PathBuf;
use std::sync::Arc;

use nrese_reasoner::{ReasonerConfig, ReasonerService};
use nrese_store::{
    DatasetRestoreReport, DatasetRestoreRequest, GraphDeleteReport, GraphTarget, GraphWriteReport,
    GraphWriteRequest, MutationCommand, MutationCommitReport, MutationError, MutationPipeline,
    MutationTicket, ReadModel, Requester, SolutionsResultFormat, SparqlQueryRequest,
    SparqlUpdateRequest, StoreConfig, StoreService, UpdateExecutionReport,
};

/// A store's writes in tests, through the mutation pipeline's gates (the store has no
/// write around them): a pipeline without reasoning, as a store without a reasoner is
/// written. Like any such pipeline it registers no rules with the store and switches its
/// `owl2-dl` mode off, so a test of a store with a reasoner writes through that store's
/// own pipeline instead, and data meant to bypass the gates goes in
/// [`write_past_the_pipeline`].
pub trait Writes {
    /// The pipeline the writes go through.
    fn writer(&self) -> MutationPipeline;

    fn execute_update(
        &self,
        request: &SparqlUpdateRequest,
    ) -> Result<UpdateExecutionReport, MutationError> {
        self.writer().execute_update(request)
    }

    fn execute_update_str(&self, update: &str) -> Result<UpdateExecutionReport, MutationError> {
        self.writer().execute_update_str(update)
    }

    fn execute_graph_write(
        &self,
        request: &GraphWriteRequest,
    ) -> Result<GraphWriteReport, MutationError> {
        self.writer().execute_graph_write(request)
    }

    fn execute_graph_delete(
        &self,
        target: &GraphTarget,
    ) -> Result<GraphDeleteReport, MutationError> {
        self.writer().execute_graph_delete(target)
    }

    fn restore_dataset(
        &self,
        request: &DatasetRestoreRequest,
    ) -> Result<DatasetRestoreReport, MutationError> {
        match self
            .writer()
            .apply_as_owner(MutationCommand::Restore(request.clone()))?
        {
            MutationCommitReport::Restore(report) => Ok(report),
            other => unreachable!("restore produced {other:?}"),
        }
    }

    /// `command` for `requester`, as a request is applied.
    fn apply(
        &self,
        command: &MutationCommand,
        requester: &Requester,
    ) -> Result<MutationCommitReport, MutationError> {
        self.writer()
            .apply(command.clone(), requester, &MutationTicket::new())
    }
}

impl Writes for StoreService {
    fn writer(&self) -> MutationPipeline {
        MutationPipeline::new(
            Arc::new(self.clone()),
            Arc::new(ReasonerService::new(ReasonerConfig::default())),
        )
    }
}

/// Loads the TriG `data` past the mutation pipeline, as a bulk load of a file does: no
/// gate, no reasoning; the store's closure is no longer current. For tests of what the
/// store does with data that came in that way.
pub fn load_past_the_pipeline(store: &StoreService, data: &str) {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("data.trig");
    std::fs::write(&file, data).unwrap();
    store
        .bulk_load(&nrese_store::BulkLoadRequest {
            files: vec![file],
            replace: false,
            graph: GraphTarget::DefaultGraph,
            invalid: None,
        })
        .unwrap();
}

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

pub fn assert_inferred_triple(inferred: &Inferred, subject: &str, predicate: &str, object: &str) {
    assert!(
        inferred
            .iter()
            .any(|(s, p, o)| s == subject && p == predicate && o == object),
        "expected inferred triple ({subject}, {predicate}, {object}), got {} inferred",
        inferred.len()
    );
}
