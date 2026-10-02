//! Physical backups: an image of a snapshot (a checkpoint file: the dictionary and both
//! index stacks, inferred statements included) and a manifest, written while writers go
//! on; restored by placing the image in an empty data directory, where opening the store
//! recovers it as any checkpoint. Faster and smaller than the N-Quads export
//! ([`crate::backup`]), and nothing is derived again after a restore; the image needs an
//! engine that reads its checkpoint format.

use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::error::{StoreError, StoreResult};

/// The manifest's file name in a backup directory.
pub const MANIFEST: &str = "manifest.json";

/// What a backup directory holds.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImageManifest {
    /// `nrese-checkpoint`.
    pub format: String,
    /// The image's file name, in the backup directory.
    pub file: String,
    pub revision: u64,
    pub quads: u64,
    pub inferred: u64,
    pub bytes: u64,
    pub sha256: String,
    /// When it was written, seconds since 1970.
    pub created_unix: u64,
    /// The NRESE version that wrote it.
    pub nrese_version: String,
}

fn io(error: std::io::Error, what: &Path) -> StoreError {
    StoreError::Configuration(format!("{}: {error}", what.display()))
}

/// The SHA-256 of a file, read in pieces.
fn sha256(path: &Path) -> StoreResult<(String, u64)> {
    let mut file = fs::File::open(path).map_err(|e| io(e, path))?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; 1 << 20];
    let mut bytes = 0u64;
    loop {
        let n = file.read(&mut buffer).map_err(|e| io(e, path))?;
        if n == 0 {
            break;
        }
        hasher.update(&buffer[..n]);
        bytes += n as u64;
    }
    let digest = hasher.finalize();
    Ok((digest.iter().map(|b| format!("{b:02x}")).collect(), bytes))
}

/// Writes an image of `engine`'s latest snapshot and its manifest into `dir`, which must
/// not hold a backup yet.
pub(crate) fn backup_image(
    engine: &nrese_engine::Engine,
    dir: &Path,
) -> StoreResult<ImageManifest> {
    if dir.join(MANIFEST).exists() {
        return Err(StoreError::Configuration(format!(
            "{} already holds a backup",
            dir.display()
        )));
    }
    fs::create_dir_all(dir).map_err(|e| io(e, dir))?;
    let image = engine.write_image(dir)?;
    let (sha256, bytes) = sha256(&image.path)?;
    let manifest = ImageManifest {
        format: "nrese-checkpoint".to_owned(),
        file: image
            .path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_default()
            .to_owned(),
        revision: image.revision,
        quads: image.quads,
        inferred: image.inferred,
        bytes,
        sha256,
        created_unix: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |since| since.as_secs()),
        nrese_version: env!("CARGO_PKG_VERSION").to_owned(),
    };
    let json = serde_json::to_vec_pretty(&manifest)
        .map_err(|error| StoreError::Configuration(error.to_string()))?;
    let path = dir.join(MANIFEST);
    fs::write(&path, json).map_err(|e| io(e, &path))?;
    Ok(manifest)
}

/// The manifest of the backup in `dir`.
pub fn read_manifest(dir: &Path) -> StoreResult<ImageManifest> {
    let path = dir.join(MANIFEST);
    let bytes = fs::read(&path).map_err(|e| io(e, &path))?;
    serde_json::from_slice(&bytes)
        .map_err(|error| StoreError::Configuration(format!("{}: {error}", path.display())))
}

/// Restores the backup in `backup` into `data_dir`, which must hold no store (no
/// checkpoint, no log): the image is checked against its manifest, then copied there.
/// The store opens from it as from any checkpoint.
pub fn restore_image(backup: &Path, data_dir: &Path) -> StoreResult<ImageManifest> {
    let manifest = read_manifest(backup)?;
    if manifest.format != "nrese-checkpoint" {
        return Err(StoreError::Configuration(format!(
            "{}: not an image backup ({})",
            backup.display(),
            manifest.format
        )));
    }
    let image: PathBuf = backup.join(&manifest.file);
    let (sha256, bytes) = sha256(&image)?;
    if sha256 != manifest.sha256 || bytes != manifest.bytes {
        return Err(StoreError::Configuration(format!(
            "{}: the image doesn't match its manifest",
            image.display()
        )));
    }
    let holds_store = |dir: &Path| -> bool {
        fs::read_dir(dir).is_ok_and(|entries| {
            entries.flatten().any(|entry| {
                let name = entry.file_name();
                let name = name.to_string_lossy();
                name.ends_with(".nck")
                    || (name == "wal"
                        && fs::read_dir(entry.path()).is_ok_and(|mut wal| wal.next().is_some()))
            })
        })
    };
    if holds_store(data_dir) {
        return Err(StoreError::Configuration(format!(
            "{} holds a store: restore into an empty data directory",
            data_dir.display()
        )));
    }
    fs::create_dir_all(data_dir).map_err(|e| io(e, data_dir))?;
    let target = data_dir.join(&manifest.file);
    let partial = data_dir.join(format!("{}.restoring", manifest.file));
    fs::copy(&image, &partial).map_err(|e| io(e, &image))?;
    fs::rename(&partial, &target).map_err(|e| io(e, &target))?;
    Ok(manifest)
}
