use std::collections::BTreeMap;
use std::path::PathBuf;

use anyhow::{Result, anyhow, bail};

use crate::model::{
    BasicAuthConfig, BenchConfig, CatalogSyncConfig, Cli, Command, CompatConfig,
    ConnectionSelection, GenerateConfig, OntologyReasoningFeature, OntologySemanticDialect,
    OntologyServiceSurface, PackConfig, PackExecutionMode, PackMatrixConfig, QueryMixConfig,
    ReferenceConnection, ReferenceKind, SeedConfig, ServiceConnectionConfig, ValidatePackConfig,
    WriteScalingConfig,
};

const DEFAULT_QUERY_WORKLOAD_PATH: &str =
    "benches/nrese-bench-harness/fixtures/workloads/query_workload.json";
const DEFAULT_UPDATE_WORKLOAD_PATH: &str =
    "benches/nrese-bench-harness/fixtures/workloads/update_workload.json";
const DEFAULT_COMPAT_CASES_PATH: &str =
    "benches/nrese-bench-harness/fixtures/compat/protocol_cases.json";
const DEFAULT_SEED_DATASET_PATH: &str =
    "benches/nrese-bench-harness/fixtures/datasets/comparison_seed.ttl";

pub fn parse_cli(args: Vec<String>) -> Result<Cli> {
    if args.len() < 2 {
        print_usage();
        bail!("missing command");
    }

    let command_name = args[1].as_str();
    let options = collect_options(&args[2..])?;

    match command_name {
        "bench" => Ok(Cli {
            command: Command::Bench(BenchConfig {
                nrese: nrese_connection(&options)?,
                reference: optional_reference_connection(&options)?,
                iterations: options
                    .get("--iterations")
                    .map(|value| value.parse::<usize>())
                    .transpose()?
                    .unwrap_or(20),
                query_workload_path: options
                    .get("--query-workload")
                    .map(PathBuf::from)
                    .unwrap_or_else(|| PathBuf::from(DEFAULT_QUERY_WORKLOAD_PATH)),
                update_workload_path: options
                    .get("--update-workload")
                    .map(PathBuf::from)
                    .unwrap_or_else(|| PathBuf::from(DEFAULT_UPDATE_WORKLOAD_PATH)),
                report_json_path: options.get("--report-json").map(PathBuf::from),
            }),
        }),
        "catalog-sync" => Ok(Cli {
            command: Command::CatalogSync(CatalogSyncConfig {
                catalog_path: options
                    .get("--catalog")
                    .map(PathBuf::from)
                    .unwrap_or_else(|| {
                        PathBuf::from(
                            "benches/nrese-bench-harness/fixtures/catalog/ontologies.toml",
                        )
                    }),
                output_dir: options
                    .get("--output-dir")
                    .map(PathBuf::from)
                    .unwrap_or_else(|| {
                        PathBuf::from("benches/nrese-bench-harness/fixtures/catalog-cache")
                    }),
                tier: options.get("--tier").cloned(),
                refresh: options
                    .get("--refresh")
                    .map(|value| parse_bool(value))
                    .transpose()?
                    .unwrap_or(false),
            }),
        }),
        "compat" => Ok(Cli {
            command: Command::Compat(CompatConfig {
                nrese: nrese_connection(&options)?,
                reference: optional_reference_connection(&options)?
                    .ok_or_else(|| anyhow!("missing required option --reference-base-url"))?,
                nrese_profiles: BTreeMap::new(),
                reference_profiles: BTreeMap::new(),
                cases_path: options
                    .get("--cases")
                    .map(PathBuf::from)
                    .unwrap_or_else(|| PathBuf::from(DEFAULT_COMPAT_CASES_PATH)),
                report_json_path: options.get("--report-json").map(PathBuf::from),
            }),
        }),
        "pack" => Ok(Cli {
            command: Command::Pack(PackConfig {
                connections: connection_selection(&options)?,
                workload_pack_path: options
                    .get("--workload-pack")
                    .map(PathBuf::from)
                    .ok_or_else(|| anyhow!("missing required option --workload-pack"))?,
                execution_mode: options
                    .get("--execution-mode")
                    .map(|value| parse_pack_execution_mode(value))
                    .transpose()?
                    .unwrap_or(PackExecutionMode::Full),
                iterations: options
                    .get("--iterations")
                    .map(|value| value.parse::<usize>())
                    .transpose()?
                    .unwrap_or(20),
                report_dir: options.get("--report-dir").map(PathBuf::from),
            }),
        }),
        "pack-validate" => Ok(Cli {
            command: Command::ValidatePack(ValidatePackConfig {
                connections: connection_selection(&options)?,
                workload_pack_path: options
                    .get("--workload-pack")
                    .map(PathBuf::from)
                    .ok_or_else(|| anyhow!("missing required option --workload-pack"))?,
                report_json_path: options.get("--report-json").map(PathBuf::from),
            }),
        }),
        "pack-matrix" => Ok(Cli {
            command: Command::PackMatrix(PackMatrixConfig {
                connections: connection_selection(&options)?,
                catalog_path: options
                    .get("--catalog")
                    .map(PathBuf::from)
                    .unwrap_or_else(|| {
                        PathBuf::from(
                            "benches/nrese-bench-harness/fixtures/catalog/ontologies.toml",
                        )
                    }),
                packs_dir: options
                    .get("--packs-dir")
                    .map(PathBuf::from)
                    .unwrap_or_else(|| PathBuf::from("benches/nrese-bench-harness/fixtures/packs")),
                ontology_name: options.get("--ontology").cloned(),
                execution_mode: options
                    .get("--execution-mode")
                    .map(|value| parse_pack_execution_mode(value))
                    .transpose()?
                    .unwrap_or(PackExecutionMode::Full),
                tier: options.get("--tier").cloned(),
                semantic_dialect: options
                    .get("--semantic-dialect")
                    .map(|value| parse_semantic_dialect(value))
                    .transpose()?,
                reasoning_feature: options
                    .get("--reasoning-feature")
                    .map(|value| parse_reasoning_feature(value))
                    .transpose()?,
                service_coverage: options
                    .get("--service-coverage")
                    .map(|value| parse_service_coverage(value))
                    .transpose()?,
                iterations: options
                    .get("--iterations")
                    .map(|value| value.parse::<usize>())
                    .transpose()?
                    .unwrap_or(20),
                report_dir: options
                    .get("--report-dir")
                    .map(PathBuf::from)
                    .unwrap_or_else(|| PathBuf::from("artifacts/pack-matrix")),
            }),
        }),
        "write-scaling" => Ok(Cli {
            command: Command::WriteScaling(WriteScalingConfig {
                nrese: nrese_connection(&options)?,
                reference: optional_reference_connection(&options)?,
                steps: options
                    .get("--steps")
                    .map(|value| {
                        value
                            .split(',')
                            .map(|step| step.trim().parse::<u64>())
                            .collect::<Result<Vec<_>, _>>()
                    })
                    .transpose()?
                    .unwrap_or_else(|| vec![10_000, 100_000, 500_000, 1_000_000]),
                chunk_triples: options
                    .get("--chunk-triples")
                    .map(|value| value.parse::<u64>())
                    .transpose()?
                    .unwrap_or(16_000),
                samples: options
                    .get("--samples")
                    .map(|value| value.parse::<usize>())
                    .transpose()?
                    .unwrap_or(5),
                reset: options
                    .get("--reset")
                    .map(|value| parse_bool(value))
                    .transpose()?
                    .unwrap_or(true),
                report_json_path: options.get("--report-json").map(PathBuf::from),
            }),
        }),
        "query-mix" => Ok(Cli {
            command: Command::QueryMix(QueryMixConfig {
                endpoint: options
                    .get("--endpoint")
                    .cloned()
                    .ok_or_else(|| anyhow::anyhow!("query-mix needs --endpoint <URL>"))?,
                queries: options
                    .get("--queries")
                    .map(PathBuf::from)
                    .ok_or_else(|| anyhow::anyhow!("query-mix needs --queries <DIR>"))?,
                label: options
                    .get("--label")
                    .cloned()
                    .unwrap_or_else(|| "endpoint".to_owned()),
                warmup: options
                    .get("--warmup")
                    .map(|value| value.parse::<usize>())
                    .transpose()?
                    .unwrap_or(1),
                runs: options
                    .get("--runs")
                    .map(|value| value.parse::<usize>())
                    .transpose()?
                    .unwrap_or(5),
                shuffle_seed: match options.get("--order").map(String::as_str) {
                    None | Some("fixed") => None,
                    Some("shuffled") => Some(
                        options
                            .get("--seed")
                            .map(|value| value.parse::<u64>())
                            .transpose()?
                            .unwrap_or(1),
                    ),
                    Some(other) => bail!("--order is fixed or shuffled, not {other}"),
                },
                timeout_s: options
                    .get("--timeout-s")
                    .map(|value| value.parse::<u64>())
                    .transpose()?
                    .unwrap_or(300),
                clients: options
                    .get("--clients")
                    .map(|value| value.parse::<usize>())
                    .transpose()?
                    .unwrap_or(0),
                duration_s: options
                    .get("--duration-s")
                    .map(|value| value.parse::<u64>())
                    .transpose()?
                    .unwrap_or(60),
                interactive_ms: options
                    .get("--interactive-ms")
                    .map(|value| value.parse::<f64>())
                    .transpose()?
                    .unwrap_or(10_000.0),
                update_endpoint: options.get("--update-endpoint").cloned(),
                write_interval_ms: options
                    .get("--write-interval-ms")
                    .map(|value| value.parse::<u64>())
                    .transpose()?
                    .unwrap_or(100),
                write_graph: options.get("--write-graph").cloned(),
                report_json_path: options.get("--report-json").map(PathBuf::from),
            }),
        }),
        "generate" => Ok(Cli {
            command: Command::Generate(GenerateConfig {
                triples: options
                    .get("--triples")
                    .map(|value| value.parse::<u64>())
                    .transpose()?
                    .unwrap_or(1_000_000),
                out: options
                    .get("--out")
                    .map(PathBuf::from)
                    .ok_or_else(|| anyhow::anyhow!("generate needs --out <PATH>"))?,
            }),
        }),
        "seed" => Ok(Cli {
            command: Command::Seed(SeedConfig {
                nrese: nrese_connection(&options)?,
                reference: optional_reference_connection(&options)?,
                dataset_path: options
                    .get("--dataset")
                    .map(PathBuf::from)
                    .unwrap_or_else(|| PathBuf::from(DEFAULT_SEED_DATASET_PATH)),
                dataset_base_iri: options.get("--dataset-base-iri").cloned(),
                content_type: options.get("--content-type").cloned(),
                replace: options
                    .get("--replace")
                    .map(|value| parse_bool(value))
                    .transpose()?
                    .unwrap_or(true),
            }),
        }),
        "help" | "--help" | "-h" => {
            print_usage();
            std::process::exit(0);
        }
        _ => bail!("unknown command: {command_name}"),
    }
}

fn collect_options(args: &[String]) -> Result<BTreeMap<String, String>> {
    let mut options = BTreeMap::new();
    let mut i = 0usize;

    while i < args.len() {
        let key = &args[i];
        if !key.starts_with("--") {
            bail!("unexpected token: {key}");
        }
        if i + 1 >= args.len() {
            bail!("missing value for {key}");
        }
        options.insert(key.clone(), args[i + 1].clone());
        i += 2;
    }

    Ok(options)
}

fn parse_bool(value: &str) -> Result<bool> {
    match value {
        "true" | "1" | "yes" => Ok(true),
        "false" | "0" | "no" => Ok(false),
        _ => bail!("invalid boolean value: {value}"),
    }
}

fn parse_pack_execution_mode(value: &str) -> Result<PackExecutionMode> {
    match value {
        "full" => Ok(PackExecutionMode::Full),
        "compat-only" => Ok(PackExecutionMode::CompatOnly),
        _ => bail!("invalid pack execution mode: {value}"),
    }
}

fn parse_semantic_dialect(value: &str) -> Result<OntologySemanticDialect> {
    match value {
        "rdfs" => Ok(OntologySemanticDialect::Rdfs),
        "owl" => Ok(OntologySemanticDialect::Owl),
        "foaf" => Ok(OntologySemanticDialect::Foaf),
        "org" => Ok(OntologySemanticDialect::Org),
        "time" => Ok(OntologySemanticDialect::Time),
        "prov-o" => Ok(OntologySemanticDialect::ProvO),
        "skos" => Ok(OntologySemanticDialect::Skos),
        "sosa" => Ok(OntologySemanticDialect::Sosa),
        "ssn" => Ok(OntologySemanticDialect::Ssn),
        "dcat" => Ok(OntologySemanticDialect::Dcat),
        "vcard" => Ok(OntologySemanticDialect::Vcard),
        "dcmi-terms" => Ok(OntologySemanticDialect::DcmiTerms),
        "odrl" => Ok(OntologySemanticDialect::Odrl),
        _ => bail!("invalid semantic dialect: {value}"),
    }
}

fn parse_reasoning_feature(value: &str) -> Result<OntologyReasoningFeature> {
    match value {
        "subclass-closure" => Ok(OntologyReasoningFeature::SubclassClosure),
        "subproperty-closure" => Ok(OntologyReasoningFeature::SubpropertyClosure),
        "domain-range-typing" => Ok(OntologyReasoningFeature::DomainRangeTyping),
        "inverse-property" => Ok(OntologyReasoningFeature::InverseProperty),
        "transitive-property" => Ok(OntologyReasoningFeature::TransitiveProperty),
        "symmetric-property" => Ok(OntologyReasoningFeature::SymmetricProperty),
        "disjointness" => Ok(OntologyReasoningFeature::Disjointness),
        "identity" => Ok(OntologyReasoningFeature::Identity),
        "restrictions" => Ok(OntologyReasoningFeature::Restrictions),
        "list-axioms" => Ok(OntologyReasoningFeature::ListAxioms),
        _ => bail!("invalid reasoning feature: {value}"),
    }
}

fn parse_service_coverage(value: &str) -> Result<OntologyServiceSurface> {
    match value {
        "catalog-sync" => Ok(OntologyServiceSurface::CatalogSync),
        "compat" => Ok(OntologyServiceSurface::Compat),
        "tell" => Ok(OntologyServiceSurface::Tell),
        "graph-store" => Ok(OntologyServiceSurface::GraphStore),
        "query" => Ok(OntologyServiceSurface::Query),
        "reasoner" => Ok(OntologyServiceSurface::Reasoner),
        "benchmark" => Ok(OntologyServiceSurface::Benchmark),
        _ => bail!("invalid service coverage: {value}"),
    }
}

fn parse_basic_auth_opt(
    options: &BTreeMap<String, String>,
    key: &str,
) -> Result<Option<BasicAuthConfig>> {
    options
        .get(key)
        .map(|value| parse_basic_auth(value))
        .transpose()
}

fn parse_basic_auth(value: &str) -> Result<BasicAuthConfig> {
    let Some((username, password)) = value.split_once(':') else {
        bail!("invalid basic auth value, expected username:password");
    };
    if username.is_empty() || password.is_empty() {
        bail!("invalid basic auth value, expected non-empty username and password");
    }

    Ok(BasicAuthConfig {
        username: username.to_owned(),
        password: password.to_owned(),
    })
}

fn required_opt(options: &BTreeMap<String, String>, key: &str) -> Result<String> {
    options
        .get(key)
        .cloned()
        .ok_or_else(|| anyhow!("missing required option {key}"))
}

fn nrese_connection(options: &BTreeMap<String, String>) -> Result<ServiceConnectionConfig> {
    Ok(ServiceConnectionConfig::new(required_opt(
        options,
        "--nrese-base-url",
    )?))
}

fn connection_selection(options: &BTreeMap<String, String>) -> Result<ConnectionSelection> {
    Ok(ConnectionSelection {
        profiles_path: options.get("--connection-profiles").map(PathBuf::from),
        profile_name: options.get("--connection-profile").cloned(),
        nrese_base_url: options.get("--nrese-base-url").cloned(),
        reference_kind: parse_reference_kind_opt(options)?,
        reference_base_url: options.get("--reference-base-url").cloned(),
        reference_basic_auth: parse_basic_auth_opt(options, "--reference-basic-auth")?,
    })
}

fn parse_reference_kind_opt(options: &BTreeMap<String, String>) -> Result<Option<ReferenceKind>> {
    options
        .get("--reference-kind")
        .map(|value| ReferenceKind::parse(value).map_err(|error| anyhow!(error)))
        .transpose()
}

/// A reference endpoint needs both a URL and an engine kind; one without the other is an
/// error rather than a guess.
fn optional_reference_connection(
    options: &BTreeMap<String, String>,
) -> Result<Option<ReferenceConnection>> {
    let kind = parse_reference_kind_opt(options)?;
    let Some(base_url) = options.get("--reference-base-url").cloned() else {
        if kind.is_some() {
            bail!("--reference-kind requires --reference-base-url");
        }
        return Ok(None);
    };
    let kind = kind.ok_or_else(|| {
        anyhow!("--reference-base-url requires --reference-kind <fuseki|graphdb|qlever>")
    })?;

    Ok(Some(ReferenceConnection {
        kind,
        connection: ServiceConnectionConfig {
            basic_auth: parse_basic_auth_opt(options, "--reference-basic-auth")?,
            ..ServiceConnectionConfig::new(base_url)
        },
    }))
}

pub fn print_usage() {
    println!(
        "nrese-bench-harness

USAGE:
  cargo run --manifest-path benches/nrese-bench-harness/Cargo.toml -- bench --nrese-base-url <URL> [--reference-kind <fuseki|graphdb|qlever> --reference-base-url <URL>] [--reference-basic-auth <user:pass>] [--iterations <N>] [--query-workload <PATH>] [--update-workload <PATH>] [--report-json <PATH>]
  cargo run --manifest-path benches/nrese-bench-harness/Cargo.toml -- catalog-sync [--catalog <PATH>] [--output-dir <DIR>] [--tier <small|medium|broad>] [--refresh <true|false>]
  cargo run --manifest-path benches/nrese-bench-harness/Cargo.toml -- compat --nrese-base-url <URL> --reference-kind <fuseki|graphdb|qlever> --reference-base-url <URL> [--reference-basic-auth <user:pass>] [--cases <PATH>] [--report-json <PATH>]
  cargo run --manifest-path benches/nrese-bench-harness/Cargo.toml -- pack [--nrese-base-url <URL>] [--reference-kind <fuseki|graphdb|qlever> --reference-base-url <URL>] [--reference-basic-auth <user:pass>] [--connection-profiles <PATH>] [--connection-profile <NAME>] [--execution-mode <full|compat-only>] --workload-pack <PATH> [--iterations <N>] [--report-dir <DIR>]
  cargo run --manifest-path benches/nrese-bench-harness/Cargo.toml -- pack-validate [--nrese-base-url <URL>] [--reference-kind <fuseki|graphdb|qlever> --reference-base-url <URL>] [--reference-basic-auth <user:pass>] [--connection-profiles <PATH>] [--connection-profile <NAME>] --workload-pack <PATH> [--report-json <PATH>]
  cargo run --manifest-path benches/nrese-bench-harness/Cargo.toml -- pack-matrix [--nrese-base-url <URL>] [--reference-kind <fuseki|graphdb|qlever> --reference-base-url <URL>] [--reference-basic-auth <user:pass>] [--connection-profiles <PATH>] [--connection-profile <NAME>] [--catalog <PATH>] [--packs-dir <DIR>] [--ontology <name>] [--execution-mode <full|compat-only>] [--tier <small|medium|broad>] [--semantic-dialect <dialect>] [--reasoning-feature <feature>] [--service-coverage <surface>] [--iterations <N>] [--report-dir <DIR>]
  cargo run --manifest-path benches/nrese-bench-harness/Cargo.toml -- generate --out <PATH> [--triples <N>]
  cargo run --manifest-path benches/nrese-bench-harness/Cargo.toml -- query-mix --endpoint <URL> --queries <DIR> [--label <NAME>] [--warmup <N>] [--runs <N>] [--order fixed|shuffled [--seed <N>]] [--timeout-s <S>] [--clients <N> --duration-s <S> --interactive-ms <MS> [--update-endpoint <URL> --write-interval-ms <MS> --write-graph <IRI>]] [--report-json <PATH>]
  cargo run --manifest-path benches/nrese-bench-harness/Cargo.toml -- write-scaling --nrese-base-url <URL> [--reference-kind <fuseki|graphdb|qlever> --reference-base-url <URL>] [--steps <triples,...>] [--chunk-triples <N>] [--samples <N>] [--reset <true|false>] [--report-json <PATH>]
  cargo run --manifest-path benches/nrese-bench-harness/Cargo.toml -- seed --nrese-base-url <URL> [--reference-kind <fuseki|graphdb|qlever> --reference-base-url <URL>] [--reference-basic-auth <user:pass>] [--dataset <PATH>] [--dataset-base-iri <IRI>] [--content-type <TYPE>] [--replace <true|false>]
"
    );
}

#[cfg(test)]
mod tests {
    use crate::model::{Command, PackExecutionMode};

    use super::{DEFAULT_COMPAT_CASES_PATH, DEFAULT_QUERY_WORKLOAD_PATH, parse_cli};

    #[test]
    fn parses_bench_defaults() {
        let cli = parse_cli(vec![
            "bench".to_owned(),
            "bench".to_owned(),
            "--nrese-base-url".to_owned(),
            "http://127.0.0.1:8080".to_owned(),
        ])
        .expect("cli");

        match cli.command {
            Command::Bench(config) => {
                assert_eq!(config.iterations, 20);
                assert_eq!(
                    config.query_workload_path.to_string_lossy(),
                    DEFAULT_QUERY_WORKLOAD_PATH
                );
                assert_eq!(config.nrese.base_url, "http://127.0.0.1:8080");
            }
            _ => panic!("expected bench command"),
        }
    }

    #[test]
    fn parses_pack_matrix_defaults() {
        let cli = parse_cli(vec![
            "bench".to_owned(),
            "pack-matrix".to_owned(),
            "--connection-profiles".to_owned(),
            "profiles.toml".to_owned(),
            "--connection-profile".to_owned(),
            "secured-live".to_owned(),
        ])
        .expect("cli");

        match cli.command {
            Command::PackMatrix(config) => {
                assert_eq!(config.connections.nrese_base_url, None);
                assert_eq!(
                    config
                        .connections
                        .profiles_path
                        .as_ref()
                        .map(|path| path.to_string_lossy().to_string()),
                    Some("profiles.toml".to_owned())
                );
                assert_eq!(
                    config.connections.profile_name.as_deref(),
                    Some("secured-live")
                );
                assert_eq!(
                    config.catalog_path.to_string_lossy(),
                    "benches/nrese-bench-harness/fixtures/catalog/ontologies.toml"
                );
                assert_eq!(
                    config.packs_dir.to_string_lossy(),
                    "benches/nrese-bench-harness/fixtures/packs"
                );
                assert!(config.ontology_name.is_none());
                assert_eq!(config.execution_mode, PackExecutionMode::Full);
                assert!(config.semantic_dialect.is_none());
                assert!(config.reasoning_feature.is_none());
                assert!(config.service_coverage.is_none());
                assert_eq!(config.iterations, 20);
            }
            _ => panic!("expected pack-matrix command"),
        }
    }

    #[test]
    fn parses_pack_matrix_ontology_filter() {
        let cli = parse_cli(vec![
            "bench".to_owned(),
            "pack-matrix".to_owned(),
            "--connection-profiles".to_owned(),
            "profiles.toml".to_owned(),
            "--connection-profile".to_owned(),
            "secured-live".to_owned(),
            "--ontology".to_owned(),
            "skos".to_owned(),
        ])
        .expect("cli");

        match cli.command {
            Command::PackMatrix(config) => {
                assert_eq!(config.ontology_name.as_deref(), Some("skos"));
            }
            _ => panic!("expected pack-matrix command"),
        }
    }

    #[test]
    fn parses_seed_command() {
        let cli = parse_cli(vec![
            "bench".to_owned(),
            "seed".to_owned(),
            "--nrese-base-url".to_owned(),
            "http://127.0.0.1:8080".to_owned(),
            "--replace".to_owned(),
            "false".to_owned(),
        ])
        .expect("cli");

        match cli.command {
            Command::Seed(config) => {
                assert!(!config.replace);
                assert_eq!(config.nrese.base_url, "http://127.0.0.1:8080");
            }
            _ => panic!("expected seed command"),
        }
    }

    #[test]
    fn parses_pack_command() {
        let cli = parse_cli(vec![
            "bench".to_owned(),
            "pack".to_owned(),
            "--connection-profiles".to_owned(),
            "profiles.toml".to_owned(),
            "--connection-profile".to_owned(),
            "secured-live".to_owned(),
            "--workload-pack".to_owned(),
            "benches/nrese-bench-harness/fixtures/packs/generic-baseline/pack.toml".to_owned(),
            "--execution-mode".to_owned(),
            "compat-only".to_owned(),
            "--iterations".to_owned(),
            "5".to_owned(),
        ])
        .expect("cli");

        match cli.command {
            Command::Pack(config) => {
                assert_eq!(config.iterations, 5);
                assert!(config.report_dir.is_none());
                assert_eq!(config.execution_mode, PackExecutionMode::CompatOnly);
                assert_eq!(
                    config
                        .connections
                        .profiles_path
                        .as_ref()
                        .map(|path| path.to_string_lossy().to_string()),
                    Some("profiles.toml".to_owned())
                );
                assert_eq!(
                    config.connections.profile_name.as_deref(),
                    Some("secured-live")
                );
            }
            _ => panic!("expected pack command"),
        }
    }

    #[test]
    fn parses_pack_validate_command() {
        let cli = parse_cli(vec![
            "bench".to_owned(),
            "pack-validate".to_owned(),
            "--connection-profiles".to_owned(),
            "profiles.toml".to_owned(),
            "--connection-profile".to_owned(),
            "secured-live".to_owned(),
            "--workload-pack".to_owned(),
            "benches/nrese-bench-harness/fixtures/packs/secured-live-auth-template/pack.toml"
                .to_owned(),
            "--report-json".to_owned(),
            "artifacts/pack-validation-report.json".to_owned(),
        ])
        .expect("cli");

        match cli.command {
            Command::ValidatePack(config) => {
                assert_eq!(
                    config
                        .connections
                        .profiles_path
                        .as_ref()
                        .map(|path| path.to_string_lossy().to_string()),
                    Some("profiles.toml".to_owned())
                );
                assert_eq!(
                    config.connections.profile_name.as_deref(),
                    Some("secured-live")
                );
                assert_eq!(
                    config
                        .report_json_path
                        .as_ref()
                        .map(|path| path.to_string_lossy().to_string()),
                    Some("artifacts/pack-validation-report.json".to_owned())
                );
            }
            _ => panic!("expected pack-validate command"),
        }
    }

    #[test]
    fn parses_compat_defaults() {
        let cli = parse_cli(vec![
            "bench".to_owned(),
            "compat".to_owned(),
            "--nrese-base-url".to_owned(),
            "http://127.0.0.1:8080".to_owned(),
            "--reference-kind".to_owned(),
            "fuseki".to_owned(),
            "--reference-base-url".to_owned(),
            "http://127.0.0.1:3030/ds".to_owned(),
        ])
        .expect("cli");

        match cli.command {
            Command::Compat(config) => assert_eq!(
                config.cases_path.to_string_lossy(),
                DEFAULT_COMPAT_CASES_PATH
            ),
            _ => panic!("expected compat command"),
        }
    }

    #[test]
    fn reference_url_without_kind_is_rejected() {
        let result = parse_cli(vec![
            "bench".to_owned(),
            "compat".to_owned(),
            "--nrese-base-url".to_owned(),
            "http://127.0.0.1:8080".to_owned(),
            "--reference-base-url".to_owned(),
            "http://127.0.0.1:7200/repositories/r".to_owned(),
        ]);
        let error = result.expect_err("kind is required").to_string();
        assert!(error.contains("--reference-kind"), "{error}");
    }

    #[test]
    fn parses_optional_reference_basic_auth() {
        let cli = parse_cli(vec![
            "bench".to_owned(),
            "compat".to_owned(),
            "--nrese-base-url".to_owned(),
            "http://127.0.0.1:8080".to_owned(),
            "--reference-kind".to_owned(),
            "fuseki".to_owned(),
            "--reference-base-url".to_owned(),
            "http://127.0.0.1:3030/ds".to_owned(),
            "--reference-basic-auth".to_owned(),
            "admin:nrese-admin".to_owned(),
        ])
        .expect("cli");

        match cli.command {
            Command::Compat(config) => {
                let auth = config.reference.connection.basic_auth.expect("basic auth");
                assert_eq!(auth.username, "admin");
                assert_eq!(auth.password, "nrese-admin");
            }
            _ => panic!("expected compat command"),
        }
    }

    #[test]
    fn parses_catalog_sync_defaults() {
        let cli = parse_cli(vec!["bench".to_owned(), "catalog-sync".to_owned()]).expect("cli");

        match cli.command {
            Command::CatalogSync(config) => {
                assert!(
                    config
                        .catalog_path
                        .ends_with("fixtures\\catalog\\ontologies.toml")
                        || config
                            .catalog_path
                            .ends_with("fixtures/catalog/ontologies.toml")
                );
                assert!(!config.refresh);
            }
            _ => panic!("expected catalog-sync command"),
        }
    }
}
