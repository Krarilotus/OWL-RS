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

/// Restores the backup in `backup` and the WAL segments in `logs` after it (an archive,
/// `wal-archive/` of a store with `wal_archive`, and the store's live `wal/` where it is
/// still there) into `data_dir`, which must hold no store, up to revision `until` and the
/// commits made up to `until_micros` (microseconds since 1970; else as far as the log
/// goes): [`restore_image`], the segments copied, then the store opened once to replay and
/// cut the log. Returns the manifest and the revision restored.
pub fn restore_until(
    backup: &Path,
    data_dir: &Path,
    logs: &[PathBuf],
    until: Option<u64>,
    until_micros: Option<u64>,
) -> StoreResult<(ImageManifest, u64)> {
    let manifest = restore_image(backup, data_dir)?;
    let wal = data_dir.join("wal");
    fs::create_dir_all(&wal).map_err(|e| io(e, &wal))?;
    // Segments are named by their first revision; one ends where the next begins. Those
    // that can hold a revision after the image go along.
    let mut segments: Vec<(u64, PathBuf)> = Vec::new();
    for dir in logs {
        for entry in fs::read_dir(dir).map_err(|e| io(e, dir))? {
            let path = entry.map_err(|e| io(e, dir))?.path();
            let first = path
                .file_name()
                .and_then(|name| name.to_str())
                .and_then(|name| name.strip_suffix(".wal"))
                .and_then(|stem| stem.parse::<u64>().ok());
            if let Some(first) = first {
                // The same segment in two places: the larger copy (a live one grew).
                let size = |path: &Path| fs::metadata(path).map_or(0, |m| m.len());
                match segments.iter_mut().find(|(f, _)| *f == first) {
                    Some(existing) if size(&path) > size(&existing.1) => existing.1 = path,
                    Some(_) => {}
                    None => segments.push((first, path)),
                }
            }
        }
    }
    segments.sort_unstable_by_key(|(first, _)| *first);
    for (i, (_, path)) in segments.iter().enumerate() {
        let after_image = segments
            .get(i + 1)
            .is_none_or(|(next, _)| *next > manifest.revision + 1);
        if after_image {
            let target = wal.join(path.file_name().expect("segment name"));
            fs::copy(path, &target).map_err(|e| io(e, path))?;
        }
    }
    let config = nrese_engine::EngineConfig {
        background_maintenance: false,
        durability: nrese_engine::DurabilityConfig {
            recover_until: until,
            recover_until_micros: until_micros,
            ..nrese_engine::DurabilityConfig::default()
        },
        ..nrese_engine::EngineConfig::default()
    };
    let engine = nrese_engine::Engine::open(data_dir, config)?;
    let revision = engine.snapshot().revision();
    Ok((manifest, revision))
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

/// Removes from `archive` (a store's `wal-archive/`) the segments whose records all come
/// before `revision`: what an image backup at `revision - 1` or later makes unneeded. The
/// newest segment stays. Safe while the store runs (it only adds to the archive). Returns
/// the number of segments removed.
pub fn prune_wal_archive(archive: &Path, revision: u64) -> StoreResult<usize> {
    let mut segments: Vec<(u64, PathBuf)> = Vec::new();
    for entry in fs::read_dir(archive).map_err(|e| io(e, archive))? {
        let path = entry.map_err(|e| io(e, archive))?.path();
        let first = path
            .file_name()
            .and_then(|name| name.to_str())
            .and_then(|name| name.strip_suffix(".wal"))
            .and_then(|stem| stem.parse::<u64>().ok());
        if let Some(first) = first {
            segments.push((first, path));
        }
    }
    segments.sort_unstable_by_key(|(first, _)| *first);
    let mut removed = 0;
    for pair in segments.windows(2) {
        // A segment's records end where the next segment's begin.
        let ((_, path), (next_first, _)) = (&pair[0], &pair[1]);
        if *next_first <= revision {
            fs::remove_file(path).map_err(|e| io(e, path))?;
            removed += 1;
        }
    }
    Ok(removed)
}
