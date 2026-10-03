//! The integration tests of `nrese-engine`, as one binary: one link instead of one per
//! file. Each file is a module.

mod bulk_load_tests;
mod derived_index_tests;
mod durability_tests;
mod engine_tests;
mod index_encoding_tests;
mod inferred_stack_tests;
mod replication_tests;
mod vector_tests;
mod vocabulary_tests;
