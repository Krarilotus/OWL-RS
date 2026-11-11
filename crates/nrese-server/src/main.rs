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
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("starting the async runtime failed")?;
    let result = runtime.block_on(run());
    // After a stop, work a drain cut short (a long query on a blocking thread) isn't waited
    // for: the process ends as on a kill, and every acknowledged commit is already synced.
    runtime.shutdown_timeout(std::time::Duration::from_secs(1));
    result
}

/// How long the requests in flight may finish after a stop signal, leaving the rest of
/// [`STOP_DEADLINE`] for the replica's follower and the store to close.
const DRAIN: std::time::Duration = std::time::Duration::from_secs(5);

/// The whole stop's bound, from the signal: inside the 10 s a container runtime
/// (`docker stop`) waits before it kills, with room for the process to end.
const STOP_DEADLINE: std::time::Duration = std::time::Duration::from_secs(8);

/// What a stop under way waits for: an index into [`STOP_STEPS`], for the line the deadline
/// logs.
static STOP_STEP: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
const STOP_STEPS: [&str; 3] = [
    "the requests in flight",
    "the replica's follower",
    "the store's close (a checkpoint or merge in progress)",
];

fn stop_step(step: usize) {
    STOP_STEP.store(step, std::sync::atomic::Ordering::SeqCst);
}

/// A close that takes long (a checkpoint of a large store), for the stop deadline's test:
/// `NRESE_TEST_HOLD_CLOSE_MS` in a debug build. A release build has no such hold.
fn hold_the_close_for_a_test() {
    #[cfg(debug_assertions)]
    if let Some(ms) = std::env::var("NRESE_TEST_HOLD_CLOSE_MS")
        .ok()
        .and_then(|ms| ms.parse().ok())
    {
        std::thread::sleep(std::time::Duration::from_millis(ms));
    }
}

/// The stop's deadline: a thread made before the server serves (it doesn't serve without
/// one), armed by the signal. At [`STOP_DEADLINE`] after it, if the stop hasn't ended the
/// process, it ends it with code 0, as a kill would, saying what the stop still waited
/// for. What is left is abandoned: every acknowledged commit is synced, and the next open
/// recovers from the checkpoint and the log.
struct StopDeadline(std::sync::mpsc::Sender<&'static str>);

impl StopDeadline {
    fn start() -> std::io::Result<Self> {
        let (arm, armed) = std::sync::mpsc::channel::<&'static str>();
        std::thread::Builder::new()
            .name("nrese-stop-deadline".to_owned())
            .spawn(move || {
                // Never armed: the server ended without a signal.
                let Ok(signal) = armed.recv() else { return };
                std::thread::sleep(STOP_DEADLINE);
                let step = STOP_STEP.load(std::sync::atomic::Ordering::SeqCst);
                let waiting_for = STOP_STEPS[step.min(STOP_STEPS.len() - 1)];
                // The line on a thread of its own, given a moment: a log reader that stopped
                // reading can't hold the end.
                let _ = std::thread::Builder::new().spawn(move || {
                    tracing::warn!(
                        signal,
                        seconds = STOP_DEADLINE.as_secs(),
                        waiting_for,
                        "nrese-server stop deadline reached: ending the process"
                    );
                });
                std::thread::sleep(std::time::Duration::from_millis(200));
                end_now();
            })?;
        Ok(Self(arm))
    }

    /// Starts the deadline: the stop has begun.
    fn arm(&self, signal: &'static str) {
        let _ = self.0.send(signal);
    }
}

/// Ends the process at once with code 0, as a kill would: no exit handlers, no destructors,
/// nothing that could wait.
fn end_now() -> ! {
    #[cfg(unix)]
    // SAFETY: `_exit` takes no pointers and only ends the process.
    unsafe {
        libc::_exit(0)
    }
    #[cfg(windows)]
    {
        use windows_sys::Win32::System::Threading::{GetCurrentProcess, TerminateProcess};
        // SAFETY: the current process's pseudo-handle, which needs no closing.
        unsafe {
            TerminateProcess(GetCurrentProcess(), 0);
        }
        // Not reached: the process ends in `TerminateProcess`.
        std::process::abort()
    }
}

/// The stop signals, listened for from the call on: SIGTERM (`docker stop`, systemd,
/// Kubernetes) and SIGINT (ctrl-c). Resolves to the one that came first.
#[cfg(unix)]
fn stop_signals() -> std::io::Result<impl std::future::Future<Output = &'static str>> {
    use tokio::signal::unix::{SignalKind, signal};
    let mut terminate = signal(SignalKind::terminate())?;
    let mut interrupt = signal(SignalKind::interrupt())?;
    Ok(async move {
        tokio::select! {
            _ = terminate.recv() => "SIGTERM",
            _ = interrupt.recv() => "SIGINT",
        }
    })
}

/// The stop signal on Windows: ctrl-c in the server's console.
#[cfg(windows)]
fn stop_signals() -> std::io::Result<impl std::future::Future<Output = &'static str>> {
    let mut ctrl_c = tokio::signal::windows::ctrl_c()?;
    Ok(async move {
        ctrl_c.recv().await;
        "ctrl-c"
    })
}

async fn run() -> Result<()> {
    let _ = dotenvy::dotenv();
    let cli = CliConfig::from_args(std::env::args_os())?;
    init_tracing(&cli.command);

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
    let config = ServerConfig::load_with(cli.config_path.as_deref(), &cli.overrides)?;
    config.install_process_memory_policy()?;
    config.install_process_settings()?;
    // The default repository's settings as changed through the engine API (kept in the data
    // directory) are in effect for it: its reasoning and its store's modes. The configured
    // reasoning stays the server's, for the repositories that name none.
    let default = nrese_server::repositories::Effective::of_default(&config.store)
        .map_err(|error| anyhow::anyhow!(error))?;
    let default_reasoner = default
        .reasoning
        .clone()
        .unwrap_or_else(|| config.reasoner.clone());
    if default.reasoning.is_some() {
        tracing::info!(
            reasoning = ?default_reasoner.mode(),
            "the default repository's stored settings choose its reasoning"
        );
    }
    if cli.command == CliCommand::CheckConfig {
        // Loading validated everything; show what takes effect. `reasoner.mode` is the
        // server's, for the repositories that name none; the default repository's own, if its
        // stored settings choose one, is its line.
        print!("{}", config.summary());
        if default.reasoning.is_some() {
            println!(
                "default repository reasoning: {} (its stored settings)",
                default_reasoner.mode().as_str()
            );
        }
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
    default.apply(&store);
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
    let program = default_reasoner.materialised_program();
    if let CliCommand::Query(query) = cli.command {
        bring_inferences_up_to_date(&store, program.as_ref(), replica)?;
        return query_once(&store, query);
    }
    if let CliCommand::PrintQuery(print) = cli.command {
        // Its schema and its equality check read the asserted statements alone: no
        // inference to bring up to date.
        return print_query(&store, print);
    }
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
                    "the loaded data is inconsistent under {}: {}{} violation(s)",
                    program.name(),
                    if report.violations_at_least {
                        "at least "
                    } else {
                        ""
                    },
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
    bring_inferences_up_to_date(&store, program.as_ref(), replica)?;
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
    // Held for as long as the server serves; stopped when it ends.
    let follower = if replica {
        let follower = nrese_server::replication::follow(state.clone())?;
        tracing::info!(primary = ?config.replication.primary, "following the primary's log");
        Some(follower)
    } else {
        None
    };
    state.mark_ready();
    let app = build_app(state);

    let listener = TcpListener::bind(config.bind_address)
        .await
        .with_context(|| format!("failed to bind {}", config.bind_address))?;

    // The address bound, not the one configured: port 0 names the port the system chose.
    let bound_address = listener
        .local_addr()
        .with_context(|| format!("reading the address bound for {}", config.bind_address))?;
    // Before the server says it serves: a stop sent from then on is heard, and bounded.
    let stop = stop_signals().context("listening for stop signals")?;
    let deadline = StopDeadline::start().context("starting the stop's deadline")?;
    tracing::info!(
        bind_address = %bound_address,
        deployment_posture = config.deployment_posture.as_str(),
        data_dir = %store.config().data_dir.display(),
        reasoning_mode = ?reasoner.config().mode(),
        default_reasoning_mode = ?default_reasoner.mode(),
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
    // Peer addresses for the mtls mode's trusted proxies. On a stop signal it accepts no
    // more connections, closes idle ones, and lets the requests in flight finish, for up to
    // DRAIN; then the replica's follower stops and the store closes, all within
    // STOP_DEADLINE of the signal.
    let (stopping, mut stopped_by) = tokio::sync::oneshot::channel();
    let mut serve = Box::pin(std::future::IntoFuture::into_future(
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
        )
        .with_graceful_shutdown(async move {
            // The deadline runs from here, whichever way serving then ends.
            let signal = stop.await;
            deadline.arm(signal);
            let _ = stopping.send((signal, std::time::Instant::now()));
        }),
    ));
    let mut stopped = None;
    let served = tokio::select! {
        Ok((signal, started)) = &mut stopped_by => {
            let drained = tokio::time::timeout(DRAIN, &mut serve).await;
            stopped = Some((signal, started, drained.is_err()));
            drained.unwrap_or(Ok(()))
        }
        served = &mut serve => served,
    };
    // Serving can end in the same poll as the signal that ended it.
    let stopped = stopped.or_else(|| {
        let (signal, started) = stopped_by.try_recv().ok()?;
        Some((signal, started, false))
    });
    stop_step(1);
    if let Some(follower) = follower {
        follower.stop().await;
    }
    // The store closes here (its maintenance ends first) unless a request the drain cut
    // still holds it; then at the runtime's shutdown.
    stop_step(2);
    drop(serve);
    hold_the_close_for_a_test();
    drop(store);
    if let Some((signal, started, requests_cut)) = stopped {
        tracing::info!(
            signal,
            ms = started.elapsed().as_millis() as u64,
            requests_cut,
            "nrese-server stopped"
        );
    }
    served.context("nrese-server terminated unexpectedly")
}

/// Reasoner v2: brings the inferred stack in line with the configured rules, unless the
/// recorded state says it already is (same rules and semantics). Without reasoning, a
/// leftover stack is cleared so reads never see stale inferences. Before the first read of
/// whatever reads inferences: the server, and `query`.
fn bring_inferences_up_to_date(
    store: &StoreService,
    program: Option<&nrese_reasoner::RuleProgram>,
    replica: bool,
) -> Result<()> {
    match program {
        // A replica's inferences come with the primary's records.
        _ if replica => {
            tracing::info!("read replica: no reasoning here, the primary's log carries it");
        }
        Some(program) if store.reasoning_is_current(program) => {
            tracing::info!(ruleset = %program.name(), "inferred stack is current");
        }
        Some(program) => {
            store
                .rematerialise(program)
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
    Ok(())
}

/// `nrese-server query`: one query on the configured store, its inferences first brought up
/// to date ([`bring_inferences_up_to_date`]), its results on standard output.
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
        invalid: load.invalid,
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
        quarantined = report.quarantined,
        "bulk load complete"
    );
    if report.quarantined > 0 {
        eprintln!(
            "warning: {} statements could not be read and are quarantined (load {}), the first:",
            report.quarantined,
            report.load.as_deref().unwrap_or_default()
        );
        for example in &report.examples {
            eprintln!(
                "  {} line {}: {}",
                example.file, example.line, example.reason
            );
        }
    }
    Ok(())
}

/// Log lines go to standard error, except the server's: a command's standard output is its
/// result (`query`'s answers, `print-query`'s query, the configuration schema), which a log
/// line before it would corrupt. The server's stay on standard output, where its operators
/// read them.
fn init_tracing(command: &CliCommand) {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    let logs = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .compact();
    if *command == CliCommand::Serve {
        logs.init();
    } else {
        logs.with_writer(std::io::stderr).init();
    }
}
