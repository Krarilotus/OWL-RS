use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use nrese_store::{StoreConfig, StoreMode};

use super::env_names as names;
use super::env_values::parse_bytes;
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
        query_cache_bytes: parse_bytes(
            source,
            names::QUERY_CACHE_BYTES,
            nrese_store::DEFAULT_QUERY_CACHE_BYTES,
        )?,
        shapes_graph: source
            .get(names::SHACL_SHAPES_GRAPH)
            .unwrap_or(defaults.shapes_graph),
        union_default_graph: parse_default_graph(source.get(names::DEFAULT_GRAPH).as_deref())?,
        total_query_memory_bytes: parse_total_query_memory(
            source.get(names::MAX_TOTAL_QUERY_MEMORY_BYTES).as_deref(),
        )?,
    })
}

/// The share of the machine's memory that all running queries may hold by default.
pub const DEFAULT_TOTAL_QUERY_MEMORY: &str = "50%";

/// Bytes or a share of the machine's memory; `0` is unlimited, and so is a share where
/// the machine's memory isn't known.
fn parse_total_query_memory(input: Option<&str>) -> Result<usize> {
    let text = input.unwrap_or(DEFAULT_TOTAL_QUERY_MEMORY);
    let bytes = super::units::parse_memory(text)
        .with_context(|| format!("failed to parse {}", names::MAX_TOTAL_QUERY_MEMORY_BYTES))?;
    Ok(bytes.unwrap_or(0) as usize)
}

/// What a query without a dataset reads as its default graph: `default` (only the
/// default graph, as the SPARQL specification describes a plain dataset) or `union` (the
/// merge of all graphs, as GraphDB, RDF4J stores and Blazegraph do).
fn parse_default_graph(input: Option<&str>) -> Result<bool> {
    match input.map(str::to_ascii_lowercase).as_deref() {
        None | Some("default") => Ok(false),
        Some("union") => Ok(true),
        Some(unknown) => bail!(
            "unsupported value '{unknown}' in {} (expected 'default' or 'union')",
            names::DEFAULT_GRAPH
        ),
    }
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
    fn default_graph_parser_knows_two_modes() {
        use super::parse_default_graph;
        assert!(!parse_default_graph(None).unwrap());
        assert!(!parse_default_graph(Some("default")).unwrap());
        assert!(parse_default_graph(Some("Union")).unwrap());
        assert!(parse_default_graph(Some("all")).is_err());
    }

    #[test]
    fn store_mode_parser_rejects_unknown_values() {
        assert!(parse_store_mode(Some("in-memroy")).is_err());
    }
}
