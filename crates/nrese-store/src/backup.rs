//! Logical backups: every asserted statement as N-Quads, and the restore of one.
//!
//! An export is written as it is read, then closed by a comment holding the SHA-256 of
//! every byte before it (`# nrese-sha256 <hex>`, [`DIGEST_PREFIX`]): the checksum travels
//! with the file, and every N-Quads reader skips it. A restore checks it where present and
//! refuses a mismatch; a dump without one (from another store, or an older NRESE) restores
//! unchecked.

use std::io::Write;

use nrese_engine::{QuadPattern, ReadModel, Snapshot, Transaction};
use nrese_rdf_io::{RdfFormat, RdfParseError};
use sha2::{Digest, Sha256};

use crate::error::{StoreError, StoreResult};
use crate::rdf_io::{parse_dataset, write_quads};

/// The closing comment of an export, before its checksum's 64 hex digits.
pub const DIGEST_PREFIX: &str = "# nrese-sha256 ";

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
    /// The payload's closing checksum comment was there and matched; `false`: it had none.
    pub checksum_verified: bool,
}

/// An export of one snapshot: its revision and statement count are known before any byte
/// is written (a transport's headers), its statements as [`Self::write`] reads them.
#[derive(Debug, Clone)]
pub struct DatasetExport {
    snapshot: Snapshot,
    pub format: DatasetBackupFormat,
    pub source_revision: u64,
    pub quad_count: u64,
}

impl DatasetExport {
    pub(crate) fn of(snapshot: Snapshot, format: DatasetBackupFormat) -> Self {
        Self {
            format,
            source_revision: snapshot.revision(),
            // Backups hold asserted statements only; inferences are derived again after a
            // restore.
            quad_count: snapshot.len_in(ReadModel::Asserted),
            snapshot,
        }
    }

    /// Writes the export to `out` as it is read, then its closing checksum comment;
    /// `cancel` stops it between statements. Returns the checksum (hex).
    pub fn write(
        &self,
        cancel: &nrese_sparql::CancellationToken,
        out: impl Write,
    ) -> StoreResult<String> {
        let out = Digesting(out, Sha256::new());
        let all = [QuadPattern::all()];
        let format = (self.format.rdf_format(), true);
        let (_, Digesting(mut out, digest)) = write_quads(
            &self.snapshot,
            ReadModel::Asserted,
            &all,
            format,
            cancel,
            out,
        )?;
        let checksum = format!("{:x}", digest.finalize());
        writeln!(out, "{DIGEST_PREFIX}{checksum}")?;
        out.flush()?;
        Ok(checksum)
    }
}

/// A writer that hashes what passes through it.
struct Digesting<W>(W, Sha256);

impl<W: Write> Write for Digesting<W> {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        let written = self.0.write(bytes)?;
        self.1.update(&bytes[..written]);
        Ok(written)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.0.flush()
    }
}

/// The whole export in memory, for callers that want it so (tests, embedding); the server
/// streams [`DatasetExport::write`].
pub fn export_dataset(
    snapshot: &Snapshot,
    format: DatasetBackupFormat,
) -> StoreResult<DatasetBackupArtifact> {
    let export = DatasetExport::of(snapshot.clone(), format);
    let mut payload = Vec::new();
    let checksum_sha256 = export.write(&nrese_sparql::CancellationToken::new(), &mut payload)?;
    Ok(DatasetBackupArtifact {
        format,
        media_type: format.media_type(),
        checksum_sha256,
        payload,
        source_revision: export.source_revision,
        quad_count: export.quad_count,
    })
}

/// Replaces the whole dataset with the artifact's quads. The artifact is parsed completely
/// before the transaction is touched, so an invalid artifact changes nothing.
pub(crate) fn apply_restore(
    tx: &mut Transaction<'_>,
    request: &DatasetRestoreRequest,
) -> StoreResult<DatasetRestoreReport> {
    let checksum_verified = verify_checksum(&request.payload)?;
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
        checksum_verified,
    })
}

/// Checks the closing checksum comment of `payload`, if it has one: `true` when it matches
/// the bytes before it, `false` when there is none; a mismatch (a damaged or truncated
/// file) is an error, before the dataset is touched.
fn verify_checksum(payload: &[u8]) -> StoreResult<bool> {
    let body = payload.strip_suffix(b"\n").unwrap_or(payload);
    let start = body.iter().rposition(|&b| b == b'\n').map_or(0, |i| i + 1);
    let Some(recorded) = body[start..].strip_prefix(DIGEST_PREFIX.as_bytes()) else {
        return Ok(false);
    };
    let recorded = String::from_utf8_lossy(recorded)
        .trim()
        .to_ascii_lowercase();
    let computed = sha256_hex(&payload[..start]);
    if recorded == computed {
        return Ok(true);
    }
    Err(StoreError::RdfParse(RdfParseError::Io(
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!(
                "the backup's checksum doesn't match its content (recorded {recorded}, computed \
             {computed}): the file is damaged or incomplete"
            ),
        ),
    )))
}

fn sha256_hex(payload: &[u8]) -> String {
    let digest = Sha256::digest(payload);
    format!("{digest:x}")
}
