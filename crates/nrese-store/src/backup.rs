use oxigraph::io::RdfFormat;
use oxigraph::store::Store;
use sha2::{Digest, Sha256};

use crate::error::StoreResult;
use crate::stats::collect_stats;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DatasetBackupFormat {
    NQuads,
}

impl DatasetBackupFormat {
    pub fn media_type(self) -> &'static str {
        match self {
            Self::NQuads => "application/n-quads",
        }
    }

    fn into_oxigraph(self) -> RdfFormat {
        match self {
            Self::NQuads => RdfFormat::NQuads,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DatasetBackupArtifact {
    pub format: DatasetBackupFormat,
    pub media_type: &'static str,
    pub payload: Vec<u8>,
    pub checksum_sha256: String,
    pub source_revision: u64,
    pub quad_count: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DatasetRestoreRequest {
    pub format: DatasetBackupFormat,
    pub payload: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DatasetRestoreReport {
    pub format: DatasetBackupFormat,
    pub checksum_sha256: String,
    pub revision: u64,
    pub quad_count: u64,
    pub replaced_existing: bool,
}

pub fn export_dataset(
    store: &Store,
    revision: u64,
    format: DatasetBackupFormat,
) -> StoreResult<DatasetBackupArtifact> {
    let payload = store.dump_to_writer(format.into_oxigraph(), Vec::new())?;
    let stats = collect_stats(store)?;

    Ok(DatasetBackupArtifact {
        format,
        media_type: format.media_type(),
        checksum_sha256: sha256_hex(&payload),
        payload,
        source_revision: revision,
        quad_count: stats.quad_count as u64,
    })
}

pub fn restore_dataset(
    store: &Store,
    request: &DatasetRestoreRequest,
    next_revision: u64,
) -> StoreResult<DatasetRestoreReport> {
    let checksum_sha256 = sha256_hex(&request.payload);
    let current_stats = collect_stats(store)?;
    let replaced_existing = !current_stats.is_empty;

    // Validate and parse the whole artifact before touching the live store.
    let validation_store = Store::new()?;
    validation_store.load_from_slice(request.format.into_oxigraph(), &request.payload)?;

    let mut transaction = store.start_transaction()?;
    transaction.clear()?;
    transaction.load_from_slice(request.format.into_oxigraph(), &request.payload)?;
    transaction.commit()?;

    let restored_stats = collect_stats(store)?;

    Ok(DatasetRestoreReport {
        format: request.format,
        checksum_sha256,
        revision: next_revision,
        quad_count: restored_stats.quad_count as u64,
        replaced_existing,
    })
}

fn sha256_hex(payload: &[u8]) -> String {
    let digest = Sha256::digest(payload);
    format!("{digest:x}")
}
