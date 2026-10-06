//! `nrese-store` (layer L3): the operations the product offers - query, update, graph
//! store, tell, backup/restore, stats - and the mutation pipeline that owns commit semantics.
//! No HTTP concerns live here. See `docs/ARCHITECTURE.md`.

pub mod access;
pub mod autocomplete;
mod backup;
mod bulk_load;
pub mod catalog;
mod classification;
pub mod config;
mod delta;
pub mod dl;
pub mod draft_check;
mod entailment;
pub mod error;
/// The process's memory and the limit long operations stop at ([`StoreConfig::process_memory_bytes`]).
pub use nrese_exec::memory;
pub mod graph_store;
mod graph_store_executor;
pub mod image_backup;
pub mod jobs;
mod loader;
pub mod mutation;
pub mod namespaces;
pub mod query;
mod query_cache;
mod query_executor;
mod rdf_io;
pub mod reasoning;
pub mod reasoning_state;
pub mod running;
pub mod scope;
pub mod service;
pub mod sessions;
pub mod shacl;
pub mod statements;
mod stats;
pub mod support;
mod tell;
pub mod update;
mod view;

pub use backup::{
    DatasetBackupArtifact, DatasetBackupFormat, DatasetRestoreReport, DatasetRestoreRequest,
};
pub use bulk_load::{BulkLoadReport, BulkLoadRequest, LoadProgress};
pub use classification::{ClassificationReport, RealisationReport};
pub use config::{
    DEFAULT_QUERY_CACHE_BYTES, DEFAULT_SHAPES_GRAPH, FederationConfig, GateSeverity, ShaclGate,
    StoreConfig, StoreMode,
};
pub use delta::MutationDeltaPreview;
pub use dl::{DlAnswers, DlConfig, DlConsistency, DlDetail};
pub use draft_check::{
    DRAFT_CHECK_PROFILES, DRAFT_CHECK_PROTOCOL, DRAFT_CHECK_SEMANTICS, DraftInputs, DraftLimits,
    DraftOperation, DraftOutcome, DraftStatus, DraftTerm, input_hash_mismatch, run_draft_check,
};
pub use entailment::{DlEntailment, Entailment};
pub use error::{IncompleteAnswer, Refusal, StoreError, StoreResult};
pub use graph_store::{
    GraphDeleteReport, GraphReadRequest, GraphReadResult, GraphTarget, GraphWriteReport,
    GraphWriteRequest,
};
pub use image_backup::{
    ImageManifest, prune_wal_archive, read_manifest, restore_image, restore_until,
};
pub use mutation::{
    MutationCommand, MutationCommitReport, MutationError, MutationKind, MutationPipeline,
    MutationReject, MutationTicket, ReasoningRunRecord, RejectAttribution,
    RejectAttributionCandidate,
};
pub use namespaces::{NamespaceMap, Namespaces};
/// The CPU the binary was built for against the one it runs on, for the server's start.
pub use nrese_engine::cpu;
/// How the process returns freed memory to the system, registered by the binary that picks
/// the allocator.
pub use nrese_engine::memory::set_release as set_memory_release;
pub use nrese_engine::{EngineError, IndexEncoding, ReadModel, VocabularyEncoding};
pub use nrese_shacl::{ValidationReport, ValidationResult};
pub use nrese_sparql::{
    CancellationToken, Explanation, PlanStep, PlannedQuery, PlannedStep, QueryEvaluationError,
};
pub use query::{
    GraphResultFormat, QueryResultKind, SerializedQueryResult, SolutionsResultFormat,
    SparqlQueryRequest,
};
pub use query_cache::QueryCacheStats;
pub use query_executor::PreparedQuery;
pub use rdf_io::{convert_file, parse_payload, parse_payload_preserving_blank_nodes};
pub use reasoning::{InferenceStep, MaterialisationReport, OntologyDiagnostic};
pub use reasoning_state::{ConsistencyStatus, ReasoningState};
pub use running::{RunningQueries, RunningQuery};
pub use scope::{ReadContext, ReadScope, Requester, WriteScope};
pub use service::StoreService;
pub use sessions::{SESSION_IDLE, Sessions};
pub use shacl::{
    ShaclResultText, ShaclValidation, ShaclValidationRequest, ShapesSource, ValidatedGraphs,
};
pub use statements::{RdfPayload, StatementOp, StatementPattern, StatementsRequest};
pub use stats::StoreStats;
pub use tell::TellRequest;
pub use update::{SparqlUpdateRequest, UpdateExecutionReport};
