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
    // Written beside it and placed durably, as the image is: a restore reads it first.
    let partial = path.with_extension("json.tmp");
    fs::write(&partial, json).map_err(|e| io(e, &partial))?;
    nrese_engine::place_file(&partial, &path).map_err(|e| io(e, &path))?;
    Ok(manifest)
}

impl ImageManifest {
    /// The image's file name, checked: a checkpoint's (`checkpoint-<revision>.nck`), a
    /// plain name in the backup directory. A manifest naming anything else (a path out of
    /// it, from a tampered archive or primary) is refused before the name is used.
    pub fn image_name(&self) -> StoreResult<&str> {
        match nrese_engine::image_revision(&self.file) {
            Some(_) => Ok(&self.file),
            None => Err(StoreError::Configuration(format!(
                "the manifest names the image {:?}, not a checkpoint file name                  (checkpoint-<revision>.nck)",
                self.file
            ))),
        }
    }
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
            // With its sidecar: the sync policy its recovery reads.
            nrese_engine::copy_log_segment(path, &wal)?;
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
    let image = backup.join(manifest.image_name()?);
    place_image(&image, manifest, data_dir)
}

/// Places the image in file `image` (any name: a download is staged under its own),
/// described by `manifest`, into `data_dir`, which must hold no store: checked against the
/// manifest, then copied there under the manifest's checked file name.
pub fn place_image(
    image: &Path,
    manifest: ImageManifest,
    data_dir: &Path,
) -> StoreResult<ImageManifest> {
    let name = manifest.image_name()?.to_owned();
    if manifest.format != "nrese-checkpoint" {
        return Err(StoreError::Configuration(format!(
            "{}: not an image backup ({})",
            image.display(),
            manifest.format
        )));
    }
    let (sha256, bytes) = sha256(image)?;
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
    let target = data_dir.join(&name);
    let partial = data_dir.join(format!("{name}.restoring"));
    fs::copy(image, &partial).map_err(|e| io(e, image))?;
    nrese_engine::place_file(&partial, &target).map_err(|e| io(e, &target))?;
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
            nrese_engine::remove_log_segment(path)?;
            removed += 1;
        }
    }
    Ok(removed)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A manifest naming its image by a path out of the backup (a tampered archive or
    /// primary) is refused before the name is used: nothing outside is read or written.
    #[test]
    fn an_image_name_that_isnt_a_plain_checkpoint_name_is_refused() {
        let root = tempfile::tempdir().unwrap();
        let (backup, data_dir) = (root.path().join("backup"), root.path().join("data"));
        fs::create_dir_all(&backup).unwrap();
        // A file outside the backup that matches the manifest: followed, it would be
        // copied to `data/../escape.nck`.
        let outside = root.path().join("escape.nck");
        fs::write(&outside, b"not an image").unwrap();
        let (sha256, bytes) = sha256(&outside).unwrap();
        let absolute = outside.display().to_string();
        for name in [
            "../escape.nck",
            absolute.as_str(),
            "checkpoint-1.nck:stream",
            "checkpoint-+1.nck",
            "escape.nck",
        ] {
            let manifest = ImageManifest {
                format: "nrese-checkpoint".to_owned(),
                file: name.to_owned(),
                revision: 1,
                quads: 0,
                inferred: 0,
                bytes,
                sha256: sha256.clone(),
                created_unix: 0,
                nrese_version: String::new(),
            };
            fs::write(
                backup.join(MANIFEST),
                serde_json::to_vec(&manifest).unwrap(),
            )
            .unwrap();
            assert!(restore_image(&backup, &data_dir).is_err(), "{name}");
            assert_eq!(fs::read(&outside).unwrap(), b"not an image", "{name}");
            assert!(!data_dir.exists(), "{name}: nothing placed");
        }
        assert_eq!(
            nrese_engine::image_revision("checkpoint-00000000000000000042.nck"),
            Some(42)
        );
    }
}
