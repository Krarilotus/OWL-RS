//! `nrese-store` (layer L3): the operations the product offers - query, update, graph
//! store, tell, backup/restore, stats - and the mutation pipeline that owns commit semantics.
//! No HTTP concerns live here. See `docs/ARCHITECTURE.md`.

mod backup;
pub mod config;
mod delta;
pub mod error;
pub mod graph_store;
mod graph_store_executor;
mod loader;
pub mod mutation;
pub mod query;
mod query_executor;
mod rdf_io;
pub mod service;
mod snapshot;
mod stats;
mod tell;
pub mod update;
mod view;

pub use backup::{
    DatasetBackupArtifact, DatasetBackupFormat, DatasetRestoreReport, DatasetRestoreRequest,
};
pub use config::{StoreConfig, StoreMode};
pub use delta::{MutationDeltaPreview, StagedMutationPreview};
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
pub use query::{
    GraphResultFormat, QueryResultKind, SerializedQueryResult, SolutionsResultFormat,
    SparqlQueryRequest,
};
pub use service::StoreService;
pub use snapshot::StoreDatasetSnapshot;
pub use stats::StoreStats;
pub use tell::TellRequest;
pub use update::{SparqlUpdateRequest, UpdateExecutionReport};
