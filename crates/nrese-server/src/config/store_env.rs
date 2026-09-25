use std::path::PathBuf;

use anyhow::{Result, bail};
use nrese_store::{StoreConfig, StoreMode};

use super::env_names as names;
use super::source::ConfigSource;

pub(super) fn parse_store_config(source: &dyn ConfigSource) -> Result<StoreConfig> {
    let defaults = StoreConfig::default();
    Ok(StoreConfig {
        mode: parse_store_mode(source.get(names::STORE_MODE).as_deref())?,
        data_dir: source
            .get(names::DATA_DIR)
            .map(PathBuf::from)
            .unwrap_or(defaults.data_dir),
        ontology_path: source.get(names::ONTOLOGY_PATH).map(PathBuf::from),
    })
}

/// Unknown values are a startup error rather than a silent switch to another storage mode.
fn parse_store_mode(input: Option<&str>) -> Result<StoreMode> {
    let Some(input) = input else {
        return Ok(default_store_mode());
    };
    match input.to_ascii_lowercase().as_str() {
        "inmemory" | "in-memory" | "memory" => Ok(StoreMode::InMemory),
        "ondisk" | "on-disk" | "disk" | "durable" => Ok(StoreMode::OnDisk),
        unknown => bail!(
            "unsupported value '{unknown}' in {} (expected 'in-memory' or 'on-disk')",
            names::STORE_MODE
        ),
    }
}

/// In-memory unless configured otherwise, so a bare `cargo run` never writes to disk.
const fn default_store_mode() -> StoreMode {
    StoreMode::InMemory
}

#[cfg(test)]
mod tests {
    use nrese_store::StoreMode;

    use super::parse_store_mode;

    #[test]
    fn store_mode_parser_accepts_aliases() {
        assert_eq!(
            parse_store_mode(Some("memory")).unwrap(),
            StoreMode::InMemory
        );
        assert_eq!(
            parse_store_mode(Some("in-memory")).unwrap(),
            StoreMode::InMemory
        );
        assert_eq!(
            parse_store_mode(Some("on-disk")).unwrap(),
            StoreMode::OnDisk
        );
    }

    #[test]
    fn store_mode_parser_rejects_unknown_values() {
        assert!(parse_store_mode(Some("in-memroy")).is_err());
    }
}
