use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use nrese_store::{GateSeverity, ShaclGate, StoreConfig, StoreMode};

use super::env_names as names;
use super::env_values::parse_bool;
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
        query_cache_bytes: parse_cache_bytes(source.get(names::QUERY_CACHE_BYTES).as_deref())?,
        shapes_graph: source
            .get(names::SHACL_SHAPES_GRAPH)
            .unwrap_or(defaults.shapes_graph),
        union_default_graph: parse_default_graph(source.get(names::DEFAULT_GRAPH).as_deref())?,
        total_query_memory_bytes: parse_total_query_memory(
            source.get(names::MAX_TOTAL_QUERY_MEMORY_BYTES).as_deref(),
        )?,
        federation: parse_federation(source)?,
        hide_unnamed_classes: parse_unnamed_classes(
            source.get(names::REASONING_UNNAMED_CLASSES).as_deref(),
        )?,
        verify_on_open: parse_bool(source, names::VERIFY_ON_OPEN, defaults.verify_on_open)?,
        map_checkpoints: parse_bool(source, names::MAP_CHECKPOINTS, defaults.map_checkpoints)?,
        shacl_gate: parse_shacl_gate(
            source.get(names::SHACL_GATE).as_deref(),
            source.get(names::SHACL_GATE_SEVERITY).as_deref(),
        )?,
    })
}

/// The result cache's budget: a size, or a share of the memory the server may use (`5%`).
/// By default 2% of it, at least 64 MiB and at most 8 GiB: on a 64 GB machine 1.3 GB, so
/// results of hundreds of megabytes are kept, as QLever's default cache keeps them.
fn parse_cache_bytes(input: Option<&str>) -> Result<usize> {
    const MAX_DEFAULT: u64 = 8 << 30;
    let floor = nrese_store::DEFAULT_QUERY_CACHE_BYTES as u64;
    let bytes = match input {
        Some(text) => super::units::parse_memory(text)
            .with_context(|| format!("failed to parse {}", names::QUERY_CACHE_BYTES))?
            .unwrap_or(floor),
        None => super::units::machine_memory_bytes()
            .map_or(floor, |memory| (memory / 50).clamp(floor, MAX_DEFAULT)),
    };
    Ok(bytes as usize)
}

/// The SHACL commit gate: `off` (the default), `report`, or `enforce` at a severity
/// (`violation`, the default, `warning` or `info`).
fn parse_shacl_gate(gate: Option<&str>, severity: Option<&str>) -> Result<ShaclGate> {
    let severity = match severity.map(str::to_ascii_lowercase).as_deref() {
        None | Some("violation") => GateSeverity::Violation,
        Some("warning") => GateSeverity::Warning,
        Some("info") => GateSeverity::Info,
        Some(unknown) => bail!(
            "unsupported value '{unknown}' in {} (expected 'violation', 'warning' or 'info')",
            names::SHACL_GATE_SEVERITY
        ),
    };
    Ok(match gate.map(str::to_ascii_lowercase).as_deref() {
        None | Some("off") => ShaclGate::Off,
        Some("report") => ShaclGate::Report,
        Some("enforce") => ShaclGate::Enforce(severity),
        Some(unknown) => bail!(
            "unsupported value '{unknown}' in {} (expected 'off', 'report' or 'enforce')",
            names::SHACL_GATE
        ),
    })
}

/// Memberships in unnamed union classes that nothing consumes: `derive` them (OWL 2 RL
/// as written, the default) or `skip` them.
fn parse_unnamed_classes(input: Option<&str>) -> Result<bool> {
    match input.map(str::to_ascii_lowercase).as_deref() {
        None | Some("derive") => Ok(false),
        Some("skip") => Ok(true),
        Some(unknown) => bail!(
            "unsupported value '{unknown}' in {} (expected 'derive' or 'skip')",
            names::REASONING_UNNAMED_CLASSES
        ),
    }
}

/// `SERVICE` endpoints: comma-separated IRIs or prefixes, `*` for any; none by default.
fn parse_federation(source: &dyn ConfigSource) -> Result<nrese_store::FederationConfig> {
    let defaults = nrese_store::FederationConfig::default();
    let allow: Vec<String> = source
        .get(names::FEDERATION_ALLOW)
        .map(|list| {
            list.split(',')
                .map(str::trim)
                .filter(|entry| !entry.is_empty())
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default();
    for entry in &allow {
        if entry != "*" && !(entry.starts_with("http://") || entry.starts_with("https://")) {
            bail!(
                "{}: '{entry}' is neither an http(s) endpoint (or prefix) nor '*'",
                names::FEDERATION_ALLOW
            );
        }
    }
    Ok(nrese_store::FederationConfig {
        allow,
        timeout_ms: match source.get(names::FEDERATION_TIMEOUT_MS) {
            Some(text) => super::units::parse_duration_ms(&text)
                .with_context(|| format!("failed to parse {}", names::FEDERATION_TIMEOUT_MS))?,
            None => defaults.timeout_ms,
        },
        max_rows: match source.get(names::FEDERATION_MAX_ROWS) {
            Some(text) => text
                .trim()
                .parse()
                .with_context(|| format!("failed to parse {}", names::FEDERATION_MAX_ROWS))?,
            None => defaults.max_rows,
        },
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
