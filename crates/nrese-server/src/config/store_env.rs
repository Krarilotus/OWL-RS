use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use nrese_store::{GateSeverity, ShaclGate, StoreConfig, StoreMode};

use super::env_names as names;
use super::env_values::parse_bool;
use super::source::ConfigSource;

pub(super) fn parse_store_config(source: &dyn ConfigSource) -> Result<StoreConfig> {
    let defaults = StoreConfig::default();
    let equality = parse_equality(source.get(names::REASONING_EQUALITY).as_deref())?;
    Ok(StoreConfig {
        mode: parse_store_mode(source.get(names::STORE_MODE).as_deref())?,
        data_dir: source
            .get(names::DATA_DIR)
            .map(PathBuf::from)
            .unwrap_or(defaults.data_dir),
        ontology_path: source.get(names::ONTOLOGY_PATH).map(PathBuf::from),
        query_cache_bytes: parse_cache_bytes(source.get(names::QUERY_CACHE_BYTES).as_deref())?,
        pinned_queries: source
            .get(names::PINNED_QUERIES)
            .map(|list| {
                list.split(';')
                    .map(str::trim)
                    .filter(|path| !path.is_empty())
                    .map(PathBuf::from)
                    .collect()
            })
            .unwrap_or_default(),
        shapes_graph: source
            .get(names::SHACL_SHAPES_GRAPH)
            .unwrap_or(defaults.shapes_graph),
        union_default_graph: parse_default_graph(source.get(names::DEFAULT_GRAPH).as_deref())?,
        geosparql_stated_only: parse_geosparql_relations(
            source.get(names::GEOSPARQL_RELATIONS).as_deref(),
        )?,
        total_query_memory_bytes: parse_total_query_memory(
            source.get(names::MAX_TOTAL_QUERY_MEMORY_BYTES).as_deref(),
        )?,
        huge_pages: match choice(source, names::HUGE_PAGES, &["off", "on"])? {
            Some(on) => on == 1,
            None => defaults.huge_pages,
        },
        reasoning_release: match choice(source, names::REASONING_RELEASE, &["off", "on"])? {
            Some(on) => on == 1,
            None => defaults.reasoning_release,
        },
        process_memory_bytes: parse_process_memory(
            source.get(names::PROCESS_MEMORY_BYTES).as_deref(),
        )?,
        federation: parse_federation(source)?,
        hide_unnamed_classes: parse_unnamed_classes(
            source.get(names::REASONING_UNNAMED_CLASSES).as_deref(),
        )?,
        ql_rewriting: match choice(
            source,
            names::REASONING_QL_REWRITING,
            &["auto", "on", "off"],
        )? {
            Some(1) => nrese_store::QlRewritingMode::On,
            Some(2) => nrese_store::QlRewritingMode::Off,
            _ => nrese_store::QlRewritingMode::Auto,
        },
        equality_by_representatives: equality.0,
        equality_compact: equality.1,
        equality_canonical_answers: choice(
            source,
            names::REASONING_EQUALITY_ANSWERS,
            &["strict", "canonical"],
        )? == Some(1),
        equality_early_expansion: choice(
            source,
            names::REASONING_EQUALITY_EXPANSION,
            &["late", "early"],
        )? == Some(1),
        support_sets: super::env_values::parse_usize(
            source,
            names::REASONING_SUPPORT_SETS,
            defaults.support_sets,
        )?,
        verify_on_open: parse_bool(source, names::VERIFY_ON_OPEN, defaults.verify_on_open)?,
        map_checkpoints: parse_bool(source, names::MAP_CHECKPOINTS, defaults.map_checkpoints)?,
        checkpoint_after_wal_bytes: match source.get(names::CHECKPOINT_AFTER_WAL_BYTES) {
            Some(text) => super::units::parse_size(&text).with_context(|| {
                format!("failed to parse {}", names::CHECKPOINT_AFTER_WAL_BYTES)
            })?,
            None => defaults.checkpoint_after_wal_bytes,
        },
        wal_archive: parse_bool(source, names::WAL_ARCHIVE, defaults.wal_archive)?,
        index_encoding: match source.get(names::INDEX_ENCODING) {
            None => defaults.index_encoding,
            Some(name) => {
                nrese_store::IndexEncoding::from_name(name.trim()).with_context(|| {
                    format!(
                        "{} must be 'fast' or 'compact', not '{name}'",
                        names::INDEX_ENCODING
                    )
                })?
            }
        },
        vocabulary: match source.get(names::VOCABULARY) {
            None => defaults.vocabulary,
            Some(name) => {
                nrese_store::VocabularyEncoding::from_name(name.trim()).with_context(|| {
                    format!(
                        "{} must be 'plain' or 'fsst', not '{name}'",
                        names::VOCABULARY
                    )
                })?
            }
        },
        bulk_load_memory_bytes: parse_bulk_load_memory(
            source.get(names::BULK_LOAD_MEMORY).as_deref(),
        )?,
        invalid_statements: match choice(
            source,
            names::INVALID_STATEMENTS,
            &["quarantine", "strict"],
        )? {
            None | Some(0) => nrese_store::InvalidStatements::Quarantine,
            Some(_) => nrese_store::InvalidStatements::Strict,
        },
        shacl_gate: parse_shacl_gate(
            source.get(names::SHACL_GATE).as_deref(),
            source.get(names::SHACL_GATE_SEVERITY).as_deref(),
        )?,
        dl: parse_dl(source)?,
    })
}

/// The `owl2-dl` mode's settings (`dl.*`).
fn parse_dl(source: &dyn ConfigSource) -> Result<nrese_store::DlConfig> {
    use nrese_store::{DlAnswers, DlConsistency};
    let defaults = nrese_store::DlConfig::default();
    let answers = match choice(
        source,
        names::DL_ANSWERS,
        &["certain-where-complete", "sound", "exact"],
    )? {
        None => defaults.answers,
        Some(at) => [
            DlAnswers::CertainWhereComplete,
            DlAnswers::Sound,
            DlAnswers::Exact,
        ][at],
    };
    let consistency = match choice(source, names::DL_CONSISTENCY, &["inline", "off"])? {
        None => defaults.consistency,
        Some(at) => [DlConsistency::Inline, DlConsistency::Off][at],
    };
    // Every budget: a value, or 0 or `unlimited` for none.
    use super::env_values::parse_limit;
    let millis = |text: &str| super::units::parse_duration_ms(text);
    let duration = |name, default: Option<std::time::Duration>| {
        let default = default.map(|d| d.as_millis() as u64);
        parse_limit(source, name, default, millis)
            .map(|ms| ms.map(std::time::Duration::from_millis))
    };
    let bytes = |text: &str| super::units::parse_size(text).map(|b| b as usize);
    let count = |text: &str| -> Result<usize> { Ok(text.parse()?) };
    let count64 = |text: &str| -> Result<u64> { Ok(text.parse()?) };
    Ok(nrese_store::DlConfig {
        answers,
        consistency,
        timeout: duration(names::DL_TIMEOUT_MS, defaults.timeout)?,
        memory_bytes: parse_limit(source, names::DL_MEMORY, defaults.memory_bytes, bytes)?,
        max_candidates: parse_limit(
            source,
            names::DL_MAX_CANDIDATES,
            defaults.max_candidates,
            count,
        )?,
        threads: super::env_values::parse_usize(source, names::DL_THREADS, defaults.threads)?,
        max_nodes: parse_limit(source, names::DL_MAX_NODES, defaults.max_nodes, count)?,
        max_branch_points: parse_limit(
            source,
            names::DL_MAX_BRANCH_POINTS,
            defaults.max_branch_points,
            count64,
        )?,
        max_merges: parse_limit(source, names::DL_MAX_MERGES, defaults.max_merges, count64)?,
        explanation_tests: parse_limit(
            source,
            names::DL_EXPLANATION_TESTS,
            defaults.explanation_tests,
            count,
        )?,
        lower_bound_timeout: duration(
            names::DL_LOWER_BOUND_TIMEOUT_MS,
            defaults.lower_bound_timeout,
        )?,
        max_join: parse_limit(source, names::DL_MAX_JOIN, defaults.max_join, count)?,
        max_join_steps: parse_limit(
            source,
            names::DL_MAX_JOIN_STEPS,
            defaults.max_join_steps,
            count,
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
/// `reasoner.equality`: whether full materialisations compute over representatives, and
/// whether the store keeps the closure over them.
fn parse_equality(input: Option<&str>) -> Result<(bool, bool)> {
    Ok(match input.map(str::to_ascii_lowercase).as_deref() {
        None | Some("representatives") => (true, false),
        Some("compact") => (true, true),
        Some("replicate") => (false, false),
        Some(unknown) => bail!(
            "unsupported value '{unknown}' in {} (expected 'representatives', 'compact' or \
             'replicate')",
            names::REASONING_EQUALITY
        ),
    })
}

/// The index of the value of `name` among `choices` (case-insensitive); `None` if unset.
fn choice(source: &dyn ConfigSource, name: &str, choices: &[&str]) -> Result<Option<usize>> {
    let Some(value) = source.get(name) else {
        return Ok(None);
    };
    let value = value.trim().to_ascii_lowercase();
    match choices.iter().position(|c| *c == value) {
        Some(at) => Ok(Some(at)),
        None => bail!(
            "unsupported value '{value}' in {name} (expected {})",
            choices
                .iter()
                .map(|c| format!("'{c}'"))
                .collect::<Vec<_>>()
                .join(" or ")
        ),
    }
}

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

/// The share of the machine's (or the container's) memory the process may hold before
/// long operations stop, by default.
pub const DEFAULT_PROCESS_MEMORY: &str = "75%";

fn parse_process_memory(input: Option<&str>) -> Result<u64> {
    let text = input.unwrap_or(DEFAULT_PROCESS_MEMORY);
    let bytes = super::units::parse_memory(text)
        .with_context(|| format!("failed to parse {}", names::PROCESS_MEMORY_BYTES))?;
    Ok(bytes.unwrap_or(0))
}

/// Bytes or a share of the machine's memory; `0` is unlimited, and so is a share where
/// the machine's memory isn't known.
/// What a bulk load's quads may take before they are spilled to disk: a size or a share of
/// the memory the server may use, `0` for no limit.
pub const DEFAULT_BULK_LOAD_MEMORY: &str = "25%";

fn parse_bulk_load_memory(input: Option<&str>) -> Result<u64> {
    let text = input.unwrap_or(DEFAULT_BULK_LOAD_MEMORY);
    let bytes = super::units::parse_memory(text)
        .with_context(|| format!("failed to parse {}", names::BULK_LOAD_MEMORY))?;
    Ok(bytes.unwrap_or(0))
}

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

fn parse_geosparql_relations(input: Option<&str>) -> Result<bool> {
    match input.map(str::to_ascii_lowercase).as_deref() {
        None | Some("computed") => Ok(false),
        Some("stated") => Ok(true),
        Some(unknown) => bail!(
            "unsupported value '{unknown}' in {} (expected 'computed' or 'stated')",
            names::GEOSPARQL_RELATIONS
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
    use nrese_store::{DlAnswers, DlConfig, DlConsistency, StoreMode};

    use super::super::source::KeyValueSource;
    use super::{names, parse_dl, parse_store_config, parse_store_mode};

    /// Spec: docs/ops/config-reference.md, `reasoner.support_sets` (env
    /// `NRESE_REASONING_SUPPORT_SETS`, default 16): the store keeps the value given, the
    /// largest included (no statement has that many sets: the cap is off).
    #[test]
    fn the_support_sets_setting_reaches_the_store() {
        let cap = |value: Option<&str>| {
            let mut source = KeyValueSource::default();
            if let Some(value) = value {
                source.insert(names::REASONING_SUPPORT_SETS, value);
            }
            parse_store_config(&source).unwrap().support_sets
        };
        assert_eq!(cap(None), 16);
        assert_eq!(cap(Some("4")), 4);
        assert_eq!(cap(Some(usize::MAX.to_string().as_str())), usize::MAX);
    }

    #[test]
    fn dl_settings_parse_and_unknown_values_are_errors() {
        let source = KeyValueSource::default();
        assert_eq!(parse_dl(&source).unwrap(), DlConfig::default());
        let mut source = KeyValueSource::default();
        source.insert(names::DL_ANSWERS, "exact");
        source.insert(names::DL_CONSISTENCY, "off");
        source.insert(names::DL_TIMEOUT_MS, "90s");
        source.insert(names::DL_MEMORY, "1GiB");
        source.insert(names::DL_MAX_CANDIDATES, "50");
        source.insert(names::DL_THREADS, "2");
        let dl = parse_dl(&source).unwrap();
        assert_eq!(dl.answers, DlAnswers::Exact);
        assert_eq!(dl.consistency, DlConsistency::Off);
        assert_eq!(dl.timeout, Some(std::time::Duration::from_secs(90)));
        assert_eq!(dl.memory_bytes, Some(1 << 30));
        assert_eq!((dl.max_candidates, dl.threads), (Some(50), 2));
        let mut source = KeyValueSource::default();
        source.insert(names::DL_ANSWERS, "complete");
        assert!(parse_dl(&source).is_err());
    }

    #[test]
    fn every_dl_budget_takes_a_value_zero_or_unlimited() {
        use std::time::Duration;
        let budgets = [
            names::DL_TIMEOUT_MS,
            names::DL_MEMORY,
            names::DL_MAX_CANDIDATES,
            names::DL_MAX_NODES,
            names::DL_MAX_BRANCH_POINTS,
            names::DL_MAX_MERGES,
            names::DL_EXPLANATION_TESTS,
            names::DL_LOWER_BOUND_TIMEOUT_MS,
            names::DL_MAX_JOIN,
            names::DL_MAX_JOIN_STEPS,
        ];
        let set = |value: &str| {
            let mut source = KeyValueSource::default();
            for name in budgets {
                source.insert(name, value);
            }
            parse_dl(&source).unwrap()
        };
        for none in ["0", "unlimited", "Unlimited"] {
            let dl = set(none);
            assert_eq!(dl.timeout, None, "{none}");
            assert_eq!(dl.lower_bound_timeout, None);
            assert_eq!((dl.memory_bytes, dl.max_candidates), (None, None));
            assert_eq!(
                (dl.max_nodes, dl.max_join, dl.max_join_steps),
                (None, None, None)
            );
            assert_eq!((dl.max_branch_points, dl.max_merges), (None, None));
            assert_eq!(dl.explanation_tests, None);
        }
        let dl = set("7");
        assert_eq!(dl.timeout, Some(Duration::from_millis(7)));
        assert_eq!((dl.memory_bytes, dl.max_nodes), (Some(7), Some(7)));
        assert_eq!((dl.max_branch_points, dl.max_merges), (Some(7), Some(7)));
        // The defaults: the server's time, the candidates, the route thresholds; no
        // memory of a run's own and no deterministic budget.
        let defaults = DlConfig::default();
        assert_eq!(defaults.timeout, Some(Duration::from_secs(30 * 60)));
        assert_eq!(defaults.max_candidates, Some(1_000));
        assert_eq!(defaults.memory_bytes, None);
        assert_eq!(
            (defaults.max_nodes, defaults.max_branch_points),
            (None, None)
        );
        assert_eq!(
            (defaults.max_merges, defaults.explanation_tests),
            (None, None)
        );
    }

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
}
