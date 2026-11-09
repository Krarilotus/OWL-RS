//! The integration tests of `nrese-engine`, as one binary: one link instead of one per
//! file. Each file is a module.

mod bulk_load_tests;
mod corpus_admission_tests;
mod corpus_bounds_tests;
mod corpus_isolation_tests;
mod corpus_settings_tests;
mod cursor_tests;
mod derived_index_tests;
mod durability_tests;
mod engine_tests;
mod index_encoding_tests;
mod inferred_stack_tests;
mod replication_tests;
mod text_scope_tests;
mod vector_tests;
mod vocabulary_tests;

/// A budget that refuses nothing, for vector searches whose memory a test doesn't check.
pub fn unlimited() -> &'static nrese_exec::Budget {
    static UNLIMITED: std::sync::OnceLock<nrese_exec::Budget> = std::sync::OnceLock::new();
    UNLIMITED.get_or_init(nrese_exec::Budget::unlimited)
}
