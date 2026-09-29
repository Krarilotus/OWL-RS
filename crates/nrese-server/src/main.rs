use anyhow::{Context, Result};
use nrese_reasoner::ReasonerService;
use nrese_store::{BulkLoadRequest, GraphTarget, StoreService};
use tokio::net::TcpListener;
use tracing_subscriber::EnvFilter;

use nrese_server::ai::AiSuggestionService;
use nrese_server::{AppState, CliCommand, CliConfig, LoadCommand, ServerConfig, build_app};

/// mimalloc (Pf7a): 7-20% faster query sets and 30% faster bulk loads than the system
/// allocator in the perf lab (benches/baselines/perf-lab/2026-09-27-r198-*).
#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

#[tokio::main]
async fn main() -> Result<()> {
    let _ = dotenvy::dotenv();
    init_tracing();

    let cli = CliConfig::from_args(std::env::args_os())?;
    let config = ServerConfig::load(cli.config_path.as_deref())?;
    let store = StoreService::new(config.store.clone())?;
    let ruleset = config.reasoner.materialised_ruleset();
    if let CliCommand::Load(load) = cli.command {
        bulk_load(&store, load)?;
        if let Some(ruleset) = ruleset {
            let report = store
                .rematerialise(ruleset)
                .context("reasoning after the bulk load failed")?;
            if report.violations > 0 {
                // The data stays loaded (for diagnosis and repair); the server will start
                // in quarantine.
                anyhow::bail!(
                    "the loaded data is inconsistent under {}: {} violation(s)",
                    ruleset.name(),
                    report.violations
                );
            }
        }
        return Ok(());
    }
    // Reasoner v2: bring the inferred stack in line with the configured ruleset, unless
    // the recorded state says it already is (same ruleset and semantics). Without v2
    // reasoning, a leftover stack is cleared so reads never see stale inferences.
    match ruleset {
        Some(ruleset)
            if store
                .reasoning_state()
                .is_some_and(|state| state.is_current_for(ruleset)) =>
        {
            tracing::info!(ruleset = ruleset.name(), "inferred stack is current");
        }
        Some(ruleset) => {
            store
                .rematerialise(ruleset)
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

    axum::serve(listener, app)
        .await
        .context("nrese-server terminated unexpectedly")
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
