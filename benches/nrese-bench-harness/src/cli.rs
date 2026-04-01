use std::collections::BTreeMap;
use std::path::PathBuf;

use anyhow::{Result, anyhow, bail};

use crate::model::{
    BasicAuthConfig, BenchConfig, CatalogSyncConfig, Cli, Command, CompatConfig, CompatHeaders,
    PackConfig, SeedConfig, ServiceConnectionConfig,
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
                nrese: ServiceConnectionConfig {
                    base_url: required_opt(&options, "--nrese-base-url")?,
                    headers: CompatHeaders::new(),
                    timeout_ms: None,
                    basic_auth: None,
                },
                fuseki: optional_fuseki_connection(&options)?,
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
                nrese: ServiceConnectionConfig {
                    base_url: required_opt(&options, "--nrese-base-url")?,
                    headers: CompatHeaders::new(),
                    timeout_ms: None,
                    basic_auth: None,
                },
                fuseki: ServiceConnectionConfig {
                    base_url: required_opt(&options, "--fuseki-base-url")?,
                    headers: CompatHeaders::new(),
                    timeout_ms: None,
                    basic_auth: parse_basic_auth_opt(&options, "--fuseki-basic-auth")?,
                },
                nrese_profiles: BTreeMap::new(),
                fuseki_profiles: BTreeMap::new(),
                cases_path: options
                    .get("--cases")
                    .map(PathBuf::from)
                    .unwrap_or_else(|| PathBuf::from(DEFAULT_COMPAT_CASES_PATH)),
                report_json_path: options.get("--report-json").map(PathBuf::from),
            }),
        }),
        "pack" => Ok(Cli {
            command: Command::Pack(PackConfig {
                nrese_base_url: required_opt(&options, "--nrese-base-url")?,
                fuseki_base_url: options.get("--fuseki-base-url").cloned(),
                fuseki_basic_auth: parse_basic_auth_opt(&options, "--fuseki-basic-auth")?,
                workload_pack_path: options
                    .get("--workload-pack")
                    .map(PathBuf::from)
                    .ok_or_else(|| anyhow!("missing required option --workload-pack"))?,
                iterations: options
                    .get("--iterations")
                    .map(|value| value.parse::<usize>())
                    .transpose()?
                    .unwrap_or(20),
                report_dir: options.get("--report-dir").map(PathBuf::from),
            }),
        }),
        "seed" => Ok(Cli {
            command: Command::Seed(SeedConfig {
                nrese: ServiceConnectionConfig {
                    base_url: required_opt(&options, "--nrese-base-url")?,
                    headers: CompatHeaders::new(),
                    timeout_ms: None,
                    basic_auth: None,
                },
                fuseki: optional_fuseki_connection(&options)?,
                dataset_path: options
                    .get("--dataset")
                    .map(PathBuf::from)
                    .unwrap_or_else(|| PathBuf::from(DEFAULT_SEED_DATASET_PATH)),
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

fn optional_fuseki_connection(
    options: &BTreeMap<String, String>,
) -> Result<Option<ServiceConnectionConfig>> {
    let Some(base_url) = options.get("--fuseki-base-url").cloned() else {
        return Ok(None);
    };

    Ok(Some(ServiceConnectionConfig {
        base_url,
        headers: CompatHeaders::new(),
        timeout_ms: None,
        basic_auth: parse_basic_auth_opt(options, "--fuseki-basic-auth")?,
    }))
}

pub fn print_usage() {
    println!(
        "nrese-bench-harness

USAGE:
  cargo run --manifest-path benches/nrese-bench-harness/Cargo.toml -- bench --nrese-base-url <URL> [--fuseki-base-url <URL>] [--fuseki-basic-auth <user:pass>] [--iterations <N>] [--query-workload <PATH>] [--update-workload <PATH>] [--report-json <PATH>]
  cargo run --manifest-path benches/nrese-bench-harness/Cargo.toml -- catalog-sync [--catalog <PATH>] [--output-dir <DIR>] [--tier <small|medium|broad>] [--refresh <true|false>]
  cargo run --manifest-path benches/nrese-bench-harness/Cargo.toml -- compat --nrese-base-url <URL> --fuseki-base-url <URL> [--fuseki-basic-auth <user:pass>] [--cases <PATH>] [--report-json <PATH>]
  cargo run --manifest-path benches/nrese-bench-harness/Cargo.toml -- pack --nrese-base-url <URL> [--fuseki-base-url <URL>] [--fuseki-basic-auth <user:pass>] --workload-pack <PATH> [--iterations <N>] [--report-dir <DIR>]
  cargo run --manifest-path benches/nrese-bench-harness/Cargo.toml -- seed --nrese-base-url <URL> [--fuseki-base-url <URL>] [--fuseki-basic-auth <user:pass>] [--dataset <PATH>] [--content-type <TYPE>] [--replace <true|false>]
"
    );
}

#[cfg(test)]
mod tests {
    use crate::model::Command;

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
            "--nrese-base-url".to_owned(),
            "http://127.0.0.1:8080".to_owned(),
            "--workload-pack".to_owned(),
            "benches/nrese-bench-harness/fixtures/packs/generic-baseline/pack.toml".to_owned(),
            "--iterations".to_owned(),
            "5".to_owned(),
        ])
        .expect("cli");

        match cli.command {
            Command::Pack(config) => {
                assert_eq!(config.iterations, 5);
                assert!(config.report_dir.is_none());
            }
            _ => panic!("expected pack command"),
        }
    }

    #[test]
    fn parses_compat_defaults() {
        let cli = parse_cli(vec![
            "bench".to_owned(),
            "compat".to_owned(),
            "--nrese-base-url".to_owned(),
            "http://127.0.0.1:8080".to_owned(),
            "--fuseki-base-url".to_owned(),
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
    fn parses_optional_fuseki_basic_auth() {
        let cli = parse_cli(vec![
            "bench".to_owned(),
            "compat".to_owned(),
            "--nrese-base-url".to_owned(),
            "http://127.0.0.1:8080".to_owned(),
            "--fuseki-base-url".to_owned(),
            "http://127.0.0.1:3030/ds".to_owned(),
            "--fuseki-basic-auth".to_owned(),
            "admin:nrese-admin".to_owned(),
        ])
        .expect("cli");

        match cli.command {
            Command::Compat(config) => {
                let auth = config.fuseki.basic_auth.expect("basic auth");
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
