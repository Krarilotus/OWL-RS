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
use crate::connection_profile::{
    merge_invocation_profiles, read_connection_profiles_registry, resolve_live_connection_profile,
    resolve_optional_service_connection, resolve_required_service_connection,
};
use crate::io::{
    infer_rdf_content_type, read_dataset_payload, read_json, read_ontology_catalog,
    read_workload_pack, write_json_report,
};
use crate::layout::ServiceTarget;
use crate::model::{
    BenchComparison, BenchConfig, BenchReport, CatalogSyncConfig, CompatCase, CompatCaseReport,
    CompatConfig, CompatGraphTarget, CompatHeaders, CompatOperation, CompatReport, OntologyFixture,
    OntologyReasoningFeature, OntologySemanticDialect, OntologySerialization,
    OntologyServiceSurface, PackArtifactReport, PackCompatSuiteReport, PackConfig,
    PackExecutionMode,
    PackMatrixConfig, PackMatrixEntryReport, PackMatrixReport, PackReport,
    PackValidationReport, QueryWorkloadCase, SeedConfig, ServiceBenchReport,
    ServiceConnectionConfig, ServiceRequestProfile, UpdateWorkloadCase, ValidatePackConfig,
    WorkloadPackManifest,
};
use crate::normalize::summarize;
use crate::pack_validation::{
    validate_catalog_baseline_pack, validate_pack_invocation_profiles,
};

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
    if let Some(base_iri) = &config.dataset_base_iri {
        println!("dataset base IRI: {base_iri}");
    }
    println!("content-type: {}", content_type);
    println!("replace mode: {}", config.replace);

    for target in targets {
        write_dataset_raw(
            &client,
            &target,
            &payload,
            &content_type,
            config.dataset_base_iri.as_deref(),
            config.replace,
        )
        .await?;
        println!("seeded {} via {}", target.label, target.data_endpoint());
    }

    Ok(())
}

pub async fn run_pack(config: PackConfig) -> Result<()> {
    ensure_report_dir(config.report_dir.as_deref())?;
    let prepared = prepare_pack_run(
        &config.workload_pack_path,
        config.connection_profiles_path.as_deref(),
        config.connection_profile_name.as_deref(),
        config.nrese_base_url.as_deref(),
        config.fuseki_base_url.as_deref(),
        config.fuseki_basic_auth.as_ref(),
    )?;
    run_loaded_pack(&config, prepared).await
}

pub async fn run_validate_pack(config: ValidatePackConfig) -> Result<()> {
    let prepared = prepare_pack_run(
        &config.workload_pack_path,
        config.connection_profiles_path.as_deref(),
        config.connection_profile_name.as_deref(),
        config.nrese_base_url.as_deref(),
        config.fuseki_base_url.as_deref(),
        config.fuseki_basic_auth.as_ref(),
    )?;
    print_pack_header("Workload pack validation", &prepared);

    if let Some(report_json_path) = &config.report_json_path {
        write_json_report(
            report_json_path.clone(),
            &PackValidationReport {
                mode: "pack-validate",
                pack_name: prepared.pack.name.clone(),
                manifest_path: prepared.manifest_path.clone(),
                connection_profiles_path: prepared.connection_profiles_path.clone(),
                connection_profile_name: prepared.connection_profile_name.clone(),
                dataset_path: prepared.pack.dataset.display().to_string(),
                dataset_base_iri: prepared.pack.dataset_base_iri.clone(),
                nrese_base_url: prepared.nrese_connection.base_url.clone(),
                fuseki_base_url: prepared
                    .fuseki_connection
                    .as_ref()
                    .map(|connection| connection.base_url.clone()),
                compat_suites: prepared
                    .pack
                    .compat_suites
                    .iter()
                    .map(|path| path.display().to_string())
                    .collect(),
                nrese_invocation_profiles: prepared
                    .merged_invocation_profiles
                    .nrese
                    .keys()
                    .cloned()
                    .collect(),
                fuseki_invocation_profiles: prepared
                    .merged_invocation_profiles
                    .fuseki
                    .keys()
                    .cloned()
                    .collect(),
            },
        )?;
    }

    Ok(())
}

async fn run_loaded_pack(config: &PackConfig, prepared: PreparedPackRun) -> Result<()> {
    print_pack_header("Workload pack run", &prepared);
    println!("execution mode: {:?}", config.execution_mode);

    let pack = prepared.pack;
    let nrese_connection = prepared.nrese_connection;
    let fuseki_connection = prepared.fuseki_connection;
    let merged_invocation_profiles = prepared.merged_invocation_profiles;
    let iterations = config.iterations;
    let manifest_path = prepared.manifest_path;
    validate_pack_execution_mode(config.execution_mode, fuseki_connection.is_some())?;

    let mut compat_reports = Vec::new();
    let mut bench_report = None;
    let run_result: Result<()> = match config.execution_mode {
        PackExecutionMode::Full => {
            run_seed(SeedConfig {
                nrese: merge_pack_service_profile(&nrese_connection, &pack.nrese),
                fuseki: fuseki_connection
                    .as_ref()
                    .map(|connection| merge_pack_service_profile(connection, &pack.fuseki)),
                dataset_path: pack.dataset.clone(),
                dataset_base_iri: pack.dataset_base_iri.clone(),
                content_type: None,
                replace: true,
            })
            .await?;

            if let Some(fuseki_connection) = fuseki_connection.as_ref() {
                for suite_path in &pack.compat_suites {
                    let compat_report_path = config
                        .report_dir
                        .as_ref()
                        .map(|dir| dir.join(compat_report_filename(suite_path)));
                    let compat_run = run_compat(CompatConfig {
                        nrese: merge_pack_service_profile(&nrese_connection, &pack.nrese),
                        fuseki: merge_pack_service_profile(fuseki_connection, &pack.fuseki),
                        nrese_profiles: merged_invocation_profiles.nrese.clone(),
                        fuseki_profiles: merged_invocation_profiles.fuseki.clone(),
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
                nrese: merge_pack_service_profile(&nrese_connection, &pack.nrese),
                fuseki: fuseki_connection
                    .as_ref()
                    .map(|connection| merge_pack_service_profile(connection, &pack.fuseki)),
                iterations,
                query_workload_path: pack.query_workload,
                update_workload_path: pack.update_workload,
                report_json_path: config
                    .report_dir
                    .as_ref()
                    .map(|dir| dir.join("bench-report.json")),
            })
            .await?;

            bench_report = bench_run.report_json_path.map(|path| PackArtifactReport {
                path: path.display().to_string(),
            });
            Ok(())
        }
        PackExecutionMode::CompatOnly => {
            let fuseki_connection = fuseki_connection
                .as_ref()
                .expect("compat-only mode requires a Fuseki connection");
            for suite_path in &pack.compat_suites {
                let compat_report_path = config
                    .report_dir
                    .as_ref()
                    .map(|dir| dir.join(compat_report_filename(suite_path)));
                let compat_run = run_compat(CompatConfig {
                    nrese: merge_pack_service_profile(&nrese_connection, &pack.nrese),
                    fuseki: merge_pack_service_profile(fuseki_connection, &pack.fuseki),
                    nrese_profiles: merged_invocation_profiles.nrese.clone(),
                    fuseki_profiles: merged_invocation_profiles.fuseki.clone(),
                    cases_path: suite_path.clone(),
                    report_json_path: compat_report_path,
                })
                .await?;
                compat_reports.push(pack_compat_suite_report(compat_run));
            }
            Ok(())
        }
    };
    let status = if run_result.is_ok() {
        "completed"
    } else {
        "failed"
    };
    let error = run_result.as_ref().err().map(ToString::to_string);

    if let Some(report_dir) = config.report_dir.as_ref() {
        write_json_report(
            report_dir.join("pack-report.json"),
            &PackReport {
                mode: "pack",
                pack_name: pack.name,
                manifest_path,
                connection_profiles_path: prepared.connection_profiles_path,
                connection_profile_name: prepared.connection_profile_name,
                execution_mode: config.execution_mode,
                dataset_path: pack.dataset.display().to_string(),
                dataset_base_iri: pack.dataset_base_iri.clone(),
                nrese_base_url: nrese_connection.base_url,
                fuseki_base_url: fuseki_connection.map(|connection| connection.base_url),
                iterations,
                status,
                error,
                compat_suites: compat_reports,
                bench_report,
            },
        )?;
    }

    run_result
}

pub async fn run_pack_matrix(config: PackMatrixConfig) -> Result<()> {
    fs::create_dir_all(&config.report_dir)
        .with_context(|| format!("failed to create report dir {}", config.report_dir.display()))?;
    let catalog = read_ontology_catalog(&config.catalog_path)?;
    let live_connections = resolve_live_connections(
        config.connection_profiles_path.as_deref(),
        config.connection_profile_name.as_deref(),
        config.nrese_base_url.as_deref(),
        config.fuseki_base_url.as_deref(),
        config.fuseki_basic_auth.as_ref(),
    )?;
    let mut entries = Vec::new();
    let mut failed = false;

    println!("== Ontology pack matrix run ==");
    println!("catalog: {}", config.catalog_path.display());
    println!("packs dir: {}", config.packs_dir.display());
    println!("nrese base: {}", live_connections.nrese.base_url);
    if let Some(fuseki) = &live_connections.fuseki {
        println!("fuseki base: {}", fuseki.base_url);
    }
    if let Some(connection_profiles_path) = &config.connection_profiles_path {
        println!("connection profiles: {}", connection_profiles_path.display());
    }
    if let Some(connection_profile_name) = &config.connection_profile_name {
        println!("connection profile: {connection_profile_name}");
    }
    if let Some(ontology_name) = &config.ontology_name {
        println!("ontology filter: {ontology_name}");
    }
    println!("execution mode: {:?}", config.execution_mode);
    if let Some(tier) = &config.tier {
        println!("tier filter: {tier}");
    }
    if let Some(dialect) = config.semantic_dialect {
        println!("semantic-dialect filter: {:?}", dialect);
    }
    if let Some(feature) = config.reasoning_feature {
        println!("reasoning-feature filter: {:?}", feature);
    }
    if let Some(surface) = config.service_coverage {
        println!("service-coverage filter: {:?}", surface);
    }

    for ontology in catalog
        .ontologies
        .iter()
        .filter(|fixture| pack_matrix_matches_filters(fixture, &config))
    {
        let manifest_path = baseline_pack_manifest_path(&config.packs_dir, ontology);
        let pack_report_path = config.report_dir.join(&ontology.name).join("pack-report.json");
        let pack_report = PackArtifactReport {
            path: pack_report_path.display().to_string(),
        };

        if !manifest_path.is_file() {
            failed = true;
            entries.push(pack_matrix_entry_missing_manifest(ontology, &manifest_path));
            continue;
        }

        let pack = match read_workload_pack(&manifest_path) {
            Ok(pack) => pack,
            Err(error) => {
                failed = true;
                entries.push(pack_matrix_entry_failure(
                    ontology,
                    &manifest_path,
                    error.to_string(),
                    None,
                ));
                continue;
            }
        };

        if let Err(error) = validate_catalog_baseline_pack(ontology, &manifest_path, &pack) {
            failed = true;
            entries.push(pack_matrix_entry_failure(
                ontology,
                &manifest_path,
                error.to_string(),
                None,
            ));
            continue;
        }

        let mut pack = pack;
        if pack.dataset_base_iri.is_none() {
            pack.dataset_base_iri = Some(ontology.url.clone());
        }

        let prepared = match prepare_loaded_pack_run(
            &manifest_path,
            pack,
            config.connection_profiles_path.as_deref(),
            config.connection_profile_name.as_deref(),
            Some(live_connections.nrese.base_url.as_str()),
            live_connections
                .fuseki
                .as_ref()
                .map(|connection| connection.base_url.as_str()),
            config.fuseki_basic_auth.as_ref(),
        ) {
            Ok(prepared) => prepared,
            Err(error) => {
                failed = true;
                entries.push(pack_matrix_entry_failure(
                    ontology,
                    &manifest_path,
                    error.to_string(),
                    Some(pack_report),
                ));
                continue;
            }
        };

        let report_dir = prepare_pack_matrix_entry_report_dir(&config.report_dir, &ontology.name)?;
        let pack_config = PackConfig {
            nrese_base_url: Some(live_connections.nrese.base_url.clone()),
            fuseki_base_url: live_connections
                .fuseki
                .as_ref()
                .map(|connection| connection.base_url.clone()),
            fuseki_basic_auth: config.fuseki_basic_auth.clone(),
            connection_profiles_path: config.connection_profiles_path.clone(),
            connection_profile_name: config.connection_profile_name.clone(),
            workload_pack_path: manifest_path.clone(),
            execution_mode: config.execution_mode,
            iterations: config.iterations,
            report_dir: Some(report_dir),
        };
        let result = run_loaded_pack(&pack_config, prepared).await;

        match result {
            Ok(()) => entries.push(pack_matrix_entry_success(ontology, &manifest_path, pack_report)),
            Err(error) => {
                failed = true;
                entries.push(pack_matrix_entry_failure(
                    ontology,
                    &manifest_path,
                    error.to_string(),
                    Some(pack_report),
                ));
            }
        }
    }

    write_json_report(
        config.report_dir.join("pack-matrix-report.json"),
        &PackMatrixReport {
            mode: "pack-matrix",
            catalog_path: config.catalog_path.display().to_string(),
            packs_dir: config.packs_dir.display().to_string(),
            connection_profiles_path: config
                .connection_profiles_path
                .as_ref()
                .map(|path| path.display().to_string()),
            connection_profile_name: config.connection_profile_name.clone(),
            ontology_name: config.ontology_name.clone(),
            execution_mode: config.execution_mode,
            tier: config.tier.clone(),
            semantic_dialect: config.semantic_dialect,
            reasoning_feature: config.reasoning_feature,
            service_coverage: config.service_coverage,
            pack_runs: entries,
        },
    )?;

    if failed {
        bail!("one or more ontology pack runs failed");
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

fn prepare_pack_matrix_entry_report_dir(report_root: &Path, ontology_name: &str) -> Result<std::path::PathBuf> {
    let report_dir = report_root.join(ontology_name);
    fs::create_dir_all(&report_dir)
        .with_context(|| format!("failed to create pack report dir {}", report_dir.display()))?;
    Ok(report_dir)
}

fn ensure_report_dir(report_dir: Option<&Path>) -> Result<()> {
    if let Some(report_dir) = report_dir {
        fs::create_dir_all(report_dir)
            .with_context(|| format!("failed to create report dir {}", report_dir.display()))?;
    }
    Ok(())
}

#[derive(Debug, Clone)]
struct ResolvedLiveConnections {
    nrese: ServiceConnectionConfig,
    fuseki: Option<ServiceConnectionConfig>,
    invocation_profiles: crate::model::ServiceInvocationProfiles,
}

#[derive(Debug, Clone)]
struct PreparedPackRun {
    pack: WorkloadPackManifest,
    manifest_path: String,
    connection_profiles_path: Option<String>,
    connection_profile_name: Option<String>,
    nrese_connection: ServiceConnectionConfig,
    fuseki_connection: Option<ServiceConnectionConfig>,
    merged_invocation_profiles: crate::model::ServiceInvocationProfiles,
}

fn resolve_live_connections(
    connection_profiles_path: Option<&Path>,
    connection_profile_name: Option<&str>,
    nrese_base_url: Option<&str>,
    fuseki_base_url: Option<&str>,
    fuseki_basic_auth: Option<&crate::model::BasicAuthConfig>,
) -> Result<ResolvedLiveConnections> {
    let selected_profile = match (connection_profiles_path, connection_profile_name) {
        (Some(path), Some(name)) => {
            let registry = read_connection_profiles_registry(path)?;
            Some(resolve_live_connection_profile(&registry, name)?.clone())
        }
        (None, Some(_)) => bail!("--connection-profile requires --connection-profiles"),
        (Some(_), None) => bail!("--connection-profiles requires --connection-profile"),
        (None, None) => None,
    };

    let nrese = resolve_required_service_connection(
        "NRESE",
        selected_profile.as_ref().map(|profile| &profile.nrese),
        nrese_base_url,
        None,
    )?;
    let fuseki = resolve_optional_service_connection(
        "Fuseki",
        selected_profile.as_ref().and_then(|profile| profile.fuseki.as_ref()),
        fuseki_base_url,
        fuseki_basic_auth,
    )?;

    Ok(ResolvedLiveConnections {
        nrese,
        fuseki,
        invocation_profiles: selected_profile
            .map(|profile| profile.invocation_profiles)
            .unwrap_or_default(),
    })
}

fn prepare_pack_run(
    workload_pack_path: &Path,
    connection_profiles_path: Option<&Path>,
    connection_profile_name: Option<&str>,
    nrese_base_url: Option<&str>,
    fuseki_base_url: Option<&str>,
    fuseki_basic_auth: Option<&crate::model::BasicAuthConfig>,
) -> Result<PreparedPackRun> {
    let pack = read_workload_pack(workload_pack_path)?;
    prepare_loaded_pack_run(
        workload_pack_path,
        pack,
        connection_profiles_path,
        connection_profile_name,
        nrese_base_url,
        fuseki_base_url,
        fuseki_basic_auth,
    )
}

fn prepare_loaded_pack_run(
    workload_pack_path: &Path,
    pack: WorkloadPackManifest,
    connection_profiles_path: Option<&Path>,
    connection_profile_name: Option<&str>,
    nrese_base_url: Option<&str>,
    fuseki_base_url: Option<&str>,
    fuseki_basic_auth: Option<&crate::model::BasicAuthConfig>,
) -> Result<PreparedPackRun> {
    let live_connections = resolve_live_connections(
        connection_profiles_path,
        connection_profile_name,
        nrese_base_url,
        fuseki_base_url,
        fuseki_basic_auth,
    )?;
    let merged_invocation_profiles =
        merge_invocation_profiles(&live_connections.invocation_profiles, &pack.invocation_profiles)?;
    if live_connections.fuseki.is_some() {
        validate_pack_invocation_profiles(&pack.compat_suites, &merged_invocation_profiles)?;
    }

    Ok(PreparedPackRun {
        pack,
        manifest_path: workload_pack_path.display().to_string(),
        connection_profiles_path: connection_profiles_path.map(|path| path.display().to_string()),
        connection_profile_name: connection_profile_name.map(str::to_owned),
        nrese_connection: live_connections.nrese,
        fuseki_connection: live_connections.fuseki,
        merged_invocation_profiles,
    })
}

fn validate_pack_execution_mode(mode: PackExecutionMode, has_fuseki: bool) -> Result<()> {
    match mode {
        PackExecutionMode::Full => Ok(()),
        PackExecutionMode::CompatOnly if has_fuseki => Ok(()),
        PackExecutionMode::CompatOnly => {
            bail!("compat-only execution mode requires a Fuseki connection")
        }
    }
}

fn merge_pack_service_profile(
    connection: &ServiceConnectionConfig,
    profile: &ServiceRequestProfile,
) -> ServiceConnectionConfig {
    let mut headers = connection.headers.clone();
    headers.extend(profile.headers.clone());

    ServiceConnectionConfig {
        base_url: connection.base_url.clone(),
        headers,
        timeout_ms: profile.timeout_ms.or(connection.timeout_ms),
        basic_auth: connection.basic_auth.clone(),
    }
}

fn print_pack_header(label: &str, prepared: &PreparedPackRun) {
    println!("== {label} ==");
    println!("pack: {}", prepared.pack.name);
    println!("manifest: {}", prepared.manifest_path);
    println!("nrese base: {}", prepared.nrese_connection.base_url);
    if let Some(fuseki_connection) = &prepared.fuseki_connection {
        println!("fuseki base: {}", fuseki_connection.base_url);
    }
    if let Some(connection_profiles_path) = &prepared.connection_profiles_path {
        println!("connection profiles: {connection_profiles_path}");
    }
    if let Some(connection_profile_name) = &prepared.connection_profile_name {
        println!("connection profile: {connection_profile_name}");
    }
}

fn baseline_pack_manifest_path(packs_dir: &Path, ontology: &OntologyFixture) -> std::path::PathBuf {
    packs_dir.join(format!("{}-baseline", ontology.name)).join("pack.toml")
}

fn pack_matrix_matches_filters(ontology: &OntologyFixture, config: &PackMatrixConfig) -> bool {
    config
        .ontology_name
        .as_deref()
        .is_none_or(|name| ontology.name == name)
        && config
        .tier
        .as_deref()
        .is_none_or(|tier| ontology.tier == tier)
        && config.semantic_dialect.is_none_or(|dialect| {
            ontology.semantic_dialects.contains(&dialect)
        })
        && config.reasoning_feature.is_none_or(|feature| {
            ontology.reasoning_features.contains(&feature)
        })
        && config
            .service_coverage
            .is_none_or(|surface| ontology.service_coverage.contains(&surface))
}

fn pack_matrix_entry_success(
    ontology: &OntologyFixture,
    manifest_path: &Path,
    pack_report: PackArtifactReport,
) -> PackMatrixEntryReport {
    PackMatrixEntryReport {
        ontology_name: ontology.name.clone(),
        ontology_title: ontology.title.clone(),
        ontology_tier: ontology.tier.clone(),
        manifest_path: manifest_path.display().to_string(),
        semantic_dialects: ontology.semantic_dialects.clone(),
        reasoning_features: ontology.reasoning_features.clone(),
        service_coverage: ontology.service_coverage.clone(),
        status: "completed",
        error: None,
        pack_report: Some(pack_report),
    }
}

fn pack_matrix_entry_failure(
    ontology: &OntologyFixture,
    manifest_path: &Path,
    error: String,
    pack_report: Option<PackArtifactReport>,
) -> PackMatrixEntryReport {
    PackMatrixEntryReport {
        ontology_name: ontology.name.clone(),
        ontology_title: ontology.title.clone(),
        ontology_tier: ontology.tier.clone(),
        manifest_path: manifest_path.display().to_string(),
        semantic_dialects: ontology.semantic_dialects.clone(),
        reasoning_features: ontology.reasoning_features.clone(),
        service_coverage: ontology.service_coverage.clone(),
        status: "failed",
        error: Some(error),
        pack_report,
    }
}

fn pack_matrix_entry_missing_manifest(
    ontology: &OntologyFixture,
    manifest_path: &Path,
) -> PackMatrixEntryReport {
    pack_matrix_entry_failure(
        ontology,
        manifest_path,
        format!("missing baseline pack manifest {}", manifest_path.display()),
        None,
    )
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
    println!(
        "serialization: {} | semantic dialects: {} | reasoning features: {} | services: {}",
        ontology_serialization_label(ontology.serialization),
        join_semantic_dialects(&ontology.semantic_dialects),
        join_reasoning_features(&ontology.reasoning_features),
        join_service_surfaces(&ontology.service_coverage)
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
    base_iri: Option<&str>,
    replace: bool,
) -> Result<()> {
    let extra_headers = base_iri
        .map(|value| CompatHeaders::from([("content-location".to_owned(), value.to_owned())]))
        .unwrap_or_default();
    let outcome = execute_graph_write_raw(
        client,
        target,
        GraphWriteRequest {
            graph_target: &CompatGraphTarget::DefaultGraph,
            content_type,
            payload,
            replace,
            extra_headers: &extra_headers,
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

fn ontology_serialization_label(serialization: OntologySerialization) -> &'static str {
    match serialization {
        OntologySerialization::Turtle => "turtle",
        OntologySerialization::RdfXml => "rdf-xml",
    }
}

fn semantic_dialect_label(dialect: OntologySemanticDialect) -> &'static str {
    match dialect {
        OntologySemanticDialect::Rdfs => "rdfs",
        OntologySemanticDialect::Owl => "owl",
        OntologySemanticDialect::Foaf => "foaf",
        OntologySemanticDialect::Org => "org",
        OntologySemanticDialect::Time => "time",
        OntologySemanticDialect::ProvO => "prov-o",
        OntologySemanticDialect::Skos => "skos",
        OntologySemanticDialect::Sosa => "sosa",
        OntologySemanticDialect::Ssn => "ssn",
        OntologySemanticDialect::Dcat => "dcat",
        OntologySemanticDialect::Vcard => "vcard",
        OntologySemanticDialect::Odrl => "odrl",
        OntologySemanticDialect::DcmiTerms => "dcmi-terms",
    }
}

fn reasoning_feature_label(feature: OntologyReasoningFeature) -> &'static str {
    match feature {
        OntologyReasoningFeature::SubclassClosure => "subclass-closure",
        OntologyReasoningFeature::SubpropertyClosure => "subproperty-closure",
        OntologyReasoningFeature::DomainRangeTyping => "domain-range-typing",
        OntologyReasoningFeature::InverseProperty => "inverse-property",
        OntologyReasoningFeature::TransitiveProperty => "transitive-property",
        OntologyReasoningFeature::SymmetricProperty => "symmetric-property",
        OntologyReasoningFeature::Disjointness => "disjointness",
        OntologyReasoningFeature::Identity => "identity",
        OntologyReasoningFeature::Restrictions => "restrictions",
        OntologyReasoningFeature::ListAxioms => "list-axioms",
    }
}

fn service_surface_label(surface: OntologyServiceSurface) -> &'static str {
    match surface {
        OntologyServiceSurface::CatalogSync => "catalog-sync",
        OntologyServiceSurface::Compat => "compat",
        OntologyServiceSurface::Tell => "tell",
        OntologyServiceSurface::GraphStore => "graph-store",
        OntologyServiceSurface::Query => "query",
        OntologyServiceSurface::Reasoner => "reasoner",
        OntologyServiceSurface::Benchmark => "benchmark",
    }
}

fn join_semantic_dialects(values: &[OntologySemanticDialect]) -> String {
    values
        .iter()
        .map(|value| semantic_dialect_label(*value))
        .collect::<Vec<_>>()
        .join(", ")
}

fn join_reasoning_features(values: &[OntologyReasoningFeature]) -> String {
    values
        .iter()
        .map(|value| reasoning_feature_label(*value))
        .collect::<Vec<_>>()
        .join(", ")
}

fn join_service_surfaces(values: &[OntologyServiceSurface]) -> String {
    values
        .iter()
        .map(|value| service_surface_label(*value))
        .collect::<Vec<_>>()
        .join(", ")
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

    use crate::model::{
        CompatCase, CompatHeaders, CompatOperation, OntologyFixture, OntologyReasoningFeature,
        OntologySemanticDialect, OntologySerialization, OntologyServiceSurface,
        PackExecutionMode, PackMatrixConfig, ServiceConnectionConfig, ServiceRequestProfile,
    };

    use super::{
        CompatRunArtifact, baseline_pack_manifest_path, compat_report_filename,
        pack_matrix_matches_filters, prepare_pack_matrix_entry_report_dir,
        validate_pack_execution_mode,
        pack_compat_suite_report, pack_matrix_entry_missing_manifest,
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

    #[test]
    fn baseline_pack_manifest_path_uses_catalog_fixture_name() {
        let ontology = OntologyFixture {
            name: "skos".to_owned(),
            title: "SKOS".to_owned(),
            url: "https://www.w3.org/2009/08/skos-reference/skos.rdf".to_owned(),
            media_type: "application/rdf+xml".to_owned(),
            serialization: OntologySerialization::RdfXml,
            filename: "skos.rdf".to_owned(),
            tier: "medium".to_owned(),
            focus_terms: vec!["http://www.w3.org/2004/02/skos/core#Concept".to_owned()],
            semantic_dialects: vec![OntologySemanticDialect::Skos],
            reasoning_features: vec![OntologyReasoningFeature::TransitiveProperty],
            service_coverage: vec![OntologyServiceSurface::Benchmark],
        };

        let manifest = baseline_pack_manifest_path(Path::new("fixtures/packs"), &ontology);

        assert!(manifest.ends_with("fixtures/packs\\skos-baseline\\pack.toml") || manifest.ends_with("fixtures/packs/skos-baseline/pack.toml"));
    }

    #[test]
    fn missing_manifest_entry_reports_failed_status() {
        let ontology = OntologyFixture {
            name: "prov".to_owned(),
            title: "PROV-O".to_owned(),
            url: "https://www.w3.org/ns/prov.ttl".to_owned(),
            media_type: "text/turtle".to_owned(),
            serialization: OntologySerialization::Turtle,
            filename: "prov.ttl".to_owned(),
            tier: "broad".to_owned(),
            focus_terms: vec!["http://www.w3.org/ns/prov#Entity".to_owned()],
            semantic_dialects: vec![OntologySemanticDialect::ProvO],
            reasoning_features: vec![OntologyReasoningFeature::InverseProperty],
            service_coverage: vec![OntologyServiceSurface::Benchmark],
        };

        let entry =
            pack_matrix_entry_missing_manifest(&ontology, Path::new("fixtures/packs/prov-baseline/pack.toml"));

        assert_eq!(entry.status, "failed");
        assert!(entry.error.is_some());
        assert!(entry.pack_report.is_none());
    }

    #[test]
    fn pack_matrix_filter_matches_reasoning_feature() {
        let ontology = OntologyFixture {
            name: "time".to_owned(),
            title: "Time".to_owned(),
            url: "https://www.w3.org/2006/time.ttl".to_owned(),
            media_type: "text/turtle".to_owned(),
            serialization: OntologySerialization::Turtle,
            filename: "time.ttl".to_owned(),
            tier: "medium".to_owned(),
            focus_terms: vec!["http://www.w3.org/2006/time#Interval".to_owned()],
            semantic_dialects: vec![OntologySemanticDialect::Time],
            reasoning_features: vec![
                OntologyReasoningFeature::TransitiveProperty,
                OntologyReasoningFeature::InverseProperty,
            ],
            service_coverage: vec![OntologyServiceSurface::Benchmark],
        };
        let config = PackMatrixConfig {
            nrese_base_url: Some("http://127.0.0.1:8080".to_owned()),
            fuseki_base_url: None,
            fuseki_basic_auth: None,
            connection_profiles_path: None,
            connection_profile_name: None,
            catalog_path: "catalog.toml".into(),
            packs_dir: "packs".into(),
            ontology_name: None,
            execution_mode: PackExecutionMode::Full,
            tier: Some("medium".to_owned()),
            semantic_dialect: Some(OntologySemanticDialect::Time),
            reasoning_feature: Some(OntologyReasoningFeature::TransitiveProperty),
            service_coverage: Some(OntologyServiceSurface::Benchmark),
            iterations: 20,
            report_dir: "artifacts".into(),
        };

        assert!(pack_matrix_matches_filters(&ontology, &config));
    }

    #[test]
    fn pack_matrix_filter_matches_explicit_ontology_name() {
        let ontology = OntologyFixture {
            name: "skos".to_owned(),
            title: "SKOS".to_owned(),
            url: "https://www.w3.org/2009/08/skos-reference/skos.rdf".to_owned(),
            media_type: "application/rdf+xml".to_owned(),
            serialization: OntologySerialization::RdfXml,
            filename: "skos.rdf".to_owned(),
            tier: "medium".to_owned(),
            focus_terms: vec!["http://www.w3.org/2004/02/skos/core#Concept".to_owned()],
            semantic_dialects: vec![OntologySemanticDialect::Skos],
            reasoning_features: vec![OntologyReasoningFeature::SubpropertyClosure],
            service_coverage: vec![OntologyServiceSurface::Benchmark],
        };
        let matching = PackMatrixConfig {
            nrese_base_url: Some("http://127.0.0.1:8080".to_owned()),
            fuseki_base_url: None,
            fuseki_basic_auth: None,
            connection_profiles_path: None,
            connection_profile_name: None,
            catalog_path: "catalog.toml".into(),
            packs_dir: "packs".into(),
            ontology_name: Some("skos".to_owned()),
            execution_mode: PackExecutionMode::Full,
            tier: None,
            semantic_dialect: None,
            reasoning_feature: None,
            service_coverage: None,
            iterations: 20,
            report_dir: "artifacts".into(),
        };
        let non_matching = PackMatrixConfig {
            ontology_name: Some("foaf".to_owned()),
            ..matching.clone()
        };

        assert!(pack_matrix_matches_filters(&ontology, &matching));
        assert!(!pack_matrix_matches_filters(&ontology, &non_matching));
    }

    #[test]
    fn pack_matrix_filter_matches_compat_service_coverage() {
        let ontology = OntologyFixture {
            name: "org".to_owned(),
            title: "ORG".to_owned(),
            url: "https://www.w3.org/ns/org.ttl".to_owned(),
            media_type: "text/turtle".to_owned(),
            serialization: OntologySerialization::Turtle,
            filename: "org.ttl".to_owned(),
            tier: "medium".to_owned(),
            focus_terms: vec!["http://www.w3.org/ns/org#Organization".to_owned()],
            semantic_dialects: vec![OntologySemanticDialect::Org],
            reasoning_features: vec![OntologyReasoningFeature::InverseProperty],
            service_coverage: vec![
                OntologyServiceSurface::Compat,
                OntologyServiceSurface::Reasoner,
            ],
        };
        let matching = PackMatrixConfig {
            nrese_base_url: Some("http://127.0.0.1:8080".to_owned()),
            fuseki_base_url: None,
            fuseki_basic_auth: None,
            connection_profiles_path: None,
            connection_profile_name: None,
            catalog_path: "catalog.toml".into(),
            packs_dir: "packs".into(),
            ontology_name: None,
            execution_mode: PackExecutionMode::Full,
            tier: None,
            semantic_dialect: None,
            reasoning_feature: None,
            service_coverage: Some(OntologyServiceSurface::Compat),
            iterations: 20,
            report_dir: "artifacts".into(),
        };
        let non_matching = PackMatrixConfig {
            service_coverage: Some(OntologyServiceSurface::Benchmark),
            ..matching.clone()
        };

        assert!(pack_matrix_matches_filters(&ontology, &matching));
        assert!(!pack_matrix_matches_filters(&ontology, &non_matching));
    }

    #[test]
    fn compat_only_mode_requires_fuseki() {
        assert!(validate_pack_execution_mode(PackExecutionMode::Full, false).is_ok());
        assert!(validate_pack_execution_mode(PackExecutionMode::CompatOnly, true).is_ok());
        let error = validate_pack_execution_mode(PackExecutionMode::CompatOnly, false)
            .expect_err("compat-only without fuseki should fail");
        assert!(error
            .to_string()
            .contains("compat-only execution mode requires a Fuseki connection"));
    }

    #[test]
    fn prepare_pack_matrix_entry_report_dir_creates_nested_directory() {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let report_dir = prepare_pack_matrix_entry_report_dir(temp_dir.path(), "foaf")
            .expect("report dir");

        assert!(report_dir.is_dir());
        assert!(report_dir.ends_with("foaf"));
    }
}
