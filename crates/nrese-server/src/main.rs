use anyhow::{Context, Result};
use axum::serve::ListenerExt;
use nrese_reasoner::ReasonerService;
use nrese_store::{
    BulkLoadRequest, CancellationToken, GraphResultFormat, GraphTarget, SolutionsResultFormat,
    SparqlQueryRequest, StoreService,
};
use tokio::net::TcpListener;
use tracing_subscriber::EnvFilter;

use nrese_server::ai::AiSuggestionService;
use nrese_server::{
    AppState, CliCommand, CliConfig, LoadCommand, PrintQueryCommand, QueryCommand, ServerConfig,
    build_app,
};

/// mimalloc (Pf7a): 7-20% faster query sets and 30% faster bulk loads than the system
/// allocator in the perf lab (benches/baselines/perf-lab/2026-09-27-r198-*).
#[cfg(not(any(system_alloc, alloc_profile)))]
#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

/// `RUSTFLAGS="--cfg system_alloc"`: the system allocator, to compare (with `alloc_profile`
/// too: the system allocator, counted).
#[cfg(all(system_alloc, alloc_profile))]
#[global_allocator]
static GLOBAL: nrese_exec::heap::Counting<std::alloc::System> =
    nrese_exec::heap::Counting(std::alloc::System);

/// `RUSTFLAGS="--cfg alloc_profile"`: mimalloc with every allocation counted, so /metrics
/// shows the bytes the program holds beside what the process holds (soak runs: growth
/// that is the program's, or the allocator's). The counting slows allocation-heavy work.
#[cfg(all(alloc_profile, not(system_alloc)))]
#[global_allocator]
static GLOBAL: nrese_exec::heap::Counting<mimalloc::MiMalloc> =
    nrese_exec::heap::Counting(mimalloc::MiMalloc);

fn main() -> Result<()> {
    // Built for a CPU with more than this one has (NRESE_TARGET_CPU): stop here, with the
    // reason, before any code compiled for that CPU runs (the async runtime is built after
    // this), not on an illegal instruction later.
    nrese_store::cpu::exit_if_missing();
    // What threads freed goes back to the system after bulk loads and reasoning.
    // SAFETY: mimalloc's `mi_collect` may be called at any time, from any thread.
    nrese_store::set_memory_release(|force| unsafe { libmimalloc_sys::mi_collect(force) });
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("starting the async runtime failed")?
        .block_on(run())
}

async fn run() -> Result<()> {
    let _ = dotenvy::dotenv();
    init_tracing();

    let cli = CliConfig::from_args(std::env::args_os())?;
    if let CliCommand::Convert(convert) = &cli.command {
        let statements = nrese_store::convert_file(&convert.input, &convert.output)
            .with_context(|| format!("converting {}", convert.input.display()))?;
        tracing::info!(statements, output = %convert.output.display(), "converted");
        return Ok(());
    }
    if cli.command == CliCommand::ConfigSchema {
        println!(
            "{}",
            serde_json::to_string_pretty(&nrese_server::config::settings::schema())?
        );
        return Ok(());
    }
    if let CliCommand::PrintQuery(print) = &cli.command
        && !print.schema.is_empty()
    {
        // Against the schema files alone: no configured store.
        let store = StoreService::new(nrese_store::StoreConfig::in_memory())?;
        bulk_load(
            &store,
            LoadCommand {
                files: print.schema.clone(),
                ..LoadCommand::default()
            },
        )?;
        return print_query(&store, print.clone());
    }
    let mut config = ServerConfig::load_with(cli.config_path.as_deref(), &cli.overrides)?;
    // The process owner installs this once. Repository and system stores only carry
    // their own watches and must never overwrite the server's safety fallback.
    config.install_process_memory_policy();
    // The default repository's reasoning as changed through the engine API overrides the
    // configuration's (it is kept in the data directory).
    if let Some(settings) = nrese_server::repositories::stored_default_settings(&config.store)
        .map_err(|error| anyhow::anyhow!(error))?
        && let Some(reasoner) = settings
            .reasoner_config()
            .map_err(|error| anyhow::anyhow!(error))?
    {
        tracing::info!(
            reasoning = ?reasoner.mode(),
            "the default repository's stored settings choose its reasoning"
        );
        config.reasoner = reasoner;
    }
    if cli.command == CliCommand::CheckConfig {
        // Loading validated everything; show what takes effect.
        print!("{}", config.summary());
        println!("cpu: {}", nrese_store::cpu::summary());
        println!("configuration is valid");
        return Ok(());
    }
    if let CliCommand::PruneArchive(Some(revision)) = cli.command {
        let archive = config.store.data_dir.join("wal-archive");
        let removed = nrese_store::prune_wal_archive(&archive, revision)
            .with_context(|| format!("pruning {}", archive.display()))?;
        tracing::info!(removed, revision, archive = %archive.display(), "archive pruned");
        return Ok(());
    }
    if let CliCommand::Restore(restore) = &cli.command {
        let (manifest, revision) = nrese_store::restore_until(
            &restore.backup,
            &config.store.data_dir,
            &restore.wal,
            restore.until_revision,
            restore.until_time,
        )
        .with_context(|| format!("restoring {}", restore.backup.display()))?;
        tracing::info!(
            image_revision = manifest.revision,
            revision,
            data_dir = %config.store.data_dir.display(),
            "restored"
        );
        return Ok(());
    }
    let replica = config.replication.mode == nrese_server::replication::ReplicationMode::Replica;
    if replica && cli.command == CliCommand::Serve {
        // A replica's start: the primary's image, if the data directory holds no store.
        if let Some(revision) =
            nrese_server::replication::bootstrap(&config.replication, &config.store.data_dir)
                .await
                .context("starting the replica from its primary's image")?
        {
            tracing::info!(revision, primary = ?config.replication.primary, "replica started from an image");
        }
    }
    let store = StoreService::new(config.store.clone())?;
    if let CliCommand::Backup(dir) = &cli.command {
        let manifest = store
            .backup_image(dir)
            .with_context(|| format!("backing up into {}", dir.display()))?;
        tracing::info!(
            revision = manifest.revision,
            quads = manifest.quads,
            bytes = manifest.bytes,
            dir = %dir.display(),
            "backed up"
        );
        return Ok(());
    }
    if let CliCommand::Query(query) = cli.command {
        return query_once(&store, query);
    }
    if let CliCommand::PrintQuery(print) = cli.command {
        return print_query(&store, print);
    }
    let program = config.reasoner.materialised_program();
    if let CliCommand::Load(load) = cli.command {
        bulk_load(&store, load)?;
        if let Some(program) = &program {
            let report = store
                .rematerialise(program)
                .context("reasoning after the bulk load failed")?;
            if report.violations > 0 {
                // The data stays loaded (for diagnosis and repair); the server will start
                // in quarantine.
                anyhow::bail!(
                    "the loaded data is inconsistent under {}: {} violation(s)",
                    program.name(),
                    report.violations
                );
            }
        }
        // The planner's statistics belong to the loaded store: built here, saved beside
        // the checkpoint, read by the server at its first query.
        let started = std::time::Instant::now();
        store.prepare_statistics();
        tracing::info!(
            ms = started.elapsed().as_millis() as u64,
            "planner statistics prepared"
        );
        return Ok(());
    }
    // Reasoner v2: bring the inferred stack in line with the configured rules, unless the
    // recorded state says it already is (same rules and semantics). Without reasoning, a
    // leftover stack is cleared so reads never see stale inferences.
    match program {
        // A replica's inferences come with the primary's records.
        _ if replica => {
            tracing::info!("read replica: no reasoning here, the primary's log carries it");
        }
        Some(program) if store.reasoning_is_current(&program) => {
            tracing::info!(ruleset = %program.name(), "inferred stack is current");
        }
        Some(program) => {
            store
                .rematerialise(&program)
                .context("startup reasoning failed")?;
        }
        None => {
            let cleared = store
                .clear_inferred()
                .context("clearing inferences failed")?;
            if cleared > 0 {
                tracing::info!(cleared, "reasoning is off: inferred stack cleared");
            }
        }
    }
    if config.store.federation.enabled() {
        let client = nrese_server::federation::HttpServiceClient::new(
            config.store.federation.clone(),
            tokio::runtime::Handle::current(),
        )?;
        store.set_service_client(std::sync::Arc::new(client));
        tracing::info!(allow = ?config.store.federation.allow, "SERVICE may call these endpoints");
    }
    let reasoner = ReasonerService::new(config.reasoner.clone());
    let ai = AiSuggestionService::new(config.ai.clone())?;
    let ontology_path = store
        .preloaded_ontology_path()
        .map(|path| path.to_path_buf());
    let state = AppState::try_new(
        store.clone(),
        reasoner.clone(),
        config.policy.clone(),
        ai,
        config.deployment_posture,
    )?;
    let state = state.with_replication(config.replication.clone());
    if replica {
        nrese_server::replication::follow(state.clone())?;
        tracing::info!(primary = ?config.replication.primary, "following the primary's log");
    }
    state.mark_ready();
    let app = build_app(state);

    let listener = TcpListener::bind(config.bind_address)
        .await
        .with_context(|| format!("failed to bind {}", config.bind_address))?;

    tracing::info!(
        bind_address = %config.bind_address,
        deployment_posture = config.deployment_posture.as_str(),
        data_dir = %store.config().data_dir.display(),
        reasoning_mode = ?reasoner.config().mode(),
        reasoner_profile = reasoner.profile_name(),
        reasoner_capabilities = reasoner.capabilities().len(),
        ontology_path = ontology_path.as_ref().map(|path| path.display().to_string()),
        "nrese-server bootstrap complete"
    );

    // Without TCP_NODELAY a response written in several pieces (every streamed result)
    // waits for the client's delayed acknowledgement: about 40 ms on Linux, whatever the
    // query took.
    let listener = listener.tap_io(|stream| {
        if let Err(error) = stream.set_nodelay(true) {
            tracing::debug!(%error, "could not set TCP_NODELAY on a connection");
        }
    });
    // Peer addresses for the mtls mode's trusted proxies.
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
    )
    .await
    .context("nrese-server terminated unexpectedly")
}

/// `nrese-server query`: one query on the configured store as it is (no reasoning first),
/// its results on standard output.
fn query_once(store: &StoreService, command: QueryCommand) -> Result<()> {
    use std::io::Write as _;
    let text = match (command.query, command.file) {
        (Some(text), _) => text,
        (None, Some(file)) => {
            std::fs::read_to_string(&file).with_context(|| format!("reading {}", file.display()))?
        }
        (None, None) => anyhow::bail!("`query` needs a query or --file"),
    };
    // The operator on the command line reads every graph.
    let mut request = SparqlQueryRequest::all(text);
    if let Some(format) = command.format.as_deref() {
        match format.to_ascii_lowercase().as_str() {
            "json" => request.solutions_format = SolutionsResultFormat::Json,
            "xml" => request.solutions_format = SolutionsResultFormat::Xml,
            "csv" => request.solutions_format = SolutionsResultFormat::Csv,
            "tsv" => request.solutions_format = SolutionsResultFormat::Tsv,
            other => {
                request.graph_format = GraphResultFormat::from_extension(other)
                    .with_context(|| format!("unknown result format {other}"))?;
            }
        }
    }
    let prepared = store
        .prepare_query(&request)
        .context("parsing the query failed")?;
    let stdout = std::io::stdout();
    let mut out = std::io::BufWriter::new(stdout.lock());
    store
        .run_query(&prepared, &CancellationToken::new(), &mut out)
        .context("the query failed")?;
    out.flush()?;
    Ok(())
}

/// `nrese-server print-query`: the query as standard SPARQL 1.1 for a store without
/// reasoning on standard output, after a comment with the completeness of its answers; what
/// keeps it from being written exactly on standard error, with exit status 2.
fn print_query(store: &StoreService, command: PrintQueryCommand) -> Result<()> {
    use nrese_sparql::ql::{PrintForm, Printed};
    let text = match (command.query, command.file) {
        (Some(text), _) => text,
        (None, Some(file)) => {
            std::fs::read_to_string(&file).with_context(|| format!("reading {}", file.display()))?
        }
        (None, None) => anyhow::bail!("`print-query` needs a query or --file"),
    };
    let form = command
        .form
        .as_deref()
        .map_or(Some(PrintForm::Paths), PrintForm::from_name)
        .context("--form is paths or values")?;
    match store
        .print_query(&text, form)
        .context("printing the query failed")?
    {
        Printed::Query { text, completeness } => {
            println!("# NRESE completeness: {}", completeness.header());
            println!("{text}");
            Ok(())
        }
        Printed::NotExpressible(reasons) => {
            eprintln!("not expressible in SPARQL 1.1 without reasoning:");
            for reason in reasons {
                eprintln!("  {reason}");
            }
            std::process::exit(2);
        }
    }
}

/// `nrese-server load`: bulk-loads files into the configured store and exits.
fn bulk_load(store: &StoreService, load: LoadCommand) -> Result<()> {
    let request = BulkLoadRequest {
        files: load.files,
        replace: load.replace,
        graph: load
            .graph
            .map_or(GraphTarget::DefaultGraph, GraphTarget::NamedGraph),
        skip_errors: load.skip_errors,
    };
    let report = store.bulk_load(&request).context("bulk load failed")?;
    let seconds = report.elapsed.as_secs_f64();
    tracing::info!(
        revision = report.revision,
        parsed = report.parsed,
        inserted = report.inserted,
        deleted = report.deleted,
        seconds,
        quads_per_second = (report.parsed as f64 / seconds) as u64,
        skipped = report.skipped,
        "bulk load complete"
    );
    Ok(())
}

fn init_tracing() {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .compact()
        .init();
}
