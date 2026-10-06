//! The integration tests of `nrese-server`, as one binary: one link instead of one per
//! file. Each file is a module; `support` holds what they share.

mod support;

mod access_api_tests;
mod access_control_tests;
mod admin_backup_api_tests;
mod classification_api_tests;
mod client_compat_tests;
mod connection_tests;
mod console_ai_api_tests;
mod dl_api_tests;
mod draft_check_tests;
mod engine_api_tests;
mod federation_tests;
mod graph_store_api_tests;
mod http_api_tests;
mod http_fuzz_tests;
mod lifecycle_tests;
mod openapi_tests;
mod packaging_tests;
mod policy_api_tests;
mod query_protocol_tests;
mod rdf4j_protocol_tests;
mod replication_tests;
mod shacl_api_tests;
mod tell_api_tests;
