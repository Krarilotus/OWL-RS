//! Read replicas (hardware plan H6, step 1; docs/ops/replication.md): one server takes the
//! writes and ships its write-ahead log, others apply it and answer queries.
//!
//! - **The primary** (`replication.mode = "primary"`, an on-disk store) serves its log
//!   after a revision ([`log`]) and an image of its latest revision ([`image`]), to
//!   administrators.
//! - **A replica** (`"replica"`, with `replication.primary` and `replication.token`) starts
//!   from the primary's image when its data directory holds no store ([`bootstrap`]), then
//!   follows the log ([`follow`]): each record a commit here with the primary's revision.
//!   It takes no writes (the write surfaces are off, as in the read-only posture) and
//!   doesn't reason (the records carry the inferences).
//! - **Status:** [`status`] on either side, and `nrese_replication_lag_revisions` in the
//!   metrics.
//!
//! A replica behind what the primary's log holds (a checkpoint covered it, or a bulk load
//! or rematerialisation wrote no records) can't catch up from the log: its status says so,
//! and it starts again from an empty data directory. The primary keeps its log for
//! replicas that fall behind with `storage.wal_archive = true`.

use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use axum::Json;
use axum::body::Body;
use axum::extract::{Query, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};

use crate::error::ApiError;
use crate::http::guard;
use crate::state::AppState;

const LAST: &str = "x-nrese-last-revision";
const LATEST: &str = "x-nrese-latest-revision";
const RECORDS: &str = "x-nrese-records";
const MANIFEST: &str = "x-nrese-image-manifest";
const LOG_TYPE: &str = "application/vnd.nrese.log";

/// This server's part in replication.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ReplicationMode {
    #[default]
    Off,
    Primary,
    Replica,
}

impl ReplicationMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Primary => "primary",
            Self::Replica => "replica",
        }
    }
}

/// `replication.*`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReplicationConfig {
    pub mode: ReplicationMode,
    /// The primary's base URL (a replica's).
    pub primary: Option<String>,
    /// The bearer token a replica presents to the primary (an administrator's).
    pub token: Option<String>,
    /// How long a replica waits before asking again when it has caught up.
    pub poll: Duration,
    /// About how many bytes of log a replica asks for at once.
    pub batch_bytes: usize,
    /// The longest one log round may take, its answer whole (a primary sending a byte at a
    /// time doesn't extend it); `None`: no total limit, the limits between pieces stay.
    pub round_timeout: Option<Duration>,
    /// The longest a replica waits for its primary to prepare an image (the primary writes
    /// and checks it before its answer begins); `None`: no limit. The image then comes as
    /// long as its pieces come.
    pub image_timeout: Option<Duration>,
}

impl Default for ReplicationConfig {
    fn default() -> Self {
        Self {
            mode: ReplicationMode::Off,
            primary: None,
            token: None,
            poll: Duration::from_millis(500),
            batch_bytes: 8 << 20,
            round_timeout: Some(Duration::from_secs(300)),
            image_timeout: Some(Duration::from_secs(3600)),
        }
    }
}

/// What this server knows of its replication, for [`status`] and the metrics.
#[derive(Debug, Default)]
pub struct ReplicationState {
    pub config: ReplicationConfig,
    primary_revision: AtomicU64,
    records: AtomicU64,
    last_contact_unix: AtomicU64,
    last_error: Mutex<Option<String>>,
}

/// `GET /api/v1/replication/status`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, utoipa::ToSchema)]
pub struct ReplicationStatus {
    /// `off`, `primary` or `replica`.
    pub mode: String,
    /// The primary followed (a replica's).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub primary: Option<String>,
    /// This server's latest revision.
    pub revision: u64,
    /// The primary's latest revision when last asked (a replica's).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub primary_revision: Option<u64>,
    /// Revisions this replica is behind the primary, as last seen.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lag_revisions: Option<u64>,
    /// Records applied since this server started.
    pub records_applied: u64,
    /// When the primary last answered, seconds since 1970.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_contact_unix: Option<u64>,
    /// What went wrong at the last attempt, if it did.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
}

fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

impl ReplicationState {
    pub fn new(config: ReplicationConfig) -> Self {
        Self {
            config,
            ..Self::default()
        }
    }

    pub fn is_replica(&self) -> bool {
        self.config.mode == ReplicationMode::Replica
    }

    /// The status with this server's revision `revision`.
    pub fn status(&self, revision: u64) -> ReplicationStatus {
        let replica = self.is_replica();
        let contact = self.last_contact_unix.load(Ordering::Acquire);
        let primary_revision =
            (replica && contact > 0).then(|| self.primary_revision.load(Ordering::Acquire));
        ReplicationStatus {
            mode: self.config.mode.as_str().to_owned(),
            primary: self.config.primary.clone().filter(|_| replica),
            revision,
            primary_revision,
            lag_revisions: primary_revision.map(|p| p.saturating_sub(revision)),
            records_applied: self.records.load(Ordering::Acquire),
            last_contact_unix: (contact > 0).then_some(contact),
            last_error: self.last_error.lock().clone(),
        }
    }

    fn contact(&self, primary_revision: u64, records: usize, error: Option<String>) {
        self.primary_revision
            .store(primary_revision, Ordering::Release);
        self.records.fetch_add(records as u64, Ordering::AcqRel);
        self.last_contact_unix.store(now_unix(), Ordering::Release);
        *self.last_error.lock() = error;
    }

    fn failed(&self, error: String) {
        *self.last_error.lock() = Some(error);
    }
}

// The primary's side ---------------------------------------------------------------------

#[derive(Debug, Deserialize, utoipa::IntoParams)]
pub struct LogQuery {
    /// The last revision the replica has.
    pub after: u64,
    /// About how many bytes to send at most (at least one record); default 8 MiB.
    pub max_bytes: Option<usize>,
}

fn primary_only(state: &AppState) -> Result<(), ApiError> {
    match state.replication().config.mode {
        ReplicationMode::Primary => Ok(()),
        _ => Err(ApiError::not_found(
            "replication.mode isn't \"primary\": this server ships no log",
        )),
    }
}

#[utoipa::path(get, path = "/api/v1/replication/log", tag = "replication", params(LogQuery),
    responses((status = 200, description = "The committed records after `after`, back to back (`application/vnd.nrese.log`); headers `x-nrese-records`, `x-nrese-last-revision`, `x-nrese-latest-revision`"),
        (status = 404, description = "This server isn't a primary", body = crate::http::openapi::Problem),
        (status = 410, description = "The log no longer holds the records after `after`: start from an image", body = crate::http::openapi::Problem)))]
/// A replica's feed: the committed records after revision `after`, from the write-ahead
/// log. Administrators only; a primary's.
pub async fn log(
    authenticated: crate::auth::Authenticated,
    State(state): State<AppState>,
    Query(query): Query<LogQuery>,
) -> Result<Response, ApiError> {
    primary_only(&state)?;
    guard::enforce_admin_write(&state, &authenticated).await?;
    let store = state.store();
    let max = query.max_bytes.unwrap_or(8 << 20).clamp(1 << 10, 256 << 20);
    let batch = tokio::task::spawn_blocking(move || store.replication_log(query.after, max))
        .await
        .map_err(|error| ApiError::internal(error.to_string()))?;
    let batch = match batch {
        Ok(batch) => batch,
        Err(nrese_store::StoreError::Engine(nrese_store::EngineError::LogTruncated { oldest })) => {
            return Err(ApiError::gone(format!(
                "the log holds no records after revision {} (from {oldest} on at the \
                     earliest): start the replica from an image",
                query.after
            )));
        }
        Err(error) => return Err(ApiError::internal(error.to_string())),
    };
    let mut response = (StatusCode::OK, batch.frames).into_response();
    let headers = response.headers_mut();
    headers.insert(header::CONTENT_TYPE, HeaderValue::from_static(LOG_TYPE));
    for (name, value) in [
        (RECORDS, batch.records as u64),
        (LAST, batch.last),
        (LATEST, batch.latest),
    ] {
        headers.insert(name, HeaderValue::from(value));
    }
    Ok(response)
}

#[utoipa::path(get, path = "/api/v1/replication/image", tag = "replication",
    responses((status = 200, description = "An image of the latest revision (a checkpoint file); its manifest, as JSON, in `x-nrese-image-manifest`"),
        (status = 404, description = "This server isn't a primary", body = crate::http::openapi::Problem)))]
/// A replica's start: an image of the latest revision, streamed. Administrators only; a
/// primary's.
pub async fn image(
    authenticated: crate::auth::Authenticated,
    State(state): State<AppState>,
) -> Result<Response, ApiError> {
    primary_only(&state)?;
    guard::enforce_admin_write(&state, &authenticated).await?;
    let store = state.store();
    let (dir, manifest) = tokio::task::spawn_blocking(move || {
        let dir =
            tempfile::tempdir_in(&store.config().data_dir).or_else(|_| tempfile::tempdir())?;
        let manifest = store
            .backup_image(dir.path())
            .map_err(|error| std::io::Error::other(error.to_string()))?;
        Ok::<_, std::io::Error>((dir, manifest))
    })
    .await
    .map_err(|error| ApiError::internal(error.to_string()))?
    .map_err(|error| ApiError::internal(error.to_string()))?;
    let path = dir.path().join(&manifest.file);
    let manifest_json =
        serde_json::to_string(&manifest).map_err(|error| ApiError::internal(error.to_string()))?;
    // Read in pieces on a thread of its own; the directory goes when the body is sent.
    let (tx, rx) = tokio::sync::mpsc::channel::<Result<Vec<u8>, std::io::Error>>(4);
    std::thread::spawn(move || {
        use std::io::Read;
        let _dir = dir;
        let mut file = match std::fs::File::open(&path) {
            Ok(file) => file,
            Err(error) => {
                let _ = tx.blocking_send(Err(error));
                return;
            }
        };
        loop {
            let mut chunk = vec![0u8; 1 << 20];
            match file.read(&mut chunk) {
                Ok(0) => return,
                Ok(n) => {
                    chunk.truncate(n);
                    if tx.blocking_send(Ok(chunk)).is_err() {
                        return;
                    }
                }
                Err(error) => {
                    let _ = tx.blocking_send(Err(error));
                    return;
                }
            }
        }
    });
    let stream = futures_util::stream::unfold(rx, |mut rx| async move {
        rx.recv().await.map(|item| (item, rx))
    });
    let mut response = Response::new(Body::from_stream(stream));
    let headers = response.headers_mut();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/octet-stream"),
    );
    headers.insert(
        MANIFEST,
        HeaderValue::from_str(&manifest_json)
            .map_err(|error| ApiError::internal(error.to_string()))?,
    );
    Ok(response)
}

#[utoipa::path(get, path = "/api/v1/replication/status", tag = "replication",
    responses((status = 200, description = "This server's part in replication and, on a replica, how far behind it is", body = ReplicationStatus)))]
/// This server's replication: its mode, its revision and, on a replica, the primary's
/// revision when last asked, the lag, and the last error.
pub async fn status(
    authenticated: crate::auth::Authenticated,
    State(state): State<AppState>,
) -> Result<Json<ReplicationStatus>, ApiError> {
    guard::enforce_operator_read(&state, &authenticated).await?;
    let revision = state.store().current_revision();
    Ok(Json(state.replication().status(revision)))
}

// The replica's side ---------------------------------------------------------------------

fn client(config: &ReplicationConfig) -> anyhow::Result<(reqwest::Client, String)> {
    let primary = config
        .primary
        .as_deref()
        .map(|p| p.trim_end_matches('/').to_owned())
        .ok_or_else(|| {
            anyhow::anyhow!("replication.mode = \"replica\" needs replication.primary")
        })?;
    // No redirects: the replica's bearer token (an administrator's) would follow one, and
    // reqwest keeps Authorization when only the scheme changes (https://p:8443 ->
    // http://p:8443 sends it in clear). `replication.primary` names the primary itself.
    let client = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(10))
        .redirect(reqwest::redirect::Policy::none())
        .build()?;
    Ok((client, primary))
}

// Every exchange with the primary ends: its answer begins within a limit, and its body
// pauses at most `IDLE` between pieces. A log round as a whole takes at most
// `replication.round_timeout`; an image comes as long as its pieces come, however large,
// after the primary has prepared it (`replication.image_timeout`).

/// The longest a replica waits for a log answer to begin (its headers).
const HEADERS: Duration = Duration::from_secs(30);
/// The longest an answer's body may pause between two pieces.
const IDLE: Duration = Duration::from_secs(30);

/// `request` sent; an error if its answer hasn't begun within `wait` (`None`: no limit).
async fn begin(
    request: reqwest::RequestBuilder,
    wait: Option<Duration>,
) -> anyhow::Result<reqwest::Response> {
    let Some(wait) = wait else {
        return Ok(request.send().await?);
    };
    match tokio::time::timeout(wait, request.send()).await {
        Ok(response) => Ok(response?),
        Err(_) => anyhow::bail!("the primary didn't answer within {} s", wait.as_secs()),
    }
}

/// The next piece of `response`'s body (`None` at its end); an error if none comes within
/// [`IDLE`].
async fn piece(response: &mut reqwest::Response) -> anyhow::Result<Option<axum::body::Bytes>> {
    match tokio::time::timeout(IDLE, response.chunk()).await {
        Ok(piece) => Ok(piece?),
        Err(_) => anyhow::bail!("the primary stopped sending for {} s", IDLE.as_secs()),
    }
}

/// An error answer's text, as far as it comes within [`IDLE`].
async fn error_text(response: reqwest::Response) -> String {
    tokio::time::timeout(IDLE, response.text())
        .await
        .ok()
        .and_then(Result::ok)
        .unwrap_or_default()
}

fn authorised(
    request: reqwest::RequestBuilder,
    config: &ReplicationConfig,
) -> reqwest::RequestBuilder {
    match &config.token {
        Some(token) => request.bearer_auth(token),
        None => request,
    }
}

/// What a replica's download of the primary's image is called while it comes, in a
/// directory of its own.
const STAGED_IMAGE: &str = "image.download";

/// A replica's start: if `data_dir` holds no store, the primary's image placed there.
/// Returns the image's revision, or `None` if a store was already there (the replica
/// follows the log from its own revision).
pub async fn bootstrap(config: &ReplicationConfig, data_dir: &Path) -> anyhow::Result<Option<u64>> {
    use anyhow::Context;
    let holds_store = std::fs::read_dir(data_dir).is_ok_and(|entries| {
        entries.flatten().any(|entry| {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            name.ends_with(".nck") || name == "wal"
        })
    });
    if holds_store {
        return Ok(None);
    }
    let (client, primary) = client(config)?;
    // The primary writes and checks the image before its answer begins.
    let response = begin(
        authorised(
            client.get(format!("{primary}/api/v1/replication/image")),
            config,
        ),
        config.image_timeout,
    )
    .await
    .with_context(|| format!("asking {primary} for an image"))?;
    if !response.status().is_success() {
        anyhow::bail!(
            "{primary} answered {} to the image request: {}",
            response.status(),
            error_text(response).await
        );
    }
    let manifest: nrese_store::ImageManifest = serde_json::from_str(
        response
            .headers()
            .get(MANIFEST)
            .and_then(|v| v.to_str().ok())
            .ok_or_else(|| anyhow::anyhow!("the image came without its manifest"))?,
    )?;
    // The name comes from the primary: checked before anything, and the download staged
    // under a name of this side's.
    manifest
        .image_name()
        .map_err(|error| anyhow::anyhow!("{primary}: {error}"))?;
    std::fs::create_dir_all(data_dir)?;
    let incoming = tempfile::tempdir_in(data_dir)?;
    let staged = incoming.path().join(STAGED_IMAGE);
    {
        use std::io::Write;
        let mut file = std::fs::File::create(&staged)?;
        let mut response = response;
        while let Some(chunk) = piece(&mut response).await? {
            file.write_all(&chunk)?;
        }
        file.sync_all()?;
    }
    // Checks the image against its manifest and places it.
    let placed = nrese_store::image_backup::place_image(&staged, manifest, data_dir)
        .map_err(|error| anyhow::anyhow!(error.to_string()))?;
    Ok(Some(placed.revision))
}

/// One round of following: asks the primary for the records after this store's revision
/// and applies them. Returns how many were applied.
pub async fn pull_once(
    client: &reqwest::Client,
    primary: &str,
    state: &AppState,
) -> anyhow::Result<usize> {
    round(client, primary, state, &Applying::default()).await
}

/// A follower's application of the records it receives: what stops it (checked between
/// records) and what is held while it runs on its own thread.
#[derive(Clone, Default)]
struct Applying {
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
    /// Held by the thread applying a batch until it ends: waiting for it waits for that.
    running: std::sync::Arc<tokio::sync::Mutex<()>>,
}

/// [`pull_once`], its application owned by `applying`.
async fn round(
    client: &reqwest::Client,
    primary: &str,
    state: &AppState,
    applying: &Applying,
) -> anyhow::Result<usize> {
    let replication = state.replication();
    let config = &replication.config;
    let after = state.store().current_revision();
    let url = format!(
        "{primary}/api/v1/replication/log?after={after}&max_bytes={}",
        config.batch_bytes
    );
    let round = async {
        let mut response = begin(authorised(client.get(url), config), Some(HEADERS)).await?;
        let status = response.status();
        if status == reqwest::StatusCode::GONE {
            anyhow::bail!(
                "the primary's log no longer holds revision {}: start this replica again from \
                 an empty data directory (the primary keeps its log for replicas with \
                 storage.wal_archive = true)",
                after.saturating_add(1)
            );
        }
        if !status.is_success() {
            anyhow::bail!(
                "the primary answered {status}: {}",
                error_text(response).await
            );
        }
        let progress = Progress::read(response.headers(), after)?;
        let mut frames = Vec::new();
        while let Some(chunk) = piece(&mut response).await? {
            frames.extend_from_slice(&chunk);
        }
        Ok::<_, anyhow::Error>((progress, frames))
    };
    let (progress, frames) = match config.round_timeout {
        None => round.await?,
        Some(limit) => tokio::time::timeout(limit, round).await.map_err(|_| {
            anyhow::anyhow!(
                "the log round took longer than replication.round_timeout ({} s)",
                limit.as_secs()
            )
        })??,
    };
    // The store checks the whole answer against what it says before it applies any of it.
    let expected = nrese_store::LogExpectation {
        after,
        records: progress.records,
        last: progress.last,
    };
    let store = state.store();
    let stop = nrese_exec::stop::StopSource::new(Some(applying.stop.clone()), None);
    let running = applying.running.clone().lock_owned().await;
    let applied = tokio::task::spawn_blocking(move || {
        let applied = store.apply_replication_log(&frames, &expected, &stop);
        // The store goes before `running` does: who waits for `running` (a stop) finds
        // the store let go of.
        drop(store);
        drop(running);
        applied
    })
    .await??;
    let published = applied.revision.saturating_sub(after) as usize;
    if applied.stopped {
        anyhow::bail!(
            "stopped at revision {} of {}",
            applied.revision,
            progress.last
        );
    }
    replication.contact(progress.latest, published, None);
    Ok(published)
}

/// What a log answer says it carries: its record count, the revision of its last record
/// (the asked-for one without records) and the primary's latest revision.
struct Progress {
    records: u64,
    last: u64,
    latest: u64,
}

impl Progress {
    /// The progress `headers` report for the records after `after`; an error if one is
    /// missing or not a number, or if the primary's latest revision contradicts them.
    /// Whether the records are what the answer says is the store's to check, before it
    /// applies any ([`nrese_store::StoreService::apply_replication_log`]).
    fn read(headers: &HeaderMap, after: u64) -> anyhow::Result<Self> {
        let header = |name: &str| {
            headers
                .get(name)
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.parse::<u64>().ok())
                .ok_or_else(|| anyhow::anyhow!("the primary's answer has no number in {name}"))
        };
        let progress = Self {
            records: header(RECORDS)?,
            last: header(LAST)?,
            latest: header(LATEST)?,
        };
        if progress.last > progress.latest || progress.latest < after {
            anyhow::bail!(
                "the primary's answer after revision {after} says {} records, the last at \
                 revision {}, its latest {}",
                progress.records,
                progress.last,
                progress.latest
            );
        }
        Ok(progress)
    }
}

/// Follows the primary's log until the returned [`Follower`] is stopped or dropped: asks
/// again at once while records come, after `replication.poll` once caught up, and after a
/// growing pause on errors (reported in the status).
pub fn follow(state: AppState) -> anyhow::Result<Follower> {
    let (client, primary) = client(&state.replication().config)?;
    let applying = Applying::default();
    let owned = applying.clone();
    let task = tokio::spawn(async move {
        let poll = state.replication().config.poll;
        let mut pause = poll;
        loop {
            match round(&client, &primary, &state, &owned).await {
                Ok(0) => {
                    pause = poll;
                    tokio::time::sleep(poll).await;
                }
                Ok(_) => pause = poll,
                Err(error) => {
                    tracing::warn!(%error, "replication");
                    state.replication().failed(error.to_string());
                    tokio::time::sleep(pause).await;
                    pause = (pause * 2).min(Duration::from_secs(60));
                }
            }
        }
    });
    Ok(Follower { task, applying })
}

/// The task [`follow`] runs and the application it starts, with the server state and the
/// store they hold. Stopped (or dropped), the task ends at its next await and a batch being
/// applied ends at its next record: no record after it is published.
pub struct Follower {
    task: tokio::task::JoinHandle<()>,
    applying: Applying,
}

impl Follower {
    /// Stops the task and the application, and waits until both have ended and let go of
    /// what they held (the store, and with it its directory's lock).
    pub async fn stop(mut self) {
        self.applying
            .stop
            .store(true, std::sync::atomic::Ordering::Release);
        self.task.abort();
        let _ = (&mut self.task).await;
        // The thread applying a batch holds this until it has ended.
        let _ = self.applying.running.lock().await;
    }
}

impl Drop for Follower {
    /// Stops both without waiting: the application ends at its next record.
    fn drop(&mut self) {
        self.applying
            .stop
            .store(true, std::sync::atomic::Ordering::Release);
        self.task.abort();
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use axum::http::{HeaderMap, StatusCode, header};
    use axum::response::IntoResponse;

    use super::*;

    /// A listener that records the Authorization of every request, answering 500; its URL.
    async fn recorder(seen: &Arc<Mutex<Vec<Option<String>>>>) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let record = Arc::clone(seen);
        tokio::spawn(async move {
            let app = axum::Router::new().fallback(move |headers: HeaderMap| {
                let record = Arc::clone(&record);
                async move {
                    let bearer = headers
                        .get(header::AUTHORIZATION)
                        .map(|value| format!("{value:?}"));
                    record.lock().unwrap().push(bearer);
                    StatusCode::INTERNAL_SERVER_ERROR
                }
            });
            axum::serve(listener, app).await.unwrap();
        });
        url
    }

    /// A primary that answers every request with a redirect to `target`; its URL.
    async fn redirecting(target: String) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move {
            let app = axum::Router::new().fallback(move || {
                let target = target.clone();
                async move {
                    (StatusCode::TEMPORARY_REDIRECT, [(header::LOCATION, target)]).into_response()
                }
            });
            axum::serve(listener, app).await.unwrap();
        });
        url
    }

    /// A replica follows no redirect, so its token reaches only the configured primary:
    /// neither the bootstrap request nor the client the log requests use (`follow`) goes
    /// where a redirect points. (A redirect that only downgrades the scheme would keep
    /// Authorization; refusing every redirect covers it without TLS in the test.)
    #[tokio::test]
    async fn a_replica_sends_its_token_nowhere_a_redirect_points() {
        let seen: Arc<Mutex<Vec<Option<String>>>> = Arc::default();
        let elsewhere = recorder(&seen).await;
        let primary = redirecting(format!("{elsewhere}/")).await;
        let config = ReplicationConfig {
            mode: ReplicationMode::Replica,
            primary: Some(primary.clone()),
            token: Some("replica-token".to_owned()),
            ..ReplicationConfig::default()
        };
        let data = tempfile::tempdir().unwrap();
        assert!(bootstrap(&config, data.path()).await.is_err());
        let (client, base) = client(&config).unwrap();
        let log = authorised(
            client.get(format!("{base}/api/v1/replication/log?after=0")),
            &config,
        )
        .send()
        .await
        .unwrap();
        assert_eq!(log.status(), StatusCode::TEMPORARY_REDIRECT);
        assert!(
            seen.lock().unwrap().is_empty(),
            "{:?}",
            seen.lock().unwrap()
        );
    }
}
