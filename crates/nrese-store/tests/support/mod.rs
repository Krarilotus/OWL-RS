use std::path::PathBuf;

use nrese_store::{StoreConfig, StoreMode};

pub fn catalog_fixture_path(filename: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../benches/nrese-bench-harness/fixtures/catalog-cache")
        .join(filename)
}

pub fn in_memory_store_config() -> StoreConfig {
    StoreConfig {
        mode: StoreMode::InMemory,
        data_dir: std::env::temp_dir().join("unused"),
        preload_ontology: false,
        ontology_path: None,
        ontology_fallbacks: Vec::new(),
    }
}
