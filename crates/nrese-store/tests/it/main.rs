//! The integration tests of `nrese-store`, as one binary: one link instead of one per
//! file. Each file is a module; `support` holds what they share.

mod support;
#[path = "../../examples/perf_lab/support_family.rs"]
mod support_family;

mod backup_restore_tests;
mod bulk_load_tests;
mod catalog_ontology_store_tests;
mod convert_tests;
mod dl;
mod dl_oracle_tests;
mod entailment_tests;
mod equality_compact_tests;
mod equality_pending_tests;
mod explanation_tests;
mod graph_alias_input_tests;
mod image_backup_tests;
mod implicit_prefix_tests;
mod inferred_access_tests;
mod mutation_pipeline_tests;
mod mutation_safety_tests;
mod ql_rewriting_tests;
mod quarantine_tests;
mod query_cache_tests;
mod query_execution_tests;
mod rdf12_tests;
mod rdf12_version_tests;
mod reasoner_ttl_fixture_tests;
mod repository_catalog_tests;
mod restricted_evidence_tests;
mod scope_tests;
mod search_scope_tests;
mod shacl_gate_tests;
mod shacl_request_shapes_tests;
mod store_service_tests;
mod support_cap_tests;
mod support_round_tests;
mod transfer_memory_tests;
mod update_memory_tests;
mod upload_rejection_tests;
mod upload_staging_tests;
mod upload_tests;
mod user_rules_tests;
mod write_path_tests;
