use nrese_engine::{QuadPattern, ReadModel, Snapshot, Transaction};
use oxrdfio::RdfFormat;
use sha2::{Digest, Sha256};

use crate::error::StoreResult;
use crate::rdf_io::{parse_dataset, serialize_quads};
use crate::view::decoded_quads;

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

    fn rdf_format(self) -> RdfFormat {
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
    snapshot: &Snapshot,
    format: DatasetBackupFormat,
) -> StoreResult<DatasetBackupArtifact> {
    // Backups hold asserted statements only; inferences are derived again after a restore.
    let quads = decoded_quads(snapshot, ReadModel::Asserted, &QuadPattern::all())
        .collect::<StoreResult<Vec<_>>>()?;
    let payload = serialize_quads(format.rdf_format(), quads)?;
    Ok(DatasetBackupArtifact {
        format,
        media_type: format.media_type(),
        checksum_sha256: sha256_hex(&payload),
        payload,
        source_revision: snapshot.revision(),
        quad_count: snapshot.len_in(ReadModel::Asserted),
    })
}

/// Replaces the whole dataset with the artifact's quads. The artifact is parsed completely
/// before the transaction is touched, so an invalid artifact changes nothing.
pub(crate) fn apply_restore(
    tx: &mut Transaction<'_>,
    request: &DatasetRestoreRequest,
) -> StoreResult<DatasetRestoreReport> {
    let quads = parse_dataset(request.format.rdf_format(), &request.payload)?;
    let replaced_existing = !tx.is_empty();
    tx.remove_matching(&QuadPattern::all());
    for quad in &quads {
        tx.insert(quad.as_ref());
    }
    Ok(DatasetRestoreReport {
        format: request.format,
        checksum_sha256: sha256_hex(&request.payload),
        revision: 0,
        quad_count: tx.len_in(ReadModel::Asserted),
        replaced_existing,
    })
}

fn sha256_hex(payload: &[u8]) -> String {
    let digest = Sha256::digest(payload);
    format!("{digest:x}")
}
