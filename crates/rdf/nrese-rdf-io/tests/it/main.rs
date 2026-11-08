//! The integration tests of `nrese-rdf-io`, as one binary: one link instead of one per
//! file. Each file is a module.

mod api_tests;
mod entity_bound_tests;
mod jsonld_tests;
mod oracle;
mod rdfxml_tests;
mod reader_contract_tests;
mod recovery_inputs;
mod split_contract_tests;
mod table;
mod turtle_tests;
mod writer_contract_tests;
