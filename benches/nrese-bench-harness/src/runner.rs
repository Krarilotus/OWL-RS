use std::collections::BTreeMap;
use std::time::{Duration, Instant};
use std::{fs, path::Path};

use anyhow::{Context, Result, anyhow, bail};
use reqwest::Client;

use crate::compat_common::{
    GraphWriteRequest, RequestExecutionOptions, execute_graph_write_raw, execute_query_raw,
    execute_update_raw, require_success_http,
};
use crate::compat_graph_store;
use crate::compat_query;
use crate::compat_update;
use crate::io::{
    infer_rdf_content_type, read_dataset_payload, read_json, read_ontology_catalog,
    read_workload_pack, write_json_report,
};
use crate::layout::ServiceTarget;
use crate::model::{
    BenchComparison, BenchConfig, BenchReport, CatalogSyncConfig, CompatCase, CompatCaseReport,
    CompatConfig, CompatGraphTarget, CompatHeaders, CompatOperation, CompatReport, OntologyFixture,
    PackArtifactReport, PackCompatSuiteReport, PackConfig, PackReport, QueryWorkloadCase,
    SeedConfig, ServiceBenchReport, ServiceConnectionConfig, ServiceRequestProfile,
    UpdateWorkloadCase,
};
use crate::normalize::summarize;

pub async fn run_bench(config: BenchConfig) -> Result<BenchRunArtifact> {
    let client = build_client()?;
    let query_cases: Vec<QueryWorkloadCase> = read_json(config.query_workload_path)?;
    let update_cases: Vec<UpdateWorkloadCase> = read_json(config.update_workload_path)?;
    let nrese = ServiceTarget::nrese(config.nrese.clone());

    println!("== Benchmark run ==");
    println!("nrese base: {}", nrese.base_url);
    println!("iterations per weighted case: {}", config.iterations);

    let mut services = Vec::new();
    let nrese_query = benchmark_queries(&client, &nrese, &query_cases, config.iterations).await?;
    let nrese_update = benchmark_updates(&client, &nrese, &update_cases, config.iterations).await?;
    print_summary("NRESE query", &nrese_query);
    print_summary("NRESE update", &nrese_update);
    services.push(ServiceBenchReport {
        label: nrese.label,
        base_url: nrese.base_url.clone(),
        query: nrese_query.clone(),
        update: nrese_update.clone(),
    });

    let mut comparison = None;
    if let Some(fuseki_config) = &config.fuseki {
        let fuseki = ServiceTarget::fuseki(fuseki_config.clone());
        println!("fuseki base: {}", fuseki.base_url);
        let fuseki_query =
            benchmark_queries(&client, &fuseki, &query_cases, config.iterations).await?;
        let fuseki_update =
            benchmark_updates(&client, &fuseki, &update_cases, config.iterations).await?;
        print_summary("Fuseki query", &fuseki_query);
        print_summary("Fuseki update", &fuseki_update);
        print_delta(
            "query p95 delta (NRESE - Fuseki)",
            nrese_query.p95_ms,
            fuseki_query.p95_ms,
        );
        print_delta(
            "update p95 delta (NRESE - Fuseki)",
            nrese_update.p95_ms,
            fuseki_update.p95_ms,
        );
        services.push(ServiceBenchReport {
            label: fuseki.label,
            base_url: fuseki.base_url.clone(),
            query: fuseki_query.clone(),
            update: fuseki_update.clone(),
        });
        comparison = Some(BenchComparison {
            left_label: nrese.label,
            right_label: fuseki.label,
            query_p95_delta_ms: nrese_query.p95_ms as i128 - fuseki_query.p95_ms as i128,
            update_p95_delta_ms: nrese_update.p95_ms as i128 - fuseki_update.p95_ms as i128,
        });
    }

    let report_json_path = config.report_json_path;
    if let Some(report_path) = &report_json_path {
        write_json_report(
            report_path.clone(),
            &BenchReport {
                mode: "bench",
                iterations: config.iterations,
                services,
                comparison,
            },
        )?;
    }

    Ok(BenchRunArtifact { report_json_path })
}

pub async fn run_compat(config: CompatConfig) -> Result<CompatRunArtifact> {
    let client = build_client()?;
    let cases_path = config.cases_path.clone();
    let cases: Vec<CompatCase> = read_json(cases_path.clone())?;
    let mut reports = Vec::with_capacity(cases.len());
    let mut matched_cases = 0usize;

    println!("== Compatibility run ==");
    println!("nrese base: {}", config.nrese.base_url);
    println!("fuseki base: {}", config.fuseki.base_url);

    for case in &cases {
        let nrese = ServiceTarget::nrese(resolve_service_connection(
            &config.nrese,
            resolve_service_profile(
                &config.nrese_profiles,
                case.nrese_profile.as_deref(),
                "NRESE",
                &case.name,
            )?,
        ));
        let fuseki = ServiceTarget::fuseki(resolve_service_connection(
            &config.fuseki,
            resolve_service_profile(
                &config.fuseki_profiles,
                case.fuseki_profile.as_deref(),
                "Fuseki",
                &case.name,
            )?,
        ));
        let report = execute_compat_case(&client, &nrese, &fuseki, case).await?;
        if report.matched {
            matched_cases += 1;
        }

        println!(
            "[{}:{}] {} => {}",
            report.operation,
            report.kind,
            case.name,
            if report.matched { "match" } else { "mismatch" }
        );

        reports.push(report);
    }

    let mismatched_cases = cases.len() - matched_cases;
    let status = if mismatched_cases == 0 {
        "all-matched"
    } else {
        "mismatches-present"
    };

    let report_json_path = config.report_json_path;
    if let Some(report_path) = &report_json_path {
        write_json_report(
            report_path.clone(),
            &CompatReport {
                mode: "compat",
                nrese_base_url: config.nrese.base_url.clone(),
                fuseki_base_url: config.fuseki.base_url.clone(),
                total_cases: cases.len(),
                matched_cases,
                mismatched_cases,
                status,
                cases: reports,
            },
        )?;
    }

    if mismatched_cases > 0 {
        bail!("compatibility mismatches detected");
    }

    Ok(CompatRunArtifact {
        cases_path,
        report_json_path,
        total_cases: cases.len(),
        matched_cases,
        mismatched_cases,
        status,
    })
}

pub async fn run_seed(config: SeedConfig) -> Result<()> {
    let client = build_client()?;
    let payload = read_dataset_payload(&config.dataset_path)?;
    let content_type = config.content_type.clone().unwrap_or_else(|| {
        infer_rdf_content_type(&config.dataset_path)
            .unwrap_or("text/turtle")
            .to_owned()
    });
    let mut targets = vec![ServiceTarget::nrese(config.nrese.clone())];
    if let Some(fuseki_config) = config.fuseki.clone() {
        targets.push(ServiceTarget::fuseki(fuseki_config));
    }

    println!("== Dataset seed run ==");
    println!("dataset: {}", config.dataset_path.display());
    println!("content-type: {}", content_type);
    println!("replace mode: {}", config.replace);

    for target in targets {
        write_dataset_raw(&client, &target, &payload, &content_type, config.replace).await?;
        println!("seeded {} via {}", target.label, target.data_endpoint());
    }

    Ok(())
}

pub async fn run_pack(config: PackConfig) -> Result<()> {
    let pack = read_workload_pack(&config.workload_pack_path)?;
    ensure_report_dir(config.report_dir.as_deref())?;
    let nrese_base_url = config.nrese_base_url.clone();
    let fuseki_base_url = config.fuseki_base_url.clone();
    let fuseki_basic_auth = config.fuseki_basic_auth.clone();
    let iterations = config.iterations;
    let manifest_path = config.workload_pack_path.display().to_string();

    println!("== Workload pack run ==");
    println!("pack: {}", pack.name);
    println!("manifest: {manifest_path}");

    run_seed(SeedConfig {
        nrese: ServiceConnectionConfig {
            base_url: nrese_base_url.clone(),
            headers: pack.nrese.headers.clone(),
            timeout_ms: pack.nrese.timeout_ms,
            basic_auth: None,
        },
        fuseki: fuseki_base_url.clone().map(|base_url| ServiceConnectionConfig {
            base_url,
            headers: pack.fuseki.headers.clone(),
            timeout_ms: pack.fuseki.timeout_ms,
            basic_auth: fuseki_basic_auth.clone(),
        }),
        dataset_path: pack.dataset.clone(),
        content_type: None,
        replace: true,
    })
    .await?;

    let bench_report_path = config
        .report_dir
        .as_ref()
        .map(|dir| dir.join("bench-report.json"));
    let mut compat_reports = Vec::new();
    if let Some(fuseki_base_url) = fuseki_base_url.clone() {
        for suite_path in &pack.compat_suites {
            let compat_report_path = config
                .report_dir
                .as_ref()
                .map(|dir| dir.join(compat_report_filename(suite_path)));
            let compat_run = run_compat(CompatConfig {
                nrese: ServiceConnectionConfig {
                    base_url: nrese_base_url.clone(),
                    headers: pack.nrese.headers.clone(),
                    timeout_ms: pack.nrese.timeout_ms,
                    basic_auth: None,
                },
                fuseki: ServiceConnectionConfig {
                    base_url: fuseki_base_url.clone(),
                    headers: pack.fuseki.headers.clone(),
                    timeout_ms: pack.fuseki.timeout_ms,
                    basic_auth: fuseki_basic_auth.clone(),
                },
                nrese_profiles: pack.invocation_profiles.nrese.clone(),
                fuseki_profiles: pack.invocation_profiles.fuseki.clone(),
                cases_path: suite_path.clone(),
                report_json_path: compat_report_path,
            })
            .await?;
            compat_reports.push(pack_compat_suite_report(compat_run));
        }
    } else {
        println!("fuseki base not provided; skipping compat stage");
    }

    let bench_run = run_bench(BenchConfig {
        nrese: ServiceConnectionConfig {
            base_url: nrese_base_url.clone(),
            headers: pack.nrese.headers,
            timeout_ms: pack.nrese.timeout_ms,
            basic_auth: None,
        },
        fuseki: fuseki_base_url.clone().map(|base_url| ServiceConnectionConfig {
            base_url,
            headers: pack.fuseki.headers,
            timeout_ms: pack.fuseki.timeout_ms,
            basic_auth: fuseki_basic_auth,
        }),
        iterations,
        query_workload_path: pack.query_workload,
        update_workload_path: pack.update_workload,
        report_json_path: bench_report_path,
    })
    .await?;

    if let Some(report_dir) = config.report_dir {
        write_json_report(
            report_dir.join("pack-report.json"),
            &PackReport {
                mode: "pack",
                pack_name: pack.name,
                manifest_path,
                dataset_path: pack.dataset.display().to_string(),
                nrese_base_url,
                fuseki_base_url,
                iterations,
                compat_suites: compat_reports,
                bench_report: bench_run.report_json_path.map(|path| PackArtifactReport {
                    path: path.display().to_string(),
                }),
            },
        )?;
    }

    Ok(())
}

pub async fn run_catalog_sync(config: CatalogSyncConfig) -> Result<()> {
    let catalog = read_ontology_catalog(&config.catalog_path)?;
    fs::create_dir_all(&config.output_dir)
        .with_context(|| format!("failed to create {}", config.output_dir.display()))?;
    let client = build_client()?;

    println!("== Ontology catalog sync ==");
    println!("catalog: {}", config.catalog_path.display());
    println!("output: {}", config.output_dir.display());

    for ontology in catalog
        .ontologies
        .iter()
        .filter(|item| config.tier.as_deref().is_none_or(|tier| item.tier == tier))
    {
        sync_ontology(&client, ontology, &config.output_dir, config.refresh).await?;
    }

    Ok(())
}

fn ensure_report_dir(report_dir: Option<&Path>) -> Result<()> {
    if let Some(report_dir) = report_dir {
        fs::create_dir_all(report_dir)
            .with_context(|| format!("failed to create report dir {}", report_dir.display()))?;
    }
    Ok(())
}

#[derive(Debug)]
pub struct CompatRunArtifact {
    cases_path: std::path::PathBuf,
    report_json_path: Option<std::path::PathBuf>,
    total_cases: usize,
    matched_cases: usize,
    mismatched_cases: usize,
    status: &'static str,
}

#[derive(Debug)]
pub struct BenchRunArtifact {
    report_json_path: Option<std::path::PathBuf>,
}

async fn sync_ontology(
    client: &Client,
    ontology: &OntologyFixture,
    output_dir: &Path,
    refresh: bool,
) -> Result<()> {
    let output_path = output_dir.join(&ontology.filename);
    if output_path.exists() && !refresh {
        println!(
            "cached {} ({}) [{}] at {}",
            ontology.name,
            ontology.title,
            ontology.tier,
            output_path.display()
        );
        return Ok(());
    }

    println!(
        "download {} ({}) [{}] from {}",
        ontology.name, ontology.title, ontology.tier, ontology.url
    );
    let response = client
        .get(&ontology.url)
        .send()
        .await
        .with_context(|| format!("failed to fetch {}", ontology.url))?;
    let status = response.status();
    if !status.is_success() {
        bail!("failed to fetch {}: HTTP {}", ontology.url, status);
    }
    let bytes = response
        .bytes()
        .await
        .with_context(|| format!("failed to read body {}", ontology.url))?;
    fs::write(&output_path, &bytes)
        .with_context(|| format!("failed to write {}", output_path.display()))?;
    println!(
        "saved {} bytes to {} ({})",
        bytes.len(),
        output_path.display(),
        ontology.media_type
    );
    if !ontology.focus_terms.is_empty() {
        println!("focus terms: {}", ontology.focus_terms.join(", "));
    }
    Ok(())
}

async fn benchmark_queries(
    client: &Client,
    target: &ServiceTarget,
    cases: &[QueryWorkloadCase],
    iterations: usize,
) -> Result<crate::model::LatencySummary> {
    let mut latencies = Vec::new();
    let mut success = 0usize;
    let mut failure = 0usize;

    for case in cases {
        for _ in 0..(case.weight * iterations) {
            let started = Instant::now();
            match execute_query_raw(
                client,
                target,
                &case.query,
                &case.accept,
                &CompatHeaders::new(),
                RequestExecutionOptions::default(),
            )
            .await
            {
                Ok(outcome) => match require_success_http(target, "query", &outcome) {
                    Ok(_) => {
                        success += 1;
                        latencies.push(started.elapsed().as_millis());
                    }
                    Err(error) => {
                        failure += 1;
                        eprintln!(
                            "query case '{}' failed against {}: {error}",
                            case.name, target.label
                        );
                    }
                },
                Err(error) => {
                    failure += 1;
                    eprintln!(
                        "query case '{}' failed against {}: {error}",
                        case.name, target.label
                    );
                }
            }
        }
    }

    Ok(summarize(latencies, success, failure))
}

async fn benchmark_updates(
    client: &Client,
    target: &ServiceTarget,
    cases: &[UpdateWorkloadCase],
    iterations: usize,
) -> Result<crate::model::LatencySummary> {
    let mut latencies = Vec::new();
    let mut success = 0usize;
    let mut failure = 0usize;

    for case in cases {
        for _ in 0..(case.weight * iterations) {
            let started = Instant::now();
            match execute_update_raw(
                client,
                target,
                &case.update,
                &CompatHeaders::new(),
                RequestExecutionOptions::default(),
            )
            .await
            {
                Ok(outcome) => match require_success_http(target, "update", &outcome) {
                    Ok(_) => {
                        success += 1;
                        latencies.push(started.elapsed().as_millis());
                    }
                    Err(error) => {
                        failure += 1;
                        eprintln!(
                            "update case '{}' failed against {}: {error}",
                            case.name, target.label
                        );
                    }
                },
                Err(error) => {
                    failure += 1;
                    eprintln!(
                        "update case '{}' failed against {}: {error}",
                        case.name, target.label
                    );
                }
            }
        }
    }

    Ok(summarize(latencies, success, failure))
}

async fn execute_compat_case(
    client: &Client,
    left: &ServiceTarget,
    right: &ServiceTarget,
    case: &CompatCase,
) -> Result<CompatCaseReport> {
    match case.operation {
        CompatOperation::Query => compat_query::execute_case(client, left, right, case).await,
        CompatOperation::GraphRead
        | CompatOperation::GraphHead
        | CompatOperation::GraphDeleteEffect
        | CompatOperation::GraphPutEffect
        | CompatOperation::GraphPostEffect => {
            compat_graph_store::execute_case(client, left, right, case).await
        }
        CompatOperation::UpdateEffect => {
            compat_update::execute_case(client, left, right, case).await
        }
    }
}

async fn write_dataset_raw(
    client: &Client,
    target: &ServiceTarget,
    payload: &[u8],
    content_type: &str,
    replace: bool,
) -> Result<()> {
    let outcome = execute_graph_write_raw(
        client,
        target,
        GraphWriteRequest {
            graph_target: &CompatGraphTarget::DefaultGraph,
            content_type,
            payload,
            replace,
            extra_headers: &CompatHeaders::new(),
            options: RequestExecutionOptions::default(),
        },
    )
    .await?;
    require_success_http(target, "dataset write", &outcome).map(|_| ())
}

fn build_client() -> Result<Client> {
    Client::builder()
        .timeout(Duration::from_secs(120))
        .build()
        .context("failed to build HTTP client")
}

fn print_summary(label: &str, summary: &crate::model::LatencySummary) {
    println!(
        "{}: samples={} success={} failure={} min={}ms p50={}ms p95={}ms p99={}ms max={}ms total={}ms",
        label,
        summary.samples,
        summary.success,
        summary.failure,
        summary.min_ms,
        summary.p50_ms,
        summary.p95_ms,
        summary.p99_ms,
        summary.max_ms,
        summary.total_ms,
    );
}

fn print_delta(label: &str, left_ms: u128, right_ms: u128) {
    println!("{label}: {}ms", left_ms as i128 - right_ms as i128);
}

fn compat_report_filename(suite_path: &Path) -> String {
    let stem = suite_path
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or("compat");
    format!("{stem}-report.json")
}

fn pack_compat_suite_report(run: CompatRunArtifact) -> PackCompatSuiteReport {
    let suite_name = run
        .cases_path
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or("compat")
        .to_owned();

    PackCompatSuiteReport {
        suite_name,
        suite_path: run.cases_path.display().to_string(),
        report: run.report_json_path.map(|path| PackArtifactReport {
            path: path.display().to_string(),
        }),
        total_cases: run.total_cases,
        matched_cases: run.matched_cases,
        mismatched_cases: run.mismatched_cases,
        status: run.status,
    }
}

fn resolve_service_profile<'a>(
    profiles: &'a BTreeMap<String, ServiceRequestProfile>,
    profile_name: Option<&str>,
    target_label: &str,
    case_name: &str,
) -> Result<Option<&'a ServiceRequestProfile>> {
    let Some(profile_name) = profile_name else {
        return Ok(None);
    };

    profiles.get(profile_name).map(Some).ok_or_else(|| {
        anyhow!(
            "{target_label} compat profile '{profile_name}' referenced by case '{case_name}' is not defined in the workload pack"
        )
    })
}

fn resolve_service_connection(
    base: &ServiceConnectionConfig,
    profile: Option<&ServiceRequestProfile>,
) -> ServiceConnectionConfig {
    let mut headers = base.headers.clone();
    let timeout_ms = profile
        .and_then(|profile| profile.timeout_ms)
        .or(base.timeout_ms);

    if let Some(profile) = profile {
        headers.extend(profile.headers.clone());
    }

    ServiceConnectionConfig {
        base_url: base.base_url.clone(),
        headers,
        timeout_ms,
        basic_auth: base.basic_auth.clone(),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::path::Path;

    use crate::model::{CompatCase, CompatHeaders, CompatOperation, ServiceConnectionConfig, ServiceRequestProfile};

    use super::{
        CompatRunArtifact, compat_report_filename, pack_compat_suite_report,
        resolve_service_connection, resolve_service_profile,
    };

    #[test]
    fn compat_case_defaults_to_query_operation() {
        let case: CompatCase = serde_json::from_str(
            r#"{
                "name":"ask",
                "query":"ASK WHERE { ?s ?p ?o }",
                "kind":"ask-boolean"
            }"#,
        )
        .expect("case");

        assert!(matches!(case.operation, CompatOperation::Query));
    }

    #[test]
    fn compat_case_parses_optional_timeout_budget() {
        let case: CompatCase = serde_json::from_str(
            r#"{
                "name":"slow-query",
                "query":"SELECT * WHERE { ?s ?p ?o }",
                "timeout_ms": 25,
                "kind":"status-content-type-body-class"
            }"#,
        )
        .expect("case");

        assert_eq!(case.timeout_ms, Some(25));
    }

    #[test]
    fn compat_case_parses_per_side_profile_refs() {
        let case: CompatCase = serde_json::from_str(
            r#"{
                "name":"secured-query",
                "query":"SELECT * WHERE { ?s ?p ?o }",
                "nrese_profile":"read",
                "fuseki_profile":"read",
                "kind":"solutions-count"
            }"#,
        )
        .expect("case");

        assert_eq!(case.nrese_profile.as_deref(), Some("read"));
        assert_eq!(case.fuseki_profile.as_deref(), Some("read"));
    }

    #[test]
    fn report_filename_uses_suite_stem() {
        assert_eq!(
            compat_report_filename(Path::new("fixtures/compat/policy_failure_cases.json")),
            "policy_failure_cases-report.json"
        );
    }

    #[test]
    fn pack_compat_suite_report_uses_suite_stem_and_status() {
        let report = pack_compat_suite_report(CompatRunArtifact {
            cases_path: "fixtures/compat/policy_failure_cases.json".into(),
            report_json_path: Some("artifacts/policy-report.json".into()),
            total_cases: 3,
            matched_cases: 3,
            mismatched_cases: 0,
            status: "all-matched",
        });

        assert_eq!(report.suite_name, "policy_failure_cases");
        assert_eq!(report.status, "all-matched");
        assert_eq!(
            report.report.as_ref().map(|entry| entry.path.as_str()),
            Some("artifacts/policy-report.json")
        );
    }

    #[test]
    fn resolve_service_profile_reports_unknown_profile() {
        let error = resolve_service_profile(
            &BTreeMap::new(),
            Some("missing"),
            "NRESE",
            "secured-query",
        )
        .expect_err("missing profile");

        assert!(error.to_string().contains("compat profile 'missing'"));
    }

    #[test]
    fn resolve_service_connection_merges_profile_over_base() {
        let base = ServiceConnectionConfig {
            base_url: "http://example.invalid".to_owned(),
            headers: CompatHeaders::from([
                ("authorization".to_owned(), "Bearer base".to_owned()),
                ("x-base".to_owned(), "1".to_owned()),
            ]),
            timeout_ms: Some(1000),
            basic_auth: None,
        };
        let profile = ServiceRequestProfile {
            headers: CompatHeaders::from([
                ("authorization".to_owned(), "Bearer profile".to_owned()),
                ("x-profile".to_owned(), "1".to_owned()),
            ]),
            timeout_ms: Some(25),
        };

        let merged = resolve_service_connection(&base, Some(&profile));

        assert_eq!(merged.timeout_ms, Some(25));
        assert_eq!(
            merged.headers.get("authorization").map(String::as_str),
            Some("Bearer profile")
        );
        assert_eq!(merged.headers.get("x-base").map(String::as_str), Some("1"));
        assert_eq!(
            merged.headers.get("x-profile").map(String::as_str),
            Some("1")
        );
    }
}
