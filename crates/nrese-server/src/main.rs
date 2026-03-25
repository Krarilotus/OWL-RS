use anyhow::{Context, Result};
use nrese_reasoner::ReasonerService;
use nrese_store::StoreService;
use tokio::net::TcpListener;
use tracing_subscriber::EnvFilter;

use nrese_server::ai::AiSuggestionService;
use nrese_server::{AppState, CliConfig, ServerConfig, build_app};

#[tokio::main]
async fn main() -> Result<()> {
    let _ = dotenvy::dotenv();
    init_tracing();

    let cli = CliConfig::from_args(std::env::args_os())?;
    let config = ServerConfig::load(cli.config_path.as_deref())?;
    let store = StoreService::new(config.store.clone())?;
    let reasoner = ReasonerService::new(config.reasoner.clone());
    let ai = AiSuggestionService::new(config.ai.clone())?;
    let ontology_path = store
        .preloaded_ontology_path()
        .map(|path| path.to_path_buf());
    let state = AppState::new(store.clone(), reasoner.clone(), config.policy.clone(), ai);
    state.mark_ready();
    let app = build_app(state);

    let listener = TcpListener::bind(config.bind_address)
        .await
        .with_context(|| format!("failed to bind {}", config.bind_address))?;

    tracing::info!(
        bind_address = %config.bind_address,
        data_dir = %store.config().data_dir.display(),
        reasoning_mode = ?reasoner.config().mode,
        reasoner_profile = reasoner.profile_name(),
        reasoner_capabilities = reasoner.capabilities().len(),
        ontology_path = ontology_path.as_ref().map(|path| path.display().to_string()),
        "nrese-server bootstrap complete"
    );

    axum::serve(listener, app)
        .await
        .context("nrese-server terminated unexpectedly")
}

fn init_tracing() {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .compact()
        .init();
}
