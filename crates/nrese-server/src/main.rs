use anyhow::{Context, Result};
use axum::serve::ListenerExt;
use nrese_reasoner::ReasonerService;
use nrese_store::{
    BulkLoadRequest, CancellationToken, GraphResultFormat, GraphTarget, PreparedQuery,
    SolutionsResultFormat, SparqlQueryRequest, StoreService,
};
use tokio::net::TcpListener;
use tracing_subscriber::EnvFilter;

use nrese_server::ai::AiSuggestionService;
use nrese_server::{
    AppState, CliCommand, CliConfig, LoadCommand, QueryCommand, ServerConfig, build_app,
};

/// mimalloc (Pf7a): 7-20% faster query sets and 30% faster bulk loads than the system
/// allocator in the perf lab (benches/baselines/perf-lab/2026-09-27-r198-*).
#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

fn main() -> Result<()> {
    // Built for a CPU with more than this one has (NRESE_TARGET_CPU): stop here, with the
    // reason, before any code compiled for that CPU runs (the async runtime is built after
    // this), not on an illegal instruction later.
    nrese_engine::cpu::exit_if_missing();
    // What threads freed goes back to the system after bulk loads and reasoning.
    nrese_engine::memory::set_release(|force| unsafe { libmimalloc_sys::mi_collect(force) });
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
    let config = ServerConfig::load(cli.config_path.as_deref())?;
    if cli.command == CliCommand::CheckConfig {
        // Loading validated everything; show what takes effect.
        print!("{}", config.summary());
        println!("cpu: {}", nrese_engine::cpu::summary());
        println!("configuration is valid");
        return Ok(());
    }
    let store = StoreService::new(config.store.clone())?;
    if let CliCommand::Query(query) = cli.command {
        return query_once(&store, query);
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
        return Ok(());
    }
    // Reasoner v2: bring the inferred stack in line with the configured rules, unless the
    // recorded state says it already is (same rules and semantics). Without reasoning, a
    // leftover stack is cleared so reads never see stale inferences.
    match program {
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
    let state = AppState::new(
        store.clone(),
        reasoner.clone(),
        config.policy.clone(),
        ai,
        config.deployment_posture,
    );
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
    axum::serve(listener, app)
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
    let mut request = SparqlQueryRequest::new(text);
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
    let prepared = PreparedQuery::parse(&request).context("parsing the query failed")?;
    let stdout = std::io::stdout();
    let mut out = std::io::BufWriter::new(stdout.lock());
    store
        .run_query(&prepared, &CancellationToken::new(), &mut out)
        .context("the query failed")?;
    out.flush()?;
    Ok(())
}

/// `nrese-server load`: bulk-loads files into the configured store and exits.
fn bulk_load(store: &StoreService, load: LoadCommand) -> Result<()> {
    let request = BulkLoadRequest {
        files: load.files,
        replace: load.replace,
        graph: load
            .graph
            .map_or(GraphTarget::DefaultGraph, GraphTarget::NamedGraph),
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
