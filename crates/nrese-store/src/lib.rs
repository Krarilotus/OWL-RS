//! `nrese-store` (layer L3): the operations the product offers - query, update, graph
//! store, tell, backup/restore, stats - and the mutation pipeline that owns commit semantics.
//! No HTTP concerns live here. See `docs/ARCHITECTURE.md`.

mod backup;
mod bulk_load;
mod classification;
pub mod config;
mod datatypes;
mod delta;
pub mod error;
pub mod graph_store;
mod graph_store_executor;
mod loader;
pub mod mutation;
pub mod query;
mod query_cache;
mod query_executor;
mod rdf_io;
pub mod reasoning;
pub mod reasoning_state;
pub mod service;
pub mod shacl;
mod stats;
mod tell;
pub mod update;
mod view;

pub use backup::{
    DatasetBackupArtifact, DatasetBackupFormat, DatasetRestoreReport, DatasetRestoreRequest,
};
pub use bulk_load::{BulkLoadReport, BulkLoadRequest};
pub use classification::ClassificationReport;
pub use config::{
    DEFAULT_QUERY_CACHE_BYTES, DEFAULT_SHAPES_GRAPH, FederationConfig, GateSeverity, ShaclGate,
    StoreConfig, StoreMode,
};
pub use delta::MutationDeltaPreview;
pub use error::{StoreError, StoreResult};
pub use graph_store::{
    GraphDeleteReport, GraphReadRequest, GraphReadResult, GraphTarget, GraphWriteReport,
    GraphWriteRequest,
};
pub use mutation::{
    MutationCommand, MutationCommitReport, MutationError, MutationKind, MutationPipeline,
    MutationReject, MutationTicket, ReasoningRunRecord, RejectAttribution,
    RejectAttributionCandidate,
};
pub use nrese_engine::{EngineError, ReadModel};
pub use nrese_shacl::{ValidationReport, ValidationResult};
pub use nrese_sparql::{CancellationToken, Explanation, PlanStep, QueryEvaluationError};
pub use query::{
    GraphResultFormat, QueryResultKind, SerializedQueryResult, SolutionsResultFormat,
    SparqlQueryRequest,
};
pub use query_cache::QueryCacheStats;
pub use query_executor::PreparedQuery;
pub use rdf_io::convert_file;
pub use reasoning::{MaterialisationReport, OntologyDiagnostic};
pub use reasoning_state::{ConsistencyStatus, ReasoningState};
pub use service::StoreService;
pub use shacl::{
    ShaclResultText, ShaclValidation, ShaclValidationRequest, ShapesSource, ValidatedGraphs,
};
pub use stats::StoreStats;
pub use tell::TellRequest;
pub use update::{SparqlUpdateRequest, UpdateExecutionReport};
