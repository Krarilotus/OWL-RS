//! The integration tests of `nrese-store`, as one binary: one link instead of one per
//! file. Each file is a module; `support` holds what they share.

mod support;

mod backup_restore_tests;
mod bulk_load_tests;
mod catalog_ontology_store_tests;
mod catalog_reasoner_fixture_tests;
mod convert_tests;
mod entailment_tests;
mod equality_compact_tests;
mod explanation_tests;
mod image_backup_tests;
mod implicit_prefix_tests;
mod inferred_access_tests;
mod mutation_pipeline_tests;
mod mutation_safety_tests;
mod query_cache_tests;
mod query_execution_tests;
mod rdf12_tests;
mod rdf12_version_tests;
mod reasoner_ttl_fixture_tests;
mod repository_catalog_tests;
mod scope_tests;
mod shacl_gate_tests;
mod store_service_tests;
mod update_safety_tests;
mod user_rules_tests;
