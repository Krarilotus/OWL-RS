//! Indexes derived from the dictionary (the text indexes, the vector index), kept on
//! disk so that a restart doesn't build them again: each is written after a checkpoint
//! into `derived/` of the data directory, and read at its first use after a start, then
//! extended with the terms interned since, as it is in a running store.
//!
//! A file says how many dictionary entries it covers and carries a fingerprint of some
//! of them (their keys' hashes): it is used only if the dictionary holds those entries
//! with those keys, so a file left from another store, an older one or a damaged one is
//! ignored and the index built anew. A checksum at its end covers everything before it;
//! files are written aside and renamed into place.
//!
//! Layout: magic (8 bytes), format (u32), covered entries (u64), fingerprint (u64), the
//! index's own bytes, CRC-32 of all before it (u32). Numbers little-endian.

use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};

const MAGIC: &[u8; 8] = b"NRDERIVD";

/// The version of the layout; an index's own bytes have their own version inside.
const FORMAT: u32 = 1;

/// Writes an index's bytes, computing the checksum as they go.
pub(crate) struct Writer<W: Write> {
    inner: W,
    crc: crc32fast::Hasher,
}

impl<W: Write> Writer<W> {
    pub(crate) fn bytes(&mut self, bytes: &[u8]) -> std::io::Result<()> {
        self.crc.update(bytes);
        self.inner.write_all(bytes)
    }

    pub(crate) fn u32(&mut self, value: u32) -> std::io::Result<()> {
        self.bytes(&value.to_le_bytes())
    }

    pub(crate) fn u64(&mut self, value: u64) -> std::io::Result<()> {
        self.bytes(&value.to_le_bytes())
    }

    pub(crate) fn f32s(&mut self, values: &[f32]) -> std::io::Result<()> {
        for value in values {
            self.bytes(&value.to_le_bytes())?;
        }
        Ok(())
    }

    /// A length-prefixed byte string.
    pub(crate) fn blob(&mut self, bytes: &[u8]) -> std::io::Result<()> {
        self.u64(bytes.len() as u64)?;
        self.bytes(bytes)
    }
}

/// Reads an index's bytes; every read is checked against the end.
pub(crate) struct Reader<'a> {
    bytes: &'a [u8],
    position: usize,
}

impl<'a> Reader<'a> {
    pub(crate) fn bytes(&mut self, n: usize) -> Option<&'a [u8]> {
        let end = self.position.checked_add(n)?;
        let slice = self.bytes.get(self.position..end)?;
        self.position = end;
        Some(slice)
    }

    pub(crate) fn u32(&mut self) -> Option<u32> {
        Some(u32::from_le_bytes(self.bytes(4)?.try_into().ok()?))
    }

    pub(crate) fn u64(&mut self) -> Option<u64> {
        Some(u64::from_le_bytes(self.bytes(8)?.try_into().ok()?))
    }

    /// A length for allocating: no more than the bytes left could hold at `unit` bytes
    /// each, so a damaged length can't ask for more memory than the file has.
    pub(crate) fn len(&mut self, unit: usize) -> Option<usize> {
        let n = usize::try_from(self.u64()?).ok()?;
        (n.checked_mul(unit.max(1))? <= self.bytes.len() - self.position).then_some(n)
    }

    pub(crate) fn f32s(&mut self, n: usize) -> Option<Vec<f32>> {
        let bytes = self.bytes(n.checked_mul(4)?)?;
        Some(
            bytes
                .as_chunks::<4>()
                .0
                .iter()
                .map(|b| f32::from_le_bytes(*b))
                .collect(),
        )
    }

    pub(crate) fn blob(&mut self) -> Option<&'a [u8]> {
        let n = self.len(1)?;
        self.bytes(n)
    }

    pub(crate) fn is_done(&self) -> bool {
        self.position == self.bytes.len()
    }
}

fn path_of(dir: &Path, name: &str) -> PathBuf {
    dir.join(format!("{name}.index"))
}

/// Writes index `name` into `dir`: `covered` entries with `fingerprint`, its bytes by
/// `body`. Atomic: a crash leaves the previous file or none.
pub(crate) fn save(
    dir: &Path,
    name: &str,
    covered: u64,
    fingerprint: u64,
    body: impl FnOnce(&mut Writer<BufWriter<File>>) -> std::io::Result<()>,
) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    let temporary = dir.join(format!("{name}.index.tmp"));
    let file = File::create(&temporary)?;
    let mut writer = Writer {
        inner: BufWriter::with_capacity(1 << 20, file),
        crc: crc32fast::Hasher::new(),
    };
    writer.bytes(MAGIC)?;
    writer.u32(FORMAT)?;
    writer.u64(covered)?;
    writer.u64(fingerprint)?;
    body(&mut writer)?;
    let crc = writer.crc.clone().finalize();
    writer.inner.write_all(&crc.to_le_bytes())?;
    let file = writer.inner.into_inner().map_err(|e| e.into_error())?;
    file.sync_all()?;
    drop(file);
    std::fs::rename(&temporary, path_of(dir, name))?;
    #[cfg(unix)]
    File::open(dir)?.sync_all()?;
    Ok(())
}

/// Index `name` from `dir` if it is there, whole, and covers entries whose fingerprint
/// `fingerprint_of` (given the covered count) gives as recorded: the covered count and a
/// mapping of the file, the index's bytes after the header.
pub(crate) fn load(
    dir: &Path,
    name: &str,
    fingerprint_of: impl FnOnce(u64) -> Option<u64>,
) -> Option<(u64, Loaded)> {
    let path = path_of(dir, name);
    let file = File::open(&path).ok()?;
    // SAFETY: the file is ours (written aside and renamed into place), read only, and
    // replaced by a rename, never changed in place: the mapping's bytes don't change.
    let map = unsafe { memmap2::Mmap::map(&file) }.ok()?;
    let complain = |why: &str| {
        tracing::warn!(path = %path.display(), why, "derived index ignored; it is built anew");
    };
    if map.len() < 8 + 4 + 8 + 8 + 4 || &map[..8] != MAGIC {
        complain("not an index file");
        return None;
    }
    let (body, trailer) = map.split_at(map.len() - 4);
    let crc = u32::from_le_bytes(trailer.try_into().ok()?);
    if crc32fast::hash(body) != crc {
        complain("checksum mismatch");
        return None;
    }
    let mut reader = Reader {
        bytes: body,
        position: 8,
    };
    if reader.u32()? != FORMAT {
        complain("another format");
        return None;
    }
    let covered = reader.u64()?;
    let fingerprint = reader.u64()?;
    if fingerprint_of(covered) != Some(fingerprint) {
        complain("another dictionary");
        return None;
    }
    let start = reader.position;
    Some((covered, Loaded { map, start }))
}

/// A loaded index file: its bytes after the header.
pub(crate) struct Loaded {
    map: memmap2::Mmap,
    start: usize,
}

impl Loaded {
    pub(crate) fn reader(&self) -> Reader<'_> {
        Reader {
            bytes: &self.map[self.start..self.map.len() - 4],
            position: 0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn files_round_trip_and_damage_is_detected() {
        let dir = tempfile::tempdir().unwrap();
        save(dir.path(), "test", 42, 7, |w| {
            w.u64(5)?;
            w.blob(b"hello")?;
            w.f32s(&[1.5, -2.0])
        })
        .unwrap();
        let (covered, loaded) = load(dir.path(), "test", |c| (c == 42).then_some(7)).unwrap();
        assert_eq!(covered, 42);
        let mut r = loaded.reader();
        assert_eq!(r.u64(), Some(5));
        assert_eq!(r.blob(), Some(&b"hello"[..]));
        assert_eq!(r.f32s(2), Some(vec![1.5, -2.0]));
        assert!(r.is_done());
        drop(loaded);
        // Another dictionary: ignored.
        assert!(load(dir.path(), "test", |_| Some(8)).is_none());
        // A flipped byte: ignored.
        let path = dir.path().join("test.index");
        let mut bytes = std::fs::read(&path).unwrap();
        bytes[30] ^= 1;
        std::fs::write(&path, bytes).unwrap();
        assert!(load(dir.path(), "test", |_| Some(7)).is_none());
        // No file: none.
        assert!(load(dir.path(), "other", |_| Some(7)).is_none());
    }
}
