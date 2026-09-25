//! Checkpoints: a full image of one revision (dictionary plus quads).
//!
//! Format (little-endian): `magic | revision u64 | dictionary_len u64 | (key_len u32, key)* |
//! quad_count u64 | (s p o g)* | crc32 u32`, where the CRC covers everything before it.
//!
//! A checkpoint is written from a [`Snapshot`], so writers keep committing while it is being
//! written. It is streamed to `checkpoint-<revision>.tmp`, synced, and atomically renamed
//! to `checkpoint-<revision>.nck`; a crash at any point leaves either the old or the new
//! checkpoint, never a partial one. Leftover `.tmp` files are deleted on open.

use std::fs::{self, File};
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};

use super::codec::{Reader, put_quad, put_u32, put_u64};
use crate::engine::Snapshot;
use crate::error::{EngineError, EngineResult};
use crate::quad::{EncodedQuad, QuadPattern};
use crate::term::Dictionary;

const MAGIC: &[u8; 8] = b"NRESECK1";
const EXTENSION: &str = "nck";
/// Dictionary keys copied per lock acquisition, so interning is never blocked for long.
const KEY_CHUNK: u64 = 64 * 1024;

fn checkpoint_path(dir: &Path, revision: u64, extension: &str) -> PathBuf {
    dir.join(format!("checkpoint-{revision:020}.{extension}"))
}

fn list(dir: &Path) -> EngineResult<Vec<(u64, PathBuf)>> {
    let mut found = Vec::new();
    for entry in fs::read_dir(dir)? {
        let path = entry?.path();
        let revision = path
            .file_name()
            .and_then(|name| name.to_str())
            .and_then(|name| name.strip_prefix("checkpoint-"))
            .and_then(|rest| rest.strip_suffix(&format!(".{EXTENSION}")))
            .and_then(|stem| stem.parse::<u64>().ok());
        if let Some(revision) = revision {
            found.push((revision, path));
        }
    }
    found.sort_unstable_by_key(|(revision, _)| *revision);
    Ok(found)
}

/// Deletes temporary files left by a crash during a checkpoint write.
pub(crate) fn remove_temporaries(dir: &Path) -> EngineResult<()> {
    for entry in fs::read_dir(dir)? {
        let path = entry?.path();
        if path.extension().is_some_and(|ext| ext == "tmp") {
            fs::remove_file(path)?;
        }
    }
    Ok(())
}

/// Writer that feeds everything it writes into a CRC.
struct Checksummed<W> {
    inner: W,
    crc: crc32fast::Hasher,
    buffer: Vec<u8>,
}

impl<W: Write> Checksummed<W> {
    /// Writes and checksums the scratch buffer.
    fn flush_buffer(&mut self) -> std::io::Result<()> {
        self.crc.update(&self.buffer);
        self.inner.write_all(&self.buffer)?;
        self.buffer.clear();
        Ok(())
    }
}

/// Writes a checkpoint of `snapshot` and returns its path.
pub(crate) fn write(dir: &Path, snapshot: &Snapshot) -> EngineResult<PathBuf> {
    let revision = snapshot.revision();
    let tmp = checkpoint_path(dir, revision, "tmp");
    let file = File::create(&tmp)?;
    let mut out = Checksummed {
        inner: BufWriter::with_capacity(1 << 20, file),
        crc: crc32fast::Hasher::new(),
        buffer: Vec::with_capacity(1 << 16),
    };
    out.buffer.extend_from_slice(MAGIC);
    put_u64(&mut out.buffer, revision);
    let dictionary_len = snapshot.dictionary_len();
    put_u64(&mut out.buffer, dictionary_len);
    let mut from = 0;
    while from < dictionary_len {
        let to = (from + KEY_CHUNK).min(dictionary_len);
        for key in snapshot.dictionary().export_keys(from, to) {
            put_u32(&mut out.buffer, key.len() as u32);
            out.buffer.extend_from_slice(&key);
        }
        out.flush_buffer()?;
        from = to;
    }
    put_u64(&mut out.buffer, snapshot.len());
    for quad in snapshot.quads_for_pattern(&QuadPattern::all()) {
        put_quad(&mut out.buffer, &quad);
        if out.buffer.len() >= 1 << 16 {
            out.flush_buffer()?;
        }
    }
    out.flush_buffer()?;
    let crc = out.crc.finalize();
    let mut inner = out.inner;
    inner.write_all(&crc.to_le_bytes())?;
    let file = inner.into_inner().map_err(|error| error.into_error())?;
    file.sync_all()?;
    drop(file);
    let path = checkpoint_path(dir, revision, EXTENSION);
    fs::rename(&tmp, &path)?;
    super::sync_dir(dir)?;
    Ok(path)
}

/// A loaded checkpoint: its revision and quads; the dictionary is restored in place.
pub(crate) struct Loaded {
    pub revision: u64,
    pub quads: Vec<EncodedQuad>,
}

/// Loads the newest checkpoint into `dictionary`, if there is one.
pub(crate) fn load_latest(dir: &Path, dictionary: &Dictionary) -> EngineResult<Option<Loaded>> {
    let Some((_, path)) = list(dir)?.pop() else {
        return Ok(None);
    };
    let bytes = fs::read(&path)?;
    let corrupt =
        |what: &str| EngineError::Corruption(format!("checkpoint {}: {what}", path.display()));
    let (body, crc) = bytes
        .split_at_checked(bytes.len().saturating_sub(4))
        .ok_or_else(|| corrupt("truncated"))?;
    if crc.len() != 4
        || crc32fast::hash(body) != u32::from_le_bytes(crc.try_into().expect("4 bytes"))
    {
        return Err(corrupt("checksum mismatch"));
    }
    let mut reader = Reader::new(body);
    if reader.bytes(MAGIC.len()) != Some(MAGIC.as_slice()) {
        return Err(corrupt("bad magic"));
    }
    let revision = reader.u64().ok_or_else(|| corrupt("truncated header"))?;
    let dictionary_len = reader.u64().ok_or_else(|| corrupt("truncated header"))?;
    for index in 0..dictionary_len {
        let len = reader
            .u32()
            .ok_or_else(|| corrupt("truncated dictionary"))?;
        let key = reader
            .bytes(len as usize)
            .ok_or_else(|| corrupt("truncated dictionary"))?;
        dictionary.restore_key(index, key)?;
    }
    let count = reader.u64().ok_or_else(|| corrupt("truncated quads"))?;
    if count.saturating_mul(32) != reader.remaining() as u64 {
        return Err(corrupt("quad count does not match file size"));
    }
    let quads = (0..count)
        .map(|_| reader.quad())
        .collect::<Option<Vec<_>>>()
        .ok_or_else(|| corrupt("truncated quads"))?;
    Ok(Some(Loaded { revision, quads }))
}

/// Deletes checkpoints older than `revision`.
pub(crate) fn remove_older_than(dir: &Path, revision: u64) -> EngineResult<()> {
    for (checkpoint_revision, path) in list(dir)? {
        if checkpoint_revision < revision {
            fs::remove_file(path)?;
        }
    }
    Ok(())
}
