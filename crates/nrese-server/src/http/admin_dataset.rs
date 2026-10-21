use axum::Json;
use axum::body::Bytes;
use axum::http::{HeaderMap, HeaderName, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use nrese_store::{
    DatasetBackupFormat, DatasetRestoreRequest, MutationCommand, MutationCommitReport, StoreError,
};

use crate::error::ApiError;
use crate::http::media::{header_value_str, media_type_matches};
use crate::http::mutation;
use crate::http::request_metrics::BackupKind;
use crate::http::responses::build_admin_restore_response;
use crate::http::result_stream::{Pace, stream_blocking};
use crate::state::AppState;

const BACKUP_FORMAT_HEADER: &str = "x-nrese-backup-format";
const SOURCE_REVISION_HEADER: &str = "x-nrese-source-revision";
const QUAD_COUNT_HEADER: &str = "x-nrese-quad-count";
/// A trailer of the export: its SHA-256, as its closing comment holds it.
const CHECKSUM_HEADER: &str = "x-nrese-checksum-sha256";

/// The N-Quads export of every asserted statement, streamed as it is read: a whole store in
/// constant memory, for as long as the client keeps reading. Its revision and statement
/// count go in the headers; its SHA-256 closes the body as a comment
/// (`# nrese-sha256 <hex>`) and follows it as the trailer [`CHECKSUM_HEADER`], announced in
/// `Trailer` (HTTP/1.1 clients send `TE: trailers` to receive it).
pub async fn backup(state: AppState) -> Result<Response, ApiError> {
    state.ensure_serving()?;
    // Administrators only (the caller's guard): every graph.
    let export = state
        .store()
        .dataset_export(&nrese_store::ReadScope::All, DatasetBackupFormat::NQuads)
        .map_err(map_backup_error)?;
    let (revision, quads) = (export.source_revision, export.quad_count);
    let cancellation = nrese_store::CancellationToken::new();
    let token = cancellation.clone();
    let metrics = state.clone();
    let started = std::time::Instant::now();
    let mut response = stream_blocking(
        Pace::stalled_after(state.policy().timeouts.graph_read),
        cancellation,
        "application/n-quads",
        "the export's client stopped reading (timeouts.graph_read)",
        move |out| {
            let mut counted = Counting(&mut *out, 0);
            let written = export.write(&token, &mut counted);
            let bytes = counted.1;
            metrics.request_metrics().record_backup(
                BackupKind::Dump,
                written.as_ref().ok().map(|_| bytes),
                started.elapsed(),
            );
            let checksum = written.map_err(map_backup_error)?;
            let mut trailers = HeaderMap::new();
            insert_header(&mut trailers, CHECKSUM_HEADER, &checksum)?;
            out.trailers(trailers)
                .map_err(|error| ApiError::internal(error.to_string()))
        },
    )
    .await?;
    let headers = response.headers_mut();
    insert_header(headers, BACKUP_FORMAT_HEADER, "n-quads")?;
    insert_header(headers, SOURCE_REVISION_HEADER, &revision.to_string())?;
    insert_header(headers, QUAD_COUNT_HEADER, &quads.to_string())?;
    headers.insert(header::TRAILER, HeaderValue::from_static(CHECKSUM_HEADER));
    Ok(response)
}

/// A writer that counts the bytes it passes on.
struct Counting<W>(W, u64);

impl<W: std::io::Write> std::io::Write for Counting<W> {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        let written = self.0.write(bytes)?;
        self.1 += written as u64;
        Ok(written)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.0.flush()
    }
}

pub async fn restore(
    state: AppState,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, ApiError> {
    state.ensure_serving()?;
    let format = parse_restore_format(headers.get(header::CONTENT_TYPE))?;
    let request = DatasetRestoreRequest {
        format,
        payload: body.to_vec(),
    };

    let started = std::time::Instant::now();
    let result = mutation::run(
        &state,
        MutationCommand::Restore(request),
        // Administrators only (the caller's guard): every graph.
        nrese_store::Requester::all(),
        state.policy().timeouts.update,
        "dataset restore exceeded policy timeout",
    )
    .await;
    state.request_metrics().record_backup(
        BackupKind::Restore,
        result.as_ref().ok().map(|_| body.len() as u64),
        started.elapsed(),
    );
    let report = match result? {
        MutationCommitReport::Restore(report) => report,
        other => {
            return Err(ApiError::internal(format!(
                "unexpected restore result: {other:?}"
            )));
        }
    };

    Ok((StatusCode::OK, Json(build_admin_restore_response(&report))).into_response())
}

/// An image backup of repository `repository`'s latest snapshot into `backups/` of the
/// default repository's data directory (`<seconds since 1970>`, or `<id>-<seconds>` for
/// another repository), while writers go on; answers its manifest and directory.
pub async fn image_backup(state: AppState, repository: &str) -> Result<Response, ApiError> {
    state.ensure_serving()?;
    let backups = state.store().config().data_dir.join("backups");
    let store = state.for_repository(repository)?.store();
    let seconds = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| since.as_secs());
    let name = match repository {
        crate::repositories::DEFAULT_REPOSITORY => seconds.to_string(),
        other => format!("{other}-{seconds}"),
    };
    let started = std::time::Instant::now();
    let written = tokio::task::spawn_blocking(move || {
        let dir = claim_backup_dir(&backups, &name)
            .map_err(|error| ApiError::internal(format!("{}: {error}", backups.display())))?;
        let manifest = store.backup_image(&dir).map_err(map_backup_error)?;
        Ok::<_, ApiError>((dir, manifest))
    })
    .await
    .map_err(|error| ApiError::internal(error.to_string()))
    .and_then(|result| result);
    let (dir, manifest) = match written {
        Ok((dir, manifest)) => (Some(dir), Ok(manifest)),
        Err(error) => (None, Err(error)),
    };
    state.request_metrics().record_backup(
        BackupKind::Image,
        manifest.as_ref().ok().map(|manifest| manifest.bytes),
        started.elapsed(),
    );
    let manifest = manifest?;
    let dir = dir.unwrap_or_default();
    Ok((
        StatusCode::OK,
        Json(serde_json::json!({
            "directory": dir.display().to_string(),
            "manifest": manifest,
        })),
    )
        .into_response())
}

/// A new, empty directory for a backup in `backups`: `name`, or `name-1`, `name-2`, … where
/// it is taken (two backups within a second; creating the directory claims it, so
/// concurrent backups can't share one).
fn claim_backup_dir(backups: &std::path::Path, name: &str) -> std::io::Result<std::path::PathBuf> {
    std::fs::create_dir_all(backups)?;
    for n in 0u32.. {
        let dir = if n == 0 {
            backups.join(name)
        } else {
            backups.join(format!("{name}-{n}"))
        };
        match std::fs::create_dir(&dir) {
            Ok(()) => return Ok(dir),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists && n < 10_000 => {}
            Err(error) => return Err(error),
        }
    }
    unreachable!("the loop returns")
}

fn parse_restore_format(
    content_type: Option<&HeaderValue>,
) -> Result<DatasetBackupFormat, ApiError> {
    let Some(content_type) = header_value_str(content_type) else {
        return Err(ApiError::bad_request(
            "restore content type must be application/n-quads",
        ));
    };

    if media_type_matches(Some(content_type), "application/n-quads") {
        Ok(DatasetBackupFormat::NQuads)
    } else {
        Err(ApiError::bad_request(format!(
            "unsupported restore content type: {content_type}"
        )))
    }
}

fn map_backup_error(error: StoreError) -> ApiError {
    ApiError::internal(error.to_string())
}

fn insert_header(
    headers: &mut axum::http::HeaderMap,
    name: &'static str,
    value: &str,
) -> Result<(), ApiError> {
    let header_name = HeaderName::from_static(name);
    let header_value =
        HeaderValue::from_str(value).map_err(|error| ApiError::internal(error.to_string()))?;
    headers.insert(header_name, header_value);
    Ok(())
}

#[cfg(test)]
mod tests {
    use axum::http::HeaderValue;
    use nrese_store::DatasetBackupFormat;

    use super::parse_restore_format;

    #[test]
    fn restore_format_accepts_parameterized_nquads_content_type() {
        let value = HeaderValue::from_static("application/n-quads; charset=utf-8");
        let format = parse_restore_format(Some(&value)).expect("format should parse");
        assert_eq!(format, DatasetBackupFormat::NQuads);
    }
}
