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
}

impl Default for ReplicationConfig {
    fn default() -> Self {
        Self {
            mode: ReplicationMode::Off,
            primary: None,
            token: None,
            poll: Duration::from_millis(500),
            batch_bytes: 8 << 20,
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
        Err(nrese_store::StoreError::Engine(nrese_engine::EngineError::LogTruncated {
            oldest,
        })) => {
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
    let client = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(10))
        .build()?;
    Ok((client, primary))
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
    let response = authorised(
        client.get(format!("{primary}/api/v1/replication/image")),
        config,
    )
    .send()
    .await
    .with_context(|| format!("asking {primary} for an image"))?;
    if !response.status().is_success() {
        anyhow::bail!(
            "{primary} answered {} to the image request: {}",
            response.status(),
            response.text().await.unwrap_or_default()
        );
    }
    let manifest: nrese_store::ImageManifest = serde_json::from_str(
        response
            .headers()
            .get(MANIFEST)
            .and_then(|v| v.to_str().ok())
            .ok_or_else(|| anyhow::anyhow!("the image came without its manifest"))?,
    )?;
    std::fs::create_dir_all(data_dir)?;
    let incoming = tempfile::tempdir_in(data_dir)?;
    {
        use std::io::Write;
        let mut file = std::fs::File::create(incoming.path().join(&manifest.file))?;
        let mut response = response;
        while let Some(chunk) = response.chunk().await? {
            file.write_all(&chunk)?;
        }
        file.sync_all()?;
    }
    std::fs::write(
        incoming.path().join(nrese_store::image_backup::MANIFEST),
        serde_json::to_vec_pretty(&manifest)?,
    )?;
    // Checks the image against its manifest and places it.
    let placed = nrese_store::restore_image(incoming.path(), data_dir)
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
    let replication = state.replication();
    let config = &replication.config;
    let after = state.store().current_revision();
    let url = format!(
        "{primary}/api/v1/replication/log?after={after}&max_bytes={}",
        config.batch_bytes
    );
    let response = authorised(client.get(url), config).send().await?;
    let status = response.status();
    if status == reqwest::StatusCode::GONE {
        anyhow::bail!(
            "the primary's log no longer holds revision {}: start this replica again from \
             an empty data directory (the primary keeps its log for replicas with \
             storage.wal_archive = true)",
            after + 1
        );
    }
    if !status.is_success() {
        anyhow::bail!(
            "the primary answered {status}: {}",
            response.text().await.unwrap_or_default()
        );
    }
    let header = |name: &str| {
        response
            .headers()
            .get(name)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse::<u64>().ok())
    };
    let (records, latest) = (
        header(RECORDS).unwrap_or(0),
        header(LATEST).unwrap_or(after),
    );
    let frames = response.bytes().await?;
    if records > 0 {
        let store = state.store();
        tokio::task::spawn_blocking(move || store.apply_replication_log(&frames)).await??;
    }
    replication.contact(latest, records as usize, None);
    Ok(records as usize)
}

/// Follows the primary's log until the server stops: asks again at once while records
/// come, after `replication.poll` once caught up, and after a growing pause on errors
/// (reported in the status).
pub fn follow(state: AppState) -> anyhow::Result<tokio::task::JoinHandle<()>> {
    let (client, primary) = client(&state.replication().config)?;
    Ok(tokio::spawn(async move {
        let poll = state.replication().config.poll;
        let mut pause = poll;
        loop {
            match pull_once(&client, &primary, &state).await {
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
    }))
}

/// The headers a log answer carries, for tests.
pub fn log_headers(headers: &HeaderMap) -> (u64, u64, u64) {
    let get = |name: &str| {
        headers
            .get(name)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse().ok())
            .unwrap_or(0)
    };
    (get(RECORDS), get(LAST), get(LATEST))
}
