use axum::Json;
use axum::body::Bytes;
use axum::http::{HeaderMap, HeaderName, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use nrese_store::{DatasetBackupFormat, DatasetRestoreRequest, StoreError};

use crate::error::ApiError;
use crate::http::media::{header_value_str, media_type_matches};
use crate::http::responses::build_admin_restore_response;
use crate::state::AppState;

const BACKUP_FORMAT_HEADER: &str = "x-nrese-backup-format";
const SOURCE_REVISION_HEADER: &str = "x-nrese-source-revision";
const QUAD_COUNT_HEADER: &str = "x-nrese-quad-count";
const CHECKSUM_HEADER: &str = "x-nrese-checksum-sha256";

pub async fn backup(state: AppState) -> Result<Response, ApiError> {
    ensure_ready(&state)?;

    let store = state.store();
    let artifact =
        tokio::task::spawn_blocking(move || store.export_dataset(DatasetBackupFormat::NQuads))
            .await
            .map_err(|error| ApiError::internal(error.to_string()))?
            .map_err(map_backup_error)?;

    let mut response = (StatusCode::OK, artifact.payload).into_response();
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/n-quads"),
    );
    insert_header(response.headers_mut(), BACKUP_FORMAT_HEADER, "n-quads")?;
    insert_header(
        response.headers_mut(),
        SOURCE_REVISION_HEADER,
        &artifact.source_revision.to_string(),
    )?;
    insert_header(
        response.headers_mut(),
        QUAD_COUNT_HEADER,
        &artifact.quad_count.to_string(),
    )?;
    insert_header(
        response.headers_mut(),
        CHECKSUM_HEADER,
        &artifact.checksum_sha256,
    )?;

    Ok(response)
}

pub async fn restore(
    state: AppState,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, ApiError> {
    ensure_ready(&state)?;
    let format = parse_restore_format(headers.get(header::CONTENT_TYPE))?;
    let request = DatasetRestoreRequest {
        format,
        payload: body.to_vec(),
    };

    let store = state.store();
    let report = tokio::task::spawn_blocking(move || store.restore_dataset(&request))
        .await
        .map_err(|error| ApiError::internal(error.to_string()))?
        .map_err(map_restore_error)?;

    Ok((StatusCode::OK, Json(build_admin_restore_response(&report))).into_response())
}

fn ensure_ready(state: &AppState) -> Result<(), ApiError> {
    if state.is_ready() {
        Ok(())
    } else {
        Err(ApiError::unavailable("server is not ready yet"))
    }
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

fn map_restore_error(error: StoreError) -> ApiError {
    match error {
        StoreError::Loader(_) | StoreError::RdfParse(_) => ApiError::bad_request(error.to_string()),
        other => ApiError::internal(other.to_string()),
    }
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
