use std::path::PathBuf;

use nrese_store::{StoreConfig, StoreMode};

use super::env_names as names;
use super::source::ConfigSource;

pub(super) fn parse_store_config(source: &dyn ConfigSource) -> StoreConfig {
    let defaults = runtime_default_store_config();

    StoreConfig {
        mode: parse_store_mode(source.get(names::STORE_MODE).as_deref()),
        data_dir: source
            .get(names::DATA_DIR)
            .map(PathBuf::from)
            .unwrap_or(defaults.data_dir),
        preload_ontology: defaults.preload_ontology,
        ontology_path: source.get(names::ONTOLOGY_PATH).map(PathBuf::from),
        ontology_fallbacks: defaults.ontology_fallbacks,
    }
}

fn runtime_default_store_config() -> StoreConfig {
    StoreConfig {
        mode: parse_store_mode(None),
        data_dir: PathBuf::from("./data"),
        preload_ontology: true,
        ontology_path: None,
        ontology_fallbacks: vec![
            PathBuf::from("../Ontology-Development/files/processed/rg_ontology.ttl"),
            PathBuf::from("../MEPHISTO/Ontology-Development/files/processed/rg_ontology.ttl"),
        ],
    }
}

fn parse_store_mode(input: Option<&str>) -> StoreMode {
    match input
        .unwrap_or(default_store_mode_name())
        .to_ascii_lowercase()
        .as_str()
    {
        "inmemory" | "in-memory" | "memory" => StoreMode::InMemory,
        _ => StoreMode::OnDisk,
    }
}

#[cfg(feature = "durable-storage")]
const fn default_store_mode_name() -> &'static str {
    "on-disk"
}

#[cfg(not(feature = "durable-storage"))]
const fn default_store_mode_name() -> &'static str {
    "in-memory"
}

#[cfg(test)]
mod tests {
    use nrese_store::StoreMode;

    use super::parse_store_mode;

    #[test]
    fn store_mode_parser_accepts_memory_aliases() {
        assert_eq!(parse_store_mode(Some("memory")), StoreMode::InMemory);
        assert_eq!(parse_store_mode(Some("in-memory")), StoreMode::InMemory);
    }
}
