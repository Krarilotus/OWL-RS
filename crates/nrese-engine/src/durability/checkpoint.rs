//! Checkpoints: a full image of one revision (dictionary plus both index stacks).
//!
//! Format 12 (little-endian): `magic | revision u64 | flags u64 | dictionary | stack* |
//! committed u64 | crc32 u32`. The CRC covers everything before it. `committed` is when the
//! revision's last commit was made, microseconds since 1970, a time restore needs
//! (`DurabilityConfig::recover_until_micros`); with its top bit ([`UNKNOWN_TIME`]) set, the
//! revision also holds commits of unknown time, and the rest is only the store's commit
//! clock, which new commits never go before; 0 knows neither. Format 11 is format 12 without
//! it. Flag bit 1 ([`COMPRESSED_KEYS`], format 11 on):
//! the dictionary's keys are FSST-compressed ([`crate::term::vocabulary`]); its two symbol
//! tables follow the dictionary's header. Flag bit 0 ([`INTEGERS_IN_DICTIONARY`]):
//! integer-derived literals are dictionary entries, not inline (a store created before
//! [`TermKind::DerivedInteger`](crate::term::TermKind)); formats 4-7 have no flags and
//! are such stores.
//! - The dictionary is `len u64 | arena_len u64 | slot_count u64 | ends_len u64 | padding |
//!   blocks: 2·⌈len/64⌉ × u64 | arena: arena_len bytes | ends: ends_len bytes | padding |
//!   slots: slot_count × u32`: block offsets ([`crate::term::offsets`]), the keys one after
//!   another, their ends relative to their block, and an open-addressing hash table of the
//!   keys (linear probing by the fixed [`key_hash`], entry index + 1 per slot, 0 empty),
//!   then `padding | order_len u64 | order: order_len × u32`, the entries with a text
//!   sorted by it ([`crate::term::order`]: prefix searches). Padding is zero bytes up to an
//!   8-byte boundary of the file. Formats 8 to 10 have `len u64 | arena_len u64 |
//!   slot_count u64 | padding | ends: len × u64 | arena | padding | slots …` instead: an
//!   8-byte end offset per key. Format 7 is format 8 without the flags, format 6 format 7
//!   without the order.
//! - Each stack (asserted, then inferred) is `count u32 | (permutation u8, packed keys)*`:
//!   every permutation of the stack's layout, compressed as in memory
//!   ([`PackedKeys::write`]), its packed bits padded to an 8-byte boundary. The layout is
//!   the one the permutations make up: since format 9 the asserted stack may have the
//!   default-graph layout too (four permutations; format 8 is format 9 with the quad
//!   layout's seven always).
//!
//! Restart maps the file and uses all of that in place ([`crate::mapped`]): the dictionary
//! as the dictionary's base, the packed bits as the index runs' data. That is most of a
//! store, and it stays in the file, read by the OS as queries touch it, instead of being
//! copied to the heap and hashed again. A running engine does the same with a checkpoint
//! it has just written ([`map_written`]), so after a bulk load or a background checkpoint
//! memory holds what queries touch, as after a restart.
//!
//! Format 5 had the dictionary as `len u64 | (key_len u32, key)*` and no padding (it is
//! copied into memory); format 4 stored the stacks as quad lists (`quad_count u64 | (s p o
//! g)* | inferred_count u64 | (s p o)*`), and the indexes are then rebuilt. Both are still
//! read.
//!
//! A checkpoint is written from a [`Snapshot`], so writers keep committing while it is being
//! written. It is streamed to `checkpoint-<revision>.tmp`, synced, and atomically renamed
//! to `checkpoint-<revision>.nck`; a crash at any point leaves either the old or the new
//! checkpoint, never a partial one. Leftover `.tmp` files are deleted on open.

use std::borrow::Cow;
use std::fs::{self, File};
use std::io::{BufWriter, Seek, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use nrese_exec::{Budget, BudgetExceeded, ChargedVec, Reservation, SharedBudget};
use rayon::prelude::*;

use super::codec::{Reader, put_u32, put_u64};
use crate::engine::{Snapshot, Stack};
use crate::error::{EngineError, EngineResult};
use crate::index::keys::PackedKeys;
use crate::index::{IndexVersion, Layout};
use crate::mapped::{Map, Mapped};
use crate::quad::{EncodedQuad, EncodedTriple, Permutation};
use crate::term::Dictionary;
use crate::term::dictionary::{Base, base_slots};
use crate::term::hash::key_hash;
use crate::term::offsets::{BlockEnds, Ends, Offsets};
use crate::term::vocabulary::{Codec, Trained, VocabularyEncoding, vocabulary_encoding};

/// Version 2 added the inferred stack (roadmap E6); version 3 changed term encoding (E1);
/// version 4 made integer ids order-preserving and split literal kinds (XC1); version 5
/// stores the packed permutations (Pf2); version 6 aligns them for mapping; version 7 adds
/// the dictionary's text order; version 8 the flags (integer-derived literals inline);
/// version 9 lets the asserted stack have the default-graph layout; version 10 lets blocks
/// of the packed permutations store a position as a palette; version 11 stores the
/// dictionary's offsets in blocks; version 12 the last commit's time (EEV-06).
const MAGIC: &[u8; 8] = b"NRESECKC";
/// Format 11: format 12 without the commit time.
const MAGIC_V11: &[u8; 8] = b"NRESECKB";
/// Format 10: 8-byte dictionary end offsets.
const MAGIC_V10: &[u8; 8] = b"NRESECKA";
/// Formats 9 and 8 read as 10: their blocks have no palettes (the byte was zero padding).
const MAGIC_V9: &[u8; 8] = b"NRESECK9";
const MAGIC_V8: &[u8; 8] = b"NRESECK8";
/// Earlier formats, still read (their integer-derived literals are in the dictionary):
/// without the flags, without the text order, unaligned packed permutations, quad lists.
const MAGIC_V7: &[u8; 8] = b"NRESECK7";
const MAGIC_V6: &[u8; 8] = b"NRESECK6";
/// The commit time's top bit (format 12): the revision holds commits of unknown time, and the
/// rest is only the store's commit clock. Times since 1970 in microseconds never reach it.
const UNKNOWN_TIME: u64 = 1 << 63;
/// Flags bit: integer-derived literals are dictionary entries.
const INTEGERS_IN_DICTIONARY: u64 = 1;
/// Flags bit: the dictionary's keys are compressed (format 11).
const COMPRESSED_KEYS: u64 = 2;
const MAGIC_V5: &[u8; 8] = b"NRESECK5";
const MAGIC_V4: &[u8; 8] = b"NRESECK4";
/// Magic prefix shared by every checkpoint format version.
const FAMILY: &[u8; 7] = b"NRESECK";
/// The file writer's buffer.
const WRITER: usize = 1 << 20;
/// What the writer's buffer holds before it is written out; it is made twice this, so that
/// nothing appended between two drains grows it (a key that doesn't fit is written past it).
/// The file writer's size: a flush that large goes past `BufWriter` to the file instead of
/// being copied into it (at 64 KiB, every key was copied once more: YAGO's checkpoint
/// dictionary pass 8 % slower, w6-ckpt-12).
const BUFFER: usize = WRITER;
const EXTENSION: &str = "nck";
/// Dictionary keys copied per lock acquisition, so interning is never blocked for long.
const KEY_CHUNK: u64 = 64 * 1024;

fn checkpoint_path(dir: &Path, revision: u64, extension: &str) -> PathBuf {
    dir.join(format!("checkpoint-{revision:020}.{extension}"))
}

/// The revision of the checkpoint (or image) file named `name`: `checkpoint-<revision>.nck`,
/// a plain file name; `None` for any other name.
pub fn image_revision(name: &str) -> Option<u64> {
    let stem = name
        .strip_prefix("checkpoint-")?
        .strip_suffix(&format!(".{EXTENSION}"))?;
    // Digits only: `parse` would take a sign.
    match !stem.is_empty() && stem.bytes().all(|b| b.is_ascii_digit()) {
        true => stem.parse().ok(),
        false => None,
    }
}

fn list(dir: &Path) -> EngineResult<Vec<(u64, PathBuf)>> {
    let mut found = Vec::new();
    for entry in fs::read_dir(dir)? {
        let path = entry?.path();
        let revision = path
            .file_name()
            .and_then(|name| name.to_str())
            .and_then(image_revision);
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
struct Checksummed<'b, W> {
    inner: W,
    crc: crc32fast::Hasher,
    buffer: Vec<u8>,
    /// Bytes handed to `inner` so far.
    written: u64,
    /// The charge of `inner`'s and `buffer`'s room: after them, released once they are freed.
    _held: Reservation<&'b Budget>,
}

impl<W: Write> Checksummed<'_, W> {
    /// Writes and checksums the scratch buffer.
    fn flush_buffer(&mut self) -> std::io::Result<()> {
        self.crc.update(&self.buffer);
        self.inner.write_all(&self.buffer)?;
        self.written += self.buffer.len() as u64;
        self.buffer.clear();
        Ok(())
    }

    /// Writes and checksums `bytes` after the scratch buffer.
    fn write_bytes(&mut self, bytes: &[u8]) -> std::io::Result<()> {
        self.flush_buffer()?;
        self.crc.update(bytes);
        self.written += bytes.len() as u64;
        self.inner.write_all(bytes)
    }

    /// The file position the next byte lands at.
    fn position(&self) -> u64 {
        self.written + self.buffer.len() as u64
    }

    /// Zero bytes up to the next 8-byte boundary of the file.
    fn pad(&mut self) {
        let padding = (8 - self.position() % 8) % 8;
        self.buffer.resize(self.buffer.len() + padding as usize, 0);
    }

    /// Writes the buffer out once it is large.
    fn drain(&mut self) -> std::io::Result<()> {
        if self.buffer.len() >= BUFFER {
            self.flush_buffer()?;
        }
        Ok(())
    }

    /// Appends `bytes` without growing the buffer: into it where they fit, else written
    /// after it.
    fn append(&mut self, bytes: &[u8]) -> std::io::Result<()> {
        if bytes.len() > self.buffer.capacity() - self.buffer.len() {
            return self.write_bytes(bytes);
        }
        self.buffer.extend_from_slice(bytes);
        self.drain()
    }
}

/// Writes the dictionary section (module docs) of the entries `0..len` but its text order
/// ([`write_text_order`]), reading the dictionary in chunks so interning is never blocked
/// for long. Its scratch, which grows with the whole vocabulary however small the change
/// since the last checkpoint, is charged to `admission` before it is allocated: the keys
/// decoded and compressed, the hash table and the offsets.
fn write_dictionary<W: Write>(
    out: &mut Checksummed<'_, W>,
    dictionary: &Dictionary,
    len: u64,
    codec: Option<&Codec>,
    admission: &Budget,
) -> EngineResult<()> {
    if len >= u64::from(u32::MAX) {
        return Err(EngineError::Configuration(
            "a checkpoint holds at most 2^32 - 1 dictionary entries".to_owned(),
        ));
    }
    let chunks = || {
        (0..len.div_ceil(KEY_CHUNK)).map(move |c| (c * KEY_CHUNK, ((c + 1) * KEY_CHUNK).min(len)))
    };
    let started = std::time::Instant::now();
    let (slots, lengths) = hashed(dictionary, len, codec, admission)?;
    let arena_len: u64 = lengths.iter().map(|&n| u64::from(n)).sum();
    let slot_count = slots.len();
    let (blocks, ends) = offsets(&lengths, admission)?;
    drop(lengths);
    let hashed = started.elapsed();
    put_u64(&mut out.buffer, len);
    put_u64(&mut out.buffer, arena_len);
    put_u64(&mut out.buffer, slot_count as u64);
    put_u64(&mut out.buffer, ends.len() as u64);
    if let Some(codec) = codec {
        codec.write(&mut out.buffer);
    }
    out.pad();
    for &block in blocks.iter() {
        put_u64(&mut out.buffer, block);
        out.drain()?;
    }
    drop(blocks);
    let (mut decoded, mut stored) = (ChargedVec::new(admission), ChargedVec::new(admission));
    for (from, to) in chunks() {
        dictionary.for_each_key(from, to, &mut decoded, |key| -> EngineResult<()> {
            let key = match codec {
                None => key,
                Some(codec) => {
                    stored.clear();
                    codec.compress_within(key, &mut stored)?;
                    &stored
                }
            };
            Ok(out.append(key)?)
        })?;
    }
    drop((decoded, stored));
    out.write_bytes(&ends)?;
    drop(ends);
    out.pad();
    for &slot in slots.iter() {
        put_u32(&mut out.buffer, slot);
        out.drain()?;
    }
    drop(slots);
    tracing::debug!(
        entries = len,
        hash_ms = hashed.as_millis() as u64,
        keys_and_slots_ms = (started.elapsed() - hashed).as_millis() as u64,
        "checkpoint dictionary written"
    );
    Ok(())
}

/// Writes the end of the dictionary section: the entries `0..len` with a text, sorted by it
/// ([`crate::term::order`]). Its sort and the order are charged to `admission` before they
/// are allocated ([`Dictionary::text_order`]), and held until the order is written.
fn write_text_order<W: Write>(
    out: &mut Checksummed<'_, W>,
    dictionary: &Dictionary,
    len: u64,
    admission: &Budget,
) -> EngineResult<()> {
    let started = std::time::Instant::now();
    let order = dictionary.text_order(len, admission)?;
    let ordered = started.elapsed();
    out.pad();
    put_u64(&mut out.buffer, order.len() as u64);
    for &index in order.iter() {
        put_u32(&mut out.buffer, index);
        out.drain()?;
    }
    drop(order);
    tracing::debug!(
        entries = len,
        text_order_ms = ordered.as_millis() as u64,
        order_write_ms = (started.elapsed() - ordered).as_millis() as u64,
        "checkpoint text order written"
    );
    Ok(())
}

/// The block offsets of keys of `lengths` ([`crate::term::offsets::encode`]), each vector
/// charged to `admission` for its exact capacity before it is allocated.
fn offsets<'b>(
    lengths: &[u32],
    admission: &'b Budget,
) -> EngineResult<(Scratch<'b, u64>, Scratch<'b, u8>)> {
    crate::term::offsets::encode(lengths, admission)?.ok_or_else(|| {
        EngineError::Configuration(
            "64 consecutive dictionary keys take 4 GiB or more; a checkpoint can't hold them"
                .to_owned(),
        )
    })
}

/// A checkpoint's scratch, charged to its admission.
type Scratch<'b, T> = ChargedVec<T, &'b Budget>;

/// The hash table of the entries `0..len` (each slot the entry's index + 1, or 0) and their
/// stored lengths, every array charged to `admission` before it is made.
///
/// Each key is read once, on every core, for its stored length and its home slot; the table
/// is then filled in slot order, each entry at the first free slot from its home (what
/// linear probing finds), in one sweep over the table instead of a cache miss per entry.
/// Lookups don't depend on the order the entries went in.
fn hashed<'b>(
    dictionary: &Dictionary,
    len: u64,
    codec: Option<&Codec>,
    admission: &'b Budget,
) -> EngineResult<(Scratch<'b, u32>, Scratch<'b, u32>)> {
    let slot_count = base_slots(len) as usize;
    let mask = slot_count - 1;
    let n = len as usize;
    let mut slots: ChargedVec<u32, &Budget> = ChargedVec::with_capacity(admission, slot_count)?;
    slots.resize(slot_count, 0)?;
    let mut lengths: ChargedVec<u32, &Budget> = ChargedVec::with_capacity(admission, n)?;
    // A compressed key's stored length: compressed into `stored`, its room charged first.
    let stored_len = |key: &[u8], stored: &mut Scratch<'b, u8>| {
        let Some(codec) = codec else {
            return Ok(key.len());
        };
        stored.clear();
        codec.compress_within(key, stored)?;
        Ok::<_, BudgetExceeded>(stored.len())
    };
    if slot_count <= 1 << 32 {
        let mut placed: ChargedVec<u64, &Budget> = ChargedVec::with_capacity(admission, n)?;
        placed.resize(n, 0)?;
        lengths.resize(n, 0)?;
        let chunk = KEY_CHUNK as usize;
        // Each chunk's count summed as they end: nothing collected.
        let total = placed
            .par_chunks_mut(chunk)
            .zip(lengths.par_chunks_mut(chunk))
            .enumerate()
            .map(|(c, (homes, lens))| {
                let from = (c * chunk) as u64;
                let mut i = 0;
                let (mut decoded, mut stored) =
                    (ChargedVec::new(admission), ChargedVec::new(admission));
                dictionary.for_each_key(from, from + homes.len() as u64, &mut decoded, |key| {
                    let length = stored_len(key, &mut stored)?;
                    let home = (key_hash(key) as usize & mask) as u64;
                    homes[i] = home << 32 | (from + i as u64);
                    lens[i] = length as u32;
                    i += 1;
                    Ok::<_, BudgetExceeded>(())
                })?;
                Ok::<_, BudgetExceeded>(i as u64)
            })
            .try_reduce(|| 0, |a, b| Ok(a + b))?;
        if total != len {
            return Err(EngineError::Corruption(
                "the dictionary is shorter than the snapshot".to_owned(),
            ));
        }
        placed.par_sort_unstable();
        let mut next = 0usize;
        let mut wrapped: ChargedVec<u32, &Budget> = ChargedVec::new(admission);
        for &entry in placed.iter() {
            let (home, index) = ((entry >> 32) as usize, (entry & 0xffff_ffff) as u32);
            let slot = home.max(next);
            if slot >= slot_count {
                // Past the end, probing wraps around to the start.
                wrapped.push(index)?;
                continue;
            }
            slots[slot] = index + 1;
            next = slot + 1;
        }
        drop(placed);
        let mut slot = 0;
        for &index in wrapped.iter() {
            while slots[slot] != 0 {
                slot += 1;
            }
            slots[slot] = index + 1;
        }
    } else {
        let (mut decoded, mut stored) = (ChargedVec::new(admission), ChargedVec::new(admission));
        for (from, to) in
            (0..len.div_ceil(KEY_CHUNK)).map(|c| (c * KEY_CHUNK, ((c + 1) * KEY_CHUNK).min(len)))
        {
            dictionary.for_each_key(from, to, &mut decoded, |key| {
                let mut slot = key_hash(key) as usize & mask;
                while slots[slot] != 0 {
                    slot = (slot + 1) & mask;
                }
                lengths.push(stored_len(key, &mut stored)? as u32)?;
                slots[slot] = lengths.len() as u32;
                Ok::<_, BudgetExceeded>(())
            })?;
        }
        if lengths.len() as u64 != len {
            return Err(EngineError::Corruption(
                "the dictionary is shorter than the snapshot".to_owned(),
            ));
        }
    }
    Ok((slots, lengths))
}

/// Reads the dictionary section (module docs) from `reader`, positioned after the
/// revision, as a [`Base`] of `map`; `with_order` from format 7 on, `blocks` (block
/// offsets) from format 11 on.
fn read_dictionary(
    reader: &mut Reader<'_>,
    map: &Map,
    with_order: bool,
    blocks: bool,
    compressed: bool,
) -> Result<Base, String> {
    let truncated = || "truncated dictionary".to_owned();
    let len = reader.u64().ok_or_else(truncated)?;
    let arena_len = reader.u64().ok_or_else(truncated)?;
    let slot_count = reader.u64().ok_or_else(truncated)?;
    let ends_len = match blocks {
        true => reader.u64().ok_or_else(truncated)?,
        false => 0,
    };
    let codec = match compressed {
        true => Some(
            Codec::read(
                reader
                    .bytes(crate::term::vocabulary::CODEC_BYTES)
                    .ok_or_else(truncated)?,
            )
            .ok_or_else(|| "damaged symbol tables".to_owned())?,
        ),
        false => None,
    };
    if len >= u64::from(u32::MAX) || !slot_count.is_power_of_two() || slot_count <= len {
        return Err("damaged dictionary header".into());
    }
    let mut section = |count: u64, size: u64| -> Result<usize, String> {
        let bytes = reader
            .bytes(
                usize::try_from(count.checked_mul(size).ok_or_else(truncated)?)
                    .map_err(|_| truncated())?,
            )
            .ok_or_else(truncated)?;
        Ok(bytes.as_ptr() as usize - map.as_ptr() as usize)
    };
    let padding = |at: usize| ((8 - at % 8) % 8) as u64;
    let at = section(0, 1)?;
    section(padding(at), 1)?;
    let unmappable = || "the dictionary can't be mapped on this machine".to_owned();
    let (offsets, arena_at) = if blocks {
        let block_count = len.div_ceil(crate::term::offsets::BLOCK as u64);
        let blocks_at = section(2 * block_count, 8)?;
        let arena_at = section(arena_len, 1)?;
        let ends_at = section(ends_len, 1)?;
        section(padding(ends_at + ends_len as usize), 1)?;
        let offsets = Offsets::Blocks(BlockEnds {
            count: len as usize,
            blocks: Mapped::new(map, blocks_at, 2 * block_count as usize).ok_or_else(unmappable)?,
            ends: Mapped::new(map, ends_at, ends_len as usize).ok_or_else(unmappable)?,
        });
        (offsets, arena_at)
    } else {
        let ends_at = section(len, 8)?;
        let arena_at = section(arena_len, 1)?;
        section(padding(arena_at + arena_len as usize), 1)?;
        let ends = Mapped::new(map, ends_at, len as usize).ok_or_else(unmappable)?;
        (Offsets::Ends(ends), arena_at)
    };
    let slots_at = section(slot_count, 4)?;
    let order = if with_order {
        let at = section(0, 1)?;
        section(padding(at), 1)?;
        let at = section(1, 8)?;
        let count = u64::from_le_bytes(map[at..at + 8].try_into().expect("8 bytes"));
        if count > len {
            return Err("the dictionary's text order is longer than the dictionary".into());
        }
        Some((section(count, 4)?, count as usize))
    } else {
        None
    };
    let base = Base {
        len,
        arena: Mapped::new(map, arena_at, arena_len as usize).ok_or_else(unmappable)?,
        offsets,
        slots: Mapped::new(map, slots_at, slot_count as usize).ok_or_else(unmappable)?,
        order: match order {
            Some((at, count)) => Some(Mapped::new(map, at, count).ok_or_else(unmappable)?),
            None => None,
        },
        codec,
    };
    // The last block's start and the last key's end, checked cheaply; `Base::verify`
    // reads every offset.
    let last_end = match (&base.offsets, len) {
        (_, 0) => 0,
        (Offsets::Ends(ends), _) => ends.last().copied().unwrap_or(0) as usize,
        (Offsets::Blocks(blocks), _) => {
            let block = (len as usize - 1) / crate::term::offsets::BLOCK;
            let at = blocks.blocks[2 * block + 1];
            let width = if at & 1 == 1 { 4 } else { 2 };
            let needed =
                (at >> 1) as usize + width * ((len as usize - 1) % crate::term::offsets::BLOCK + 1);
            if needed > blocks.ends.len() {
                return Err("dictionary block offsets point past their ends".into());
            }
            blocks.end(len as usize - 1)
        }
    };
    if last_end as u64 != arena_len {
        return Err("dictionary end offsets don't match its keys".into());
    }
    Ok(base)
}

/// Whether there is a checkpoint of `revision` (its content is that revision's: a committed
/// revision is never reused).
fn out_written(path: &Path) -> u64 {
    fs::metadata(path).map_or(0, |m| m.len())
}

pub(crate) fn exists(dir: &Path, revision: u64) -> bool {
    checkpoint_path(dir, revision, EXTENSION).exists()
}

/// Whether the checkpoint of `revision` is in place under its name; an error where that
/// can't be told.
pub(crate) fn placed(dir: &Path, revision: u64) -> std::io::Result<bool> {
    checkpoint_path(dir, revision, EXTENSION).try_exists()
}

/// A checkpoint's times: its version's (`None`: it holds commits of unknown time) and the
/// store's commit clock, no earlier than any commit in it.
pub(crate) type Times = (Option<u64>, u64);

/// The format 12 trailer word of `times`; a time past [`super::MAX_COMMIT_TIME`] (which
/// recovery and replication refuse, so none reaches here) fails the write rather than being
/// kept short.
fn trailer((committed, clock): Times) -> EngineResult<u64> {
    let time = committed.unwrap_or(clock);
    if time > super::MAX_COMMIT_TIME {
        return Err(EngineError::Corruption(format!(
            "a commit time past what a checkpoint keeps ({time})"
        )));
    }
    Ok(match committed {
        Some(committed) => committed,
        None => clock | UNKNOWN_TIME,
    })
}

/// The times of a format 12 trailer word (0: neither known).
fn times_of(trailer: u64) -> Times {
    match (trailer, trailer & UNKNOWN_TIME) {
        (0, _) => (None, 0),
        (_, 0) => (Some(trailer), trailer),
        _ => (None, trailer & !UNKNOWN_TIME),
    }
}

/// Writes a checkpoint of `snapshot`, of a committed revision (its version's time; `clock`,
/// the store's commit clock, read with or after the snapshot), and returns its path; its
/// scratch is charged within `admission` ([`write_parts`]).
pub(crate) fn write(
    dir: &Path,
    snapshot: &Snapshot,
    clock: u64,
    admission: Option<&Arc<SharedBudget>>,
) -> EngineResult<PathBuf> {
    let committed_micros = snapshot.version().committed_micros;
    write_stamped(dir, snapshot, admission, &mut || (committed_micros, clock))
}

/// [`write`] of a revision this checkpoint commits: its times are `stamp`'s, asked for once
/// everything else is written ([`write_parts`]).
pub(crate) fn write_stamped(
    dir: &Path,
    snapshot: &Snapshot,
    admission: Option<&Arc<SharedBudget>>,
    stamp: &mut dyn FnMut() -> Times,
) -> EngineResult<PathBuf> {
    // A private snapshot's own ids mean terms the dictionary doesn't have.
    let Some(dictionary) = snapshot.shared_dictionary() else {
        return Err(EngineError::Configuration(
            "a private snapshot has no published state to checkpoint".to_owned(),
        ));
    };
    let version = snapshot.version();
    write_parts(
        dir,
        admission,
        snapshot.revision(),
        stamp,
        dictionary,
        snapshot.dictionary_len(),
        [version.asserted.layout(), version.inferred.layout()],
        &mut |stack, permutation| Ok(snapshot.version().stack(stack).packed(permutation)),
    )
}

/// Writes a checkpoint of `revision` from its parts and returns its path. Its times
/// ([`Times`]) are `stamp`'s, asked for once the parts are written, just before the
/// file is synced and renamed into place: for a bulk load the checkpoint commits, and the time
/// is then that of its commit, not of the build before it (as a transaction's is taken just
/// before its record is synced). It holds the dictionary's
/// entries `0..dictionary_len`, and each stack's permutations as `packed` gives them, in
/// the order of the stack's layout (`layouts`: asserted, inferred). Each is written before the next is asked for, so a
/// caller can build one at a time and drop it (bulk loads).
///
/// The scratch of the dictionary's section is charged to a budget within `admission`, the
/// process's root (`EngineConfig::admission`; `None`: no limit), before it is allocated:
/// refused, it fails with [`EngineError::MemoryLimit`] and publishes nothing.
#[expect(
    clippy::too_many_arguments,
    reason = "one per header field and part, each given apart by its two callers"
)]
pub(crate) fn write_parts<'a>(
    dir: &Path,
    admission: Option<&Arc<SharedBudget>>,
    revision: u64,
    stamp: &mut dyn FnMut() -> Times,
    dictionary: &Dictionary,
    dictionary_len: u64,
    layouts: [Layout; 2],
    packed: &mut dyn FnMut(Stack, Permutation) -> EngineResult<Cow<'a, PackedKeys>>,
) -> EngineResult<PathBuf> {
    let admission = Budget::unlimited().within(admission.cloned());
    // The writer's buffers, and keys compressed with tables trained on a sample of them
    // ([`crate::term::vocabulary`]), before the file is made.
    let mut held = Reservation::new(&admission);
    held.charge(WRITER + 2 * BUFFER)?;
    let trained = match vocabulary_encoding() {
        VocabularyEncoding::Fsst => {
            let sample = dictionary.sample_within(dictionary_len, &admission)?;
            Some(Codec::train_within(&sample, &admission)?)
        }
        VocabularyEncoding::Plain => None,
    };
    let codec = trained.as_ref().map(Trained::codec);
    let tmp = checkpoint_path(dir, revision, "tmp");
    let mut file = File::create(&tmp)?;
    // The magic is written last, once everything else is on file; it is checksummed apart
    // and the checksums combined.
    file.write_all(MAGIC)?;
    let mut out = Checksummed {
        inner: BufWriter::with_capacity(WRITER, file),
        crc: crc32fast::Hasher::new(),
        buffer: Vec::with_capacity(2 * BUFFER),
        written: MAGIC.len() as u64,
        _held: held,
    };
    put_u64(&mut out.buffer, revision);
    let flags = match dictionary.integers_in_dictionary() {
        true => INTEGERS_IN_DICTIONARY,
        false => 0,
    } | match codec {
        Some(_) => COMPRESSED_KEYS,
        None => 0,
    };
    put_u64(&mut out.buffer, flags);
    // Where the time goes, for the load log (bulk loads build the permutations here).
    let started = std::time::Instant::now();
    write_dictionary(&mut out, dictionary, dictionary_len, codec, &admission)?;
    write_text_order(&mut out, dictionary, dictionary_len, &admission)?;
    let dictionary_ms = started.elapsed().as_millis() as u64;
    let (mut build, mut write) = (std::time::Duration::ZERO, std::time::Duration::ZERO);
    for (stack, layout) in Stack::ALL.into_iter().zip(layouts) {
        let permutations = layout.permutations();
        put_u32(&mut out.buffer, permutations.len() as u32);
        for &permutation in permutations {
            out.buffer.push(permutation as u8);
            let asked = std::time::Instant::now();
            let keys = packed(stack, permutation)?;
            build += asked.elapsed();
            let writing = std::time::Instant::now();
            let at = out.position();
            keys.write(Some(at), &mut |piece| out.write_bytes(piece))?;
            write += writing.elapsed();
        }
    }
    #[cfg(test)]
    if let Some(at) = super::fault::CLOCK_AT_TRAILER.get() {
        super::fault::CLOCK.set(Some(at));
    }
    // The last commit's time (format 12), 0 where it isn't known.
    put_u64(&mut out.buffer, trailer(stamp())?);
    out.flush_buffer()?;
    let syncing = std::time::Instant::now();
    let magic = MAGIC;
    let mut crc = crc32fast::Hasher::new();
    crc.update(magic);
    crc.combine(&out.crc);
    let crc = crc.finalize();
    let mut inner = out.inner;
    inner.write_all(&crc.to_le_bytes())?;
    let mut file = inner.into_inner().map_err(|error| error.into_error())?;
    file.seek(std::io::SeekFrom::Start(0))?;
    file.write_all(magic)?;
    let flushed = syncing.elapsed();
    file.sync_all()?;
    drop(file);
    let path = checkpoint_path(dir, revision, EXTENSION);
    super::place::rename(&tmp, &path)?;
    #[cfg(test)]
    super::fault::check(&super::fault::FAIL_DIR_SYNC, "directory sync")?;
    super::sync_dir(dir)?;
    tracing::debug!(
        revision,
        bytes = out_written(&path),
        dictionary_ms,
        permutations_build_ms = build.as_millis() as u64,
        permutations_write_ms = write.as_millis() as u64,
        flush_ms = flushed.as_millis() as u64,
        sync_ms = (syncing.elapsed() - flushed).as_millis() as u64,
        "checkpoint written"
    );
    Ok(path)
}

/// The index of `stack` from a checkpoint's packed permutations: the inferred stack in the
/// default-graph layout, the asserted one in either.
pub(crate) fn stack_index(
    stack: Stack,
    packed: Vec<(Permutation, PackedKeys)>,
) -> Result<IndexVersion, String> {
    let index = IndexVersion::from_packed(packed)?;
    match (stack, index.layout()) {
        (Stack::Inferred, Layout::Quads) => Err("the inferred stack holds named graphs".into()),
        _ => Ok(index),
    }
}

/// A loaded checkpoint: its revision and both stacks; the dictionary is restored in place.
pub(crate) struct Loaded {
    pub revision: u64,
    /// When its last commit was made (format 12); `None` where that isn't known.
    pub committed_micros: Option<u64>,
    /// The store's commit clock as it was then (format 12; 0 before).
    pub clock_micros: u64,
    pub stacks: Stacks,
    /// The store keeps integer-derived literals in its dictionary.
    pub integers_in_dictionary: bool,
}

/// The index stacks of a checkpoint.
pub(crate) enum Stacks {
    /// Format 5: the packed permutations of each stack (asserted, inferred).
    Packed([Vec<(Permutation, PackedKeys)>; 2]),
    /// Format 4: quad lists, to be indexed again.
    Quads {
        quads: Vec<EncodedQuad>,
        inferred: Vec<EncodedTriple>,
    },
}

/// Loads the newest checkpoint into `dictionary`, if there is one. Format 6 is used in
/// place, and only its structure is checked unless `verify` (then also the CRC, every
/// index block and every dictionary key); older formats are copied and always checked.
pub(crate) fn load_latest(
    dir: &Path,
    dictionary: &Dictionary,
    verify: bool,
) -> EngineResult<Option<Loaded>> {
    let Some((_, path)) = list(dir)?.pop() else {
        return Ok(None);
    };
    let clock = std::time::Instant::now();
    let lap = |what: &str| {
        tracing::debug!(
            part = what,
            seconds = clock.elapsed().as_secs_f64(),
            "recovery: checkpoint part"
        );
    };
    // Mapped: the packed permutations are used in place (format 6).
    let map = crate::mapped::map(&path)?;
    let bytes: &[u8] = &map;
    lap("file mapped");
    let corrupt =
        |what: &str| EngineError::Corruption(format!("checkpoint {}: {what}", path.display()));
    let (body, crc) = bytes
        .split_at_checked(bytes.len().saturating_sub(4))
        .ok_or_else(|| corrupt("truncated"))?;
    let mut reader = Reader::new(body);
    let mut blocks = false;
    let mut timed = false;
    let (packed, aligned, with_order, with_flags) = match reader.bytes(MAGIC.len()) {
        Some(magic) if magic == MAGIC || magic == MAGIC_V11 => {
            blocks = true;
            timed = magic == MAGIC;
            (true, true, true, true)
        }
        Some(magic) if magic == MAGIC_V10 || magic == MAGIC_V9 || magic == MAGIC_V8 => {
            (true, true, true, true)
        }
        Some(magic) if magic == MAGIC_V7 => (true, true, true, false),
        Some(magic) if magic == MAGIC_V6 => (true, true, false, false),
        Some(magic) if magic == MAGIC_V5 => (true, false, false, false),
        Some(magic) if magic == MAGIC_V4 => (false, false, false, false),
        Some(magic) if magic.starts_with(FAMILY) => {
            return Err(EngineError::UnsupportedFormat(path));
        }
        _ => return Err(corrupt("bad magic")),
    };
    let verify = verify || !aligned;
    if verify
        && (crc.len() != 4
            || crc32fast::hash(body) != u32::from_le_bytes(crc.try_into().expect("4 bytes")))
    {
        return Err(corrupt("checksum mismatch"));
    }
    lap("crc");
    // Format 12: the commit times end the body; the rest is read as format 11.
    let (mut committed_micros, mut clock_micros) = (None, 0);
    if timed {
        let (rest, time) = body
            .split_at_checked(body.len().saturating_sub(8))
            .filter(|(rest, _)| rest.len() >= MAGIC.len())
            .ok_or_else(|| corrupt("truncated"))?;
        (committed_micros, clock_micros) =
            times_of(u64::from_le_bytes(time.try_into().expect("8 bytes")));
        reader = Reader::new(rest);
        reader.bytes(MAGIC.len());
    }
    let revision = reader.u64().ok_or_else(|| corrupt("truncated header"))?;
    let (integers_in_dictionary, compressed) = match with_flags {
        true => {
            let flags = reader.u64().ok_or_else(|| corrupt("truncated header"))?;
            let known = INTEGERS_IN_DICTIONARY | if blocks { COMPRESSED_KEYS } else { 0 };
            if flags & !known != 0 {
                return Err(corrupt("unknown flags"));
            }
            (
                flags & INTEGERS_IN_DICTIONARY != 0,
                flags & COMPRESSED_KEYS != 0,
            )
        }
        false => (true, false),
    };
    if aligned {
        let base = read_dictionary(&mut reader, &map, with_order, blocks, compressed)
            .map_err(|error| corrupt(&error))?;
        if verify {
            base.verify().map_err(|error| corrupt(&error))?;
        }
        dictionary.restore_base(base)?;
    } else {
        let dictionary_len = reader.u64().ok_or_else(|| corrupt("truncated header"))?;
        let count = usize::try_from(dictionary_len).map_err(|_| corrupt("dictionary too large"))?;
        if count > reader.remaining() / 4 {
            return Err(corrupt("dictionary length exceeds file size"));
        }
        let mut keys: Vec<&[u8]> = Vec::with_capacity(count);
        for _ in 0..count {
            let len = reader
                .u32()
                .ok_or_else(|| corrupt("truncated dictionary"))?;
            let key = reader
                .bytes(len as usize)
                .ok_or_else(|| corrupt("truncated dictionary"))?;
            keys.push(key);
        }
        dictionary.restore_keys(&keys)?;
    }
    lap("dictionary");
    if packed {
        let stacks = read_stacks(&mut reader, bytes, aligned.then_some(&map), verify)
            .map_err(|error| corrupt(&error))?;
        lap("packed permutations");
        return Ok(Some(Loaded {
            revision,
            committed_micros,
            clock_micros,
            stacks: Stacks::Packed(stacks),
            integers_in_dictionary,
        }));
    }
    let count = reader.u64().ok_or_else(|| corrupt("truncated quads"))?;
    if count.saturating_mul(32) > reader.remaining() as u64 {
        return Err(corrupt("quad count exceeds file size"));
    }
    let quads = (0..count)
        .map(|_| reader.quad())
        .collect::<Option<Vec<_>>>()
        .ok_or_else(|| corrupt("truncated quads"))?;
    let count = reader.u64().ok_or_else(|| corrupt("truncated inferred"))?;
    if count.saturating_mul(24) != reader.remaining() as u64 {
        return Err(corrupt("inferred count does not match file size"));
    }
    let inferred = (0..count)
        .map(|_| reader.triple())
        .collect::<Option<Vec<_>>>()
        .ok_or_else(|| corrupt("truncated inferred"))?;
    Ok(Some(Loaded {
        revision,
        committed_micros: None,
        clock_micros: 0,
        stacks: Stacks::Quads { quads, inferred },
        integers_in_dictionary,
    }))
}

/// Reads the stacks (module docs) from `reader`, positioned after the dictionary, to the
/// end of `bytes`, the file's body: used in place from `map` where given (format 6 on), else
/// copied and checked.
fn read_stacks<'a>(
    reader: &mut Reader<'a>,
    bytes: &'a [u8],
    map: Option<&Map>,
    verify: bool,
) -> Result<[Vec<(Permutation, PackedKeys)>; 2], String> {
    let mut stacks: [Vec<(Permutation, PackedKeys)>; 2] = Default::default();
    for stack in &mut stacks {
        let count = reader.u32().ok_or("truncated stack")?;
        for _ in 0..count {
            let id = reader.bytes(1).ok_or("truncated stack")?[0];
            let permutation = *Permutation::ALL
                .get(id as usize)
                .ok_or("unknown permutation")?;
            let rest = reader.bytes(reader.remaining()).expect("remaining bytes");
            let at = (rest.as_ptr() as usize - bytes.as_ptr() as usize) as u64;
            let (keys, used) = match map {
                Some(map) => PackedKeys::read(rest, Some(at), Some(map), verify),
                None => PackedKeys::read(rest, None, None, true),
            }?;
            *reader = Reader::new(&rest[used..]);
            stack.push((permutation, keys));
        }
    }
    if !reader.is_done() {
        return Err("trailing bytes".into());
    }
    Ok(stacks)
}

/// The checkpoint at `path`, which this engine has just written, mapped: its dictionary
/// base and both stacks, each one run, to be used in place of the copies in memory. Only
/// the structure is read (the file was checked as it was written).
pub(crate) fn map_written(path: &Path) -> EngineResult<(Base, [IndexVersion; 2])> {
    let corrupt =
        |what: &str| EngineError::Corruption(format!("checkpoint {}: {what}", path.display()));
    #[cfg(test)]
    super::fault::check(&super::fault::FAIL_MAP, "mapping")?;
    let map = crate::mapped::map(path)?;
    let bytes: &[u8] = &map;
    let mut body = &bytes[..bytes.len().saturating_sub(4)];
    // Format 12 ends its body in the commit time, which the version carries already.
    if body.starts_with(MAGIC) {
        body = &body[..body.len().saturating_sub(8)];
    }
    let mut reader = Reader::new(body);
    // Format 8 is format 9 with the quad layout only; 9 is 10 without palettes.
    let blocks = match reader.bytes(MAGIC.len()) {
        Some(magic) if magic == MAGIC || magic == MAGIC_V11 => true,
        Some(magic) if magic == MAGIC_V10 || magic == MAGIC_V9 || magic == MAGIC_V8 => false,
        _ => return Err(corrupt("bad magic")),
    };
    reader.u64().ok_or_else(|| corrupt("truncated header"))?;
    // The flags: this engine's, but whether the keys are compressed.
    let flags = reader.u64().ok_or_else(|| corrupt("truncated header"))?;
    let base = read_dictionary(
        &mut reader,
        &map,
        true,
        blocks,
        flags & COMPRESSED_KEYS != 0,
    )
    .map_err(|error| corrupt(&error))?;
    let [asserted, inferred] =
        read_stacks(&mut reader, body, Some(&map), false).map_err(|error| corrupt(&error))?;
    let index = |stack: Stack, packed| stack_index(stack, packed).map_err(|error| corrupt(&error));
    Ok((
        base,
        [
            index(Stack::Asserted, asserted)?,
            index(Stack::Inferred, inferred)?,
        ],
    ))
}

/// Deletes checkpoints older than `revision`. One still mapped can't be deleted on Windows
/// ([`crate::mapped`]); it stays until a later checkpoint, and recovery reads the newest.
pub(crate) fn remove_older_than(dir: &Path, revision: u64) -> EngineResult<()> {
    for (checkpoint_revision, path) in list(dir)? {
        if checkpoint_revision < revision
            && let Err(error) = fs::remove_file(&path)
        {
            tracing::debug!(%error, path = %path.display(), "older checkpoint kept for now");
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::sync::Arc;

    use nrese_exec::{Budget, Reservation, SharedBudget};
    use nrese_rdf::{BlankNodeRef, GraphName, Literal, LiteralRef, NamedNode, Quad};

    use super::Checksummed;
    use crate::term::Dictionary;
    use crate::term::vocabulary::{Codec, SAMPLE, Sample};

    /// `n` pseudo-random hex digits, from `seed`.
    fn hex(seed: u64, n: usize) -> String {
        let mut x = 0x9E37_79B9_7F4A_7C15u64 ^ seed;
        (0..n)
            .map(|_| {
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
                char::from_digit((x & 15) as u32, 16).unwrap()
            })
            .collect()
    }

    /// 70,000 keys over two of the hash's chunks: a block of 64 of them of 2,000
    /// pseudo-random hex digits each (a wide offset block even compressed), and one of
    /// 8,000,000 (compressed, longer than the writer's buffer), mapped from a section written
    /// with a codec trained on them (a compressed base: each key decoded as it is read), and
    /// 1,000 keys interned on top.
    fn dictionary(dir: &Path) -> Dictionary {
        let heap = Dictionary::default();
        for i in 0..70_000u64 {
            let tail = match i {
                6_400..6_464 => hex(i, 2_000),
                50_000 => hex(i, 8_000_000),
                _ => String::new(),
            };
            heap.intern(LiteralRef::new_simple_literal(&format!("word {i}{tail}")).into());
        }
        let codec = Codec::train(&heap.export_keys(0, heap.len()));
        let budget = Budget::unlimited();
        let mut out = Checksummed {
            inner: Vec::new(),
            crc: crc32fast::Hasher::new(),
            buffer: Vec::new(),
            written: 0,
            _held: Reservation::new(&budget),
        };
        super::write_dictionary(&mut out, &heap, heap.len(), Some(&codec), &budget).unwrap();
        super::write_text_order(&mut out, &heap, heap.len(), &budget).unwrap();
        out.flush_buffer().unwrap();
        let path = dir.join("dictionary");
        std::fs::write(&path, out.inner).unwrap();
        let map = crate::mapped::map(&path).unwrap();
        let base = super::read_dictionary(&mut super::Reader::new(&map), &map, true, true, true);
        let dictionary = Dictionary::default();
        dictionary.restore_base(base.unwrap()).unwrap();
        for i in 0..1_000 {
            dictionary.intern(LiteralRef::new_simple_literal(&format!("new {i}")).into());
        }
        dictionary
    }

    /// P2-01 A4. A view of the dictionary's keys taken before a rebase reads every key it
    /// held after it. The rebase moves the heap's suffix into a new heap: the view is then the
    /// only owner of the old heap's blocks, which go when it drops.
    #[test]
    fn a_view_taken_before_a_rebase_reads_its_keys_after_it() {
        let dir = tempfile::tempdir().unwrap();
        let dictionary = Dictionary::default();
        let n = 10_000u64;
        for i in 0..n {
            dictionary.intern(LiteralRef::new_simple_literal(&format!("word {i}")).into());
        }
        let keys = dictionary.export_keys(0, n);
        let held = dictionary.held_view();
        // A checkpoint of the first 6,000 entries, compressed, with its text order.
        let len = 6_000;
        let codec = Codec::train(&dictionary.export_keys(0, len));
        let budget = Budget::unlimited();
        let mut out = Checksummed {
            inner: Vec::new(),
            crc: crc32fast::Hasher::new(),
            buffer: Vec::new(),
            written: 0,
            _held: Reservation::new(&budget),
        };
        super::write_dictionary(&mut out, &dictionary, len, Some(&codec), &budget).unwrap();
        super::write_text_order(&mut out, &dictionary, len, &budget).unwrap();
        out.flush_buffer().unwrap();
        let path = dir.path().join("dictionary");
        std::fs::write(&path, out.inner).unwrap();
        let map = crate::mapped::map(&path).unwrap();
        let base =
            super::read_dictionary(&mut super::Reader::new(&map), &map, true, true, true).unwrap();
        assert!(dictionary.rebase(base).unwrap());
        assert!(
            held.sole_owner_of_its_heap(),
            "the rebase let go of the old heap's blocks"
        );
        for (i, key) in keys.iter().enumerate() {
            assert_eq!(&held.key(i as u64), key, "entry {i} through the view");
        }
        assert_eq!(dictionary.export_keys(0, n), keys);
        drop(held);
    }

    /// A mapped checkpoint of `dictionary`'s first `len` entries, compressed, with its text
    /// order: what a rebase serves them from.
    fn mapped_base(dictionary: &Dictionary, len: u64, dir: &Path) -> super::Base {
        let codec = Codec::train(&dictionary.export_keys(0, len));
        let budget = Budget::unlimited();
        let mut out = Checksummed {
            inner: Vec::new(),
            crc: crc32fast::Hasher::new(),
            buffer: Vec::new(),
            written: 0,
            _held: Reservation::new(&budget),
        };
        super::write_dictionary(&mut out, dictionary, len, Some(&codec), &budget).unwrap();
        super::write_text_order(&mut out, dictionary, len, &budget).unwrap();
        out.flush_buffer().unwrap();
        let path = dir.join("dictionary");
        std::fs::write(&path, out.inner).unwrap();
        let map = crate::mapped::map(&path).unwrap();
        super::read_dictionary(&mut super::Reader::new(&map), &map, true, true, true).unwrap()
    }

    /// Package 1's rebase interleavings (`notes/2026-11-08-p1-prepared-install-design.md`,
    /// T3–T5): a dictionary of `n` literals and a suffix of `added` more at its end.
    fn with_suffix(n: u64, added: usize) -> (Dictionary, crate::term::suffix::Suffix, Vec<String>) {
        let dictionary = Dictionary::default();
        for i in 0..n {
            dictionary.intern(LiteralRef::new_simple_literal(&format!("word {i}")).into());
        }
        let texts: Vec<String> = (0..added).map(|i| format!("added {i}")).collect();
        let scratch = Dictionary::default();
        for text in &texts {
            scratch.intern(LiteralRef::new_simple_literal(text).into());
        }
        let budget = Arc::new(Budget::unlimited());
        let mut suffix = crate::term::suffix::Suffix::at(dictionary.len(), budget);
        for key in scratch.export_keys(0, scratch.len()) {
            suffix
                .push(&key, crate::term::hash::key_hash(&key))
                .unwrap();
        }
        (dictionary, suffix, texts)
    }

    fn assert_added(dictionary: &Dictionary, n: u64, texts: &[String], row: &str) {
        for (i, text) in texts.iter().enumerate() {
            let id = dictionary
                .lookup(LiteralRef::new_simple_literal(text).into())
                .unwrap_or_else(|| panic!("{row}: {text} found"));
            assert_eq!(id.payload(), n + i as u64, "{row}: {text}'s id");
            assert_eq!(
                dictionary.decode(id),
                Some(Literal::new_simple_literal(text).into()),
                "{row}: {text} decodes"
            );
        }
    }

    /// T3. A rebase between a prepared install and the install keeps the room it promised: the
    /// install still allocates nothing, and every entry, old and added, is where it was.
    /// Measured, alone.
    #[test]
    fn a_rebase_between_prepare_and_install_keeps_the_promised_room() {
        if !nrese_exec::measure::alone() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let n = 10_000;
        let (dictionary, suffix, texts) = with_suffix(n, 2 * crate::term::BLOCK + 3);
        let before = dictionary.export_keys(0, n);
        let slot = parking_lot::Mutex::new(());
        let guard = crate::term::slot::WriterGuard::hold(slot.lock());
        let prepared = dictionary.prepare(&guard, &suffix).unwrap();
        assert!(
            dictionary
                .rebase(mapped_base(&dictionary, 6_000, dir.path()))
                .unwrap()
        );
        assert_eq!(
            dictionary.promised(),
            texts.len(),
            "the promise kept across the rebase"
        );
        let ((), held) = nrese_exec::measure::measure(|| prepared.install());
        assert_eq!(held, 0, "bytes allocated by the install after the rebase");
        assert_eq!(dictionary.export_keys(0, n), before);
        assert_added(&dictionary, n, &texts, "after the rebase");
        dictionary.seal(&guard);
        assert_added(&dictionary, n, &texts, "sealed");
    }

    /// T4. A rebase that finds no room for a promise gives up and changes nothing: the old state
    /// and its promise stay, and the install goes on.
    #[test]
    fn a_rebase_without_room_for_the_promise_changes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let n = 10_000;
        let (dictionary, suffix, texts) = with_suffix(n, 40);
        let slot = parking_lot::Mutex::new(());
        let guard = crate::term::slot::WriterGuard::hold(slot.lock());
        let prepared = dictionary.prepare(&guard, &suffix).unwrap();
        let base = mapped_base(&dictionary, 6_000, dir.path());
        Dictionary::refuse_promised_room(true);
        let refused = dictionary.rebase(base);
        Dictionary::refuse_promised_room(false);
        assert!(refused.is_err(), "the rebase gives up");
        assert_eq!(
            dictionary.stats().mapped_bytes,
            0,
            "the old state kept: nothing mapped"
        );
        assert_eq!(dictionary.promised(), 40, "the promise kept");
        prepared.install();
        assert_added(&dictionary, n, &texts, "installed after the refused rebase");
    }

    /// T5. A rebase between an install and its seal re-pushes the unsealed tail, sealing it as
    /// it goes: the seal after it has nothing left to do, and every entry is where it was.
    #[test]
    fn a_rebase_between_install_and_seal_seals_the_tail() {
        let dir = tempfile::tempdir().unwrap();
        let n = 10_000;
        let (dictionary, suffix, texts) = with_suffix(n, crate::term::BLOCK + 7);
        let slot = parking_lot::Mutex::new(());
        let guard = crate::term::slot::WriterGuard::hold(slot.lock());
        dictionary.prepare(&guard, &suffix).unwrap().install();
        assert!(dictionary.tail_len() >= crate::term::BLOCK, "unsealed");
        assert!(
            dictionary
                .rebase(mapped_base(&dictionary, 6_000, dir.path()))
                .unwrap()
        );
        assert!(
            dictionary.tail_len() < crate::term::BLOCK,
            "sealed by the rebase"
        );
        let tail = dictionary.tail_len();
        dictionary.seal(&guard);
        assert_eq!(dictionary.tail_len(), tail, "nothing left to seal");
        assert_added(&dictionary, n, &texts, "rebased");
    }

    /// Package 2a (sol-loop r1 S3, S4). An overlay over a dictionary whose base is mapped and
    /// compressed: its triple intern probes the base's compressed keys, and a view's charged
    /// lookup of a triple term and of a literal too; what they allocate is charged first, at
    /// the bound of either kind of base. The overlay's own block is the one allocation held
    /// uncharged (design note §11). Measured, alone.
    #[test]
    fn overlay_probes_of_a_compressed_base_hold_only_what_they_charged() {
        if !nrese_exec::measure::alone() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let dictionary = Arc::new(Dictionary::default());
        let s = NamedNode::new_unchecked("http://e/s");
        let p = NamedNode::new_unchecked("http://e/p");
        dictionary.intern(s.as_ref().into());
        dictionary.intern(p.as_ref().into());
        for i in 0..10_000 {
            dictionary.intern(LiteralRef::new_simple_literal(&format!("word {i}")).into());
        }
        let base = mapped_base(&dictionary, dictionary.len(), dir.path());
        assert!(dictionary.rebase(base).unwrap());
        assert!(dictionary.stats().mapped_bytes > 0, "the base is mapped");
        let budget = Arc::new(Budget::unlimited());
        let triple = nrese_rdf::Triple::new(s, p, Literal::new_simple_literal("word 5"));
        let start = dictionary.len();
        let overlay_block = 2 * size_of::<usize>() + size_of::<crate::term::overlay::Overlay>();
        let ((), excess) = nrese_exec::measure::measure_within(&budget, || {
            let overlay = crate::term::overlay::Overlay::over(
                Arc::clone(&dictionary),
                start,
                Arc::clone(&budget),
            );
            let id = overlay.intern(nrese_rdf::TermRef::Triple(&triple)).unwrap();
            assert!(
                id.payload() >= start,
                "a triple term the base lacks: the overlay's own"
            );
            let view = overlay.freeze().unwrap();
            let mut held = Reservation::new(&*budget);
            for term in [
                nrese_rdf::TermRef::Triple(&triple),
                LiteralRef::new_simple_literal("word 7").into(),
            ] {
                let found = view
                    .lookup_charged(term, |bytes| held.charge(bytes))
                    .unwrap();
                assert!(found.is_some());
            }
            drop(held);
            drop(view);
            drop(overlay);
        });
        assert_eq!(
            excess, overlay_block,
            "bytes held beyond the charges: the overlay's own block alone"
        );
        assert_eq!(budget.used(), 0);
    }

    /// Package C3: a charged decode calls its charge with no lock of the dictionary held, so
    /// a checkpoint's rebase can come between its bound and its decode. Here one does, in its
    /// first charge: a literal of 1,000 bytes interned (plain, on the heap), bounded at its
    /// key, then rebased onto a compressed base whose bound for it is larger. The decode
    /// charges the difference as a second increment, with no lock held either, and gives the
    /// literal: the two sum to the bound of the state it decodes from, which a compressed
    /// decode holds no more than (`tests/read_bounds_memory.rs`, `decode_charged_kinds.rs`).
    /// A charged lookup's charge, too, runs with no lock held.
    #[test]
    fn a_rebase_between_a_charged_decodes_bound_and_its_decode_is_charged_too() {
        let dir = tempfile::tempdir().unwrap();
        let dictionary = Dictionary::default();
        for i in 0..10_000 {
            dictionary.intern(LiteralRef::new_simple_literal(&format!("word {i}")).into());
        }
        let literal = Literal::new_simple_literal("word ".repeat(200));
        let id = dictionary.intern(literal.as_ref().into());
        let plain = dictionary.decode_bound(id).unwrap();
        let mut base = Some(mapped_base(&dictionary, dictionary.len(), dir.path()));
        let mut charges = Vec::new();
        let decoded = dictionary.decode_charged(id, |bytes| {
            assert!(dictionary.unlocked(), "charged under the dictionary's lock");
            charges.push(bytes);
            if let Some(base) = base.take() {
                assert!(dictionary.rebase(base).unwrap());
            }
            Ok::<(), ()>(())
        });
        assert!(base.is_none(), "the rebase ran in the first charge");
        assert_eq!(decoded, Ok(Some(literal.clone().into())));
        let compressed = dictionary.decode_bound(id).unwrap();
        assert!(compressed > plain, "{compressed} compressed, {plain} plain");
        assert_eq!(charges, [plain, compressed - plain]);
        let found = dictionary.lookup_charged(literal.as_ref().into(), u64::MAX, |_| {
            assert!(dictionary.unlocked(), "charged under the dictionary's lock");
            Ok::<(), ()>(())
        });
        assert_eq!(found, Ok(Some(id)));
    }

    /// Samples FSST trains on at its largest: the most keys a sample has, each empty (FSST
    /// copies the list of them), of either table; long keys of both (FSST samples them, one
    /// table trained while the other lives); every byte value, in no order (the most symbol
    /// pairs it weighs).
    fn largest_trainings() -> [Vec<Vec<u8>>; 4] {
        let empty = |tag: &[u8]| vec![tag.to_vec(); 2 * SAMPLE - 1];
        let long = (0..2_000)
            .map(|i| format!("{} item {i} {}", ["I", "S"][i % 2], "of words ".repeat(50)))
            .map(String::into_bytes)
            .collect();
        let mut x = 0x9E37_79B9_7F4A_7C15u64;
        let noise = (0..2_000)
            .map(|i| {
                let bytes = (0..500).map(|_| {
                    x ^= x << 13;
                    x ^= x >> 7;
                    x ^= x << 17;
                    x as u8
                });
                [b"IS"[i % 2]].into_iter().chain(bytes).collect()
            })
            .collect();
        [empty(b"S"), empty(b"I"), long, noise]
    }

    /// A checkpoint's dictionary section takes nothing it didn't charge to its admission
    /// first: the codec's sample and training, the keys of a compressed base decoded (Sol's
    /// review: 176,020 bytes held beyond the charges), the hash table, homes, lengths and
    /// offsets, the keys compressed, and the writer's buffer, which never grows (a key past
    /// it is written after it). All of it grows with the whole vocabulary, however small the
    /// change since the last checkpoint, and was charged nowhere (EEV-25). Rows after the
    /// section's: the codec's training at its largest, within the constants it charges for
    /// FSST's training and tables (fsst pinned; a new version fails here first).
    #[test]
    fn the_dictionary_section_holds_what_it_charged() {
        if !nrese_exec::measure::alone() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let dictionary = dictionary(dir.path());
        let budget = Budget::unlimited();
        let mut out = Checksummed {
            inner: std::io::sink(),
            crc: crc32fast::Hasher::new(),
            buffer: Vec::with_capacity(2 * super::BUFFER),
            written: 0,
            _held: Reservation::new(&budget),
        };
        let ((), section) = nrese_exec::measure::measure_within(&budget, || {
            let sample = dictionary.sample_within(dictionary.len(), &budget).unwrap();
            let trained = Codec::train_within(&sample, &budget).unwrap();
            drop(sample);
            let (len, codec) = (dictionary.len(), Some(trained.codec()));
            super::write_dictionary(&mut out, &dictionary, len, codec, &budget).unwrap();
        });
        let mut excess = vec![section];
        for keys in largest_trainings() {
            let (_, training) = nrese_exec::measure::measure_within(&budget, || {
                let mut sample = Sample::new(&budget);
                for key in &keys {
                    sample.push(key).unwrap();
                }
                Codec::train_within(&sample, &budget).unwrap()
            });
            excess.push(training);
        }
        assert_eq!(excess, [0; 5], "bytes held beyond the charges");
        assert_eq!(budget.used(), 0);
    }

    /// A dictionary section refused part way, by a root another holder filled (here on the
    /// writer's first write: the keys' first flush, before the long key's compression needs
    /// room), frees every buffer before it releases its charge, on the error
    /// path as on success: the offsets' reservation was released before their bytes, 60,100
    /// bytes held uncharged (Sol's review r2, `review_dictionary_error_frees_before_releasing`).
    #[test]
    fn a_refused_section_frees_before_it_releases() {
        if !nrese_exec::measure::alone() {
            return;
        }
        const ROOM: usize = 8 << 20;
        /// A writer whose first write fills the root but 64 bytes, for another holder.
        struct Filling<'a> {
            other: Reservation<&'a Budget>,
            root: &'a SharedBudget,
        }
        impl std::io::Write for Filling<'_> {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                if self.other.bytes() == 0 {
                    self.other.charge(ROOM - self.root.used() - 64).unwrap();
                }
                Ok(bytes.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let dictionary = Dictionary::default();
        // Keys enough for the writer's buffer to flush before the long key (over 1 MiB stored).
        for i in 0..120_000 {
            let key = format!("word {i} {}", hex(i, 24));
            dictionary.intern(LiteralRef::new_simple_literal(&key).into());
        }
        dictionary.intern(LiteralRef::new_simple_literal(&"long ".repeat(20_000)).into());
        let codec = Codec::train(&dictionary.export_keys(0, dictionary.len()));
        let root = SharedBudget::new(ROOM);
        let budget = Budget::unlimited().within(Some(Arc::clone(&root)));
        let other = Budget::unlimited().within(Some(Arc::clone(&root)));
        let buffer = Vec::with_capacity(2 * super::BUFFER);
        let ((), excess) = nrese_exec::measure::measure_within(&budget, || {
            let mut out = Checksummed {
                inner: Filling {
                    other: Reservation::new(&other),
                    root: &root,
                },
                crc: crc32fast::Hasher::new(),
                buffer,
                written: 0,
                _held: Reservation::new(&budget),
            };
            let len = dictionary.len();
            let refused =
                super::write_dictionary(&mut out, &dictionary, len, Some(&codec), &budget);
            assert!(matches!(refused, Err(crate::EngineError::MemoryLimit(_))));
        });
        assert_eq!((excess, budget.used(), root.used()), (0, 0, 0));
    }

    /// A dictionary of `texts` simple literals and `blanks` blank nodes (no text: not in
    /// the order), on the heap alone: its text order is sorted whole.
    fn heap_dictionary(texts: u64, blanks: u64) -> Dictionary {
        let dictionary = Dictionary::default();
        for i in 0..texts {
            let text = format!("{} {i}", hex(i, 12));
            dictionary.intern(LiteralRef::new_simple_literal(&text).into());
        }
        for i in 0..blanks {
            dictionary.intern(BlankNodeRef::new_unchecked(&format!("b{i}")).into());
        }
        dictionary
    }

    /// A writer into nothing, its buffer made before a measurement.
    fn sink(budget: &Budget) -> Checksummed<'_, std::io::Sink> {
        Checksummed {
            inner: std::io::sink(),
            crc: crc32fast::Hasher::new(),
            buffer: Vec::with_capacity(2 * super::BUFFER),
            written: 0,
            _held: Reservation::new(budget),
        }
    }

    /// A checkpoint's text order takes nothing it didn't charge to its admission first
    /// (SYN-49: 16 bytes an entry sorted, and the merged order, charged nowhere: 640 MB at
    /// 40 M terms). Three shapes: a compressed base with its order and entries since (those
    /// sorted, then merged with the base's, its keys decoded); the same once a prefix
    /// search has sorted them (only merged); a heap with no base, blank nodes among it
    /// (all sorted).
    #[test]
    fn a_checkpoints_text_order_holds_what_it_charged() {
        if !nrese_exec::measure::alone() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let (sorted, kept) = (dir.path().join("sorted"), dir.path().join("kept"));
        std::fs::create_dir_all(&sorted).unwrap();
        std::fs::create_dir_all(&kept).unwrap();
        let sorted = dictionary(&sorted);
        let kept = dictionary(&kept);
        let prefix = crate::term::StringTest {
            needle: "new",
            placement: crate::term::Placement::Start,
            iris: false,
            strings: true,
            lang_strings: true,
            typed: false,
            language: None,
            ascii_case_insensitive: false,
        };
        assert_eq!(kept.matching_strings(&prefix, u64::MAX).len(), 1_000);
        let heap = heap_dictionary(20_000, 100);
        let budget = Budget::unlimited();
        let mut excess = Vec::new();
        for dictionary in [&sorted, &kept, &heap] {
            let mut out = sink(&budget);
            let len = dictionary.len();
            let ((), held) = nrese_exec::measure::measure_within(&budget, || {
                super::write_text_order(&mut out, dictionary, len, &budget).unwrap();
            });
            excess.push(held);
        }
        assert_eq!(excess, [0; 3], "bytes held beyond the charges");
        assert_eq!(budget.used(), 0);
    }

    /// A text order refused part way frees what it made before it releases its charge,
    /// and holds nothing after: refused its entries (nothing made), refused the order
    /// once the entries are sorted (the entries made), refused the merged order of a kept
    /// one. Room in a root of exactly so much: 20,100 entries of 16 bytes, 20,000 of them
    /// with a text, 4 bytes each in the order and in the merged order.
    #[test]
    fn a_refused_text_order_frees_before_it_releases() {
        if !nrese_exec::measure::alone() {
            return;
        }
        let heap = heap_dictionary(20_000, 100);
        let (n, m) = (20_100, 20_000);
        // The view of the keys the order is sorted from, charged first: its heap's sealed
        // blocks' `Arc`s (8 bytes each, a block a 4,096 keys) and its tail keys' (16 each).
        let view = 8 * (n / 4_096) + 16 * (n % 4_096);
        let mut outcomes = Vec::new();
        for room in [view + 16 * n - 1, view + 16 * n + 4 * m - 1] {
            let root = SharedBudget::new(room);
            let budget = Budget::unlimited().within(Some(Arc::clone(&root)));
            let mut out = sink(&budget);
            let (refused, excess) = nrese_exec::measure::measure_within(&budget, || {
                super::write_text_order(&mut out, &heap, n as u64, &budget)
            });
            let refused = matches!(refused, Err(crate::EngineError::MemoryLimit(_)));
            drop(out);
            outcomes.push((refused, excess, root.used()));
        }
        // Kept: the merged order alone, 4 bytes an entry.
        let dir = tempfile::tempdir().unwrap();
        let kept = dictionary(dir.path());
        let prefix = crate::term::StringTest {
            needle: "new",
            placement: crate::term::Placement::Start,
            iris: false,
            strings: true,
            lang_strings: true,
            typed: false,
            language: None,
            ascii_case_insensitive: false,
        };
        assert_eq!(kept.matching_strings(&prefix, u64::MAX).len(), 1_000);
        // The view first: the 1,000 entries interned since the base, in the heap's tail.
        let root = SharedBudget::new(16 * 1_000 + 4 * kept.len() as usize - 1);
        let budget = Budget::unlimited().within(Some(Arc::clone(&root)));
        let mut out = sink(&budget);
        let len = kept.len();
        let (refused, excess) = nrese_exec::measure::measure_within(&budget, || {
            super::write_text_order(&mut out, &kept, len, &budget)
        });
        let refused = matches!(refused, Err(crate::EngineError::MemoryLimit(_)));
        drop(out);
        outcomes.push((refused, excess, root.used()));
        assert_eq!(outcomes, [(true, 0, 0); 3]);
    }

    /// A checkpoint its admission root has no room for fails with a memory error, once its
    /// file is begun: it publishes nothing and holds nothing. The next start removes the
    /// file and recovers the revision from the log, and a checkpoint with room is written
    /// (Sol's review: the refusal was only seen in a helper).
    #[test]
    fn a_checkpoint_its_admission_refuses_publishes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let config = |admission| crate::EngineConfig {
            background_maintenance: false,
            admission,
            ..Default::default()
        };
        // Room for the writer's buffers, none for the dictionary's scratch.
        let root = SharedBudget::new(super::WRITER + 2 * super::BUFFER);
        let engine = crate::Engine::open(dir.path(), config(Some(Arc::clone(&root)))).unwrap();
        let mut transaction = engine.transaction();
        transaction.insert(
            Quad::new(
                NamedNode::new_unchecked("urn:s"),
                NamedNode::new_unchecked("urn:p"),
                Literal::new_simple_literal("value"),
                GraphName::DefaultGraph,
            )
            .as_ref(),
        );
        transaction.commit().unwrap();
        let refused = engine.checkpoint();
        assert!(matches!(refused, Err(crate::EngineError::MemoryLimit(_))));
        assert_eq!((engine.snapshot().len(), root.used()), (1, 0));
        assert!(!super::exists(dir.path(), 1));
        assert!(super::checkpoint_path(dir.path(), 1, "tmp").exists());
        drop(engine);
        for _ in 0..2 {
            let engine = crate::Engine::open(dir.path(), config(None)).unwrap();
            assert!(!super::checkpoint_path(dir.path(), 1, "tmp").exists());
            let snapshot = engine.snapshot();
            assert_eq!((snapshot.revision(), snapshot.len()), (1, 1));
            assert_eq!(engine.checkpoint().unwrap(), 1);
        }
    }

    /// A format 11 checkpoint (no commit time) still opens as it did, plainly and for a
    /// restore by revision; a restore by time from it is refused, since nothing says when its
    /// commits were made (EEV-06). Format 12 is format 11 with the time before the checksum,
    /// so removing it gives the format 11 file of the same store.
    #[test]
    fn a_format_11_checkpoint_opens_but_not_for_a_time_restore() {
        use crate::{DurabilityConfig, Engine, EngineConfig, EngineError, QuadPattern};
        let dir = tempfile::tempdir().unwrap();
        let config = |until: Option<u64>, revision: Option<u64>| EngineConfig {
            background_maintenance: false,
            durability: DurabilityConfig {
                recover_until_micros: until,
                recover_until: revision,
                ..DurabilityConfig::default()
            },
            ..EngineConfig::default()
        };
        let node = |n: &str| NamedNode::new_unchecked(format!("http://example.com/{n}"));
        let quad = Quad::new(node("s"), node("p"), node("o"), GraphName::DefaultGraph);
        {
            let engine = Engine::open(dir.path(), config(None, None)).unwrap();
            super::super::fault::CLOCK.set(Some(1000));
            let mut tx = engine.transaction();
            tx.insert(quad.as_ref());
            tx.commit().unwrap();
            super::super::fault::CLOCK.set(None);
            assert_eq!(engine.checkpoint().unwrap(), 1);
        }
        let path = super::checkpoint_path(dir.path(), 1, super::EXTENSION);
        let file = std::fs::read(&path).unwrap();
        assert_eq!(&file[..8], super::MAGIC);
        let time = file.len() - 12;
        assert_eq!(
            u64::from_le_bytes(file[time..time + 8].try_into().unwrap()),
            1000
        );
        let mut old = super::MAGIC_V11.to_vec();
        old.extend_from_slice(&file[8..time]);
        let crc = crc32fast::hash(&old);
        old.extend_from_slice(&crc.to_le_bytes());
        std::fs::write(&path, &old).unwrap();

        let refused = Engine::open(dir.path(), config(Some(u64::MAX), None)).err();
        assert!(
            matches!(refused, Some(EngineError::Configuration(_))),
            "{refused:?}"
        );
        for (case, open) in [
            ("plain", config(None, None)),
            ("by revision", config(None, Some(1))),
        ] {
            let engine = Engine::open(dir.path(), open).unwrap();
            let snapshot = engine.snapshot();
            assert_eq!(snapshot.revision(), 1, "{case}");
            let held: Vec<Quad> = snapshot
                .quads_for_pattern(&QuadPattern::all())
                .map(|q| snapshot.decode_quad(q).unwrap())
                .collect();
            assert_eq!(held, std::slice::from_ref(&quad), "{case}");
        }
        // With verification too (its checksum and structure), as format 11 was read.
        let verified = EngineConfig {
            durability: DurabilityConfig {
                verify_on_open: true,
                ..config(None, None).durability
            },
            ..config(None, None)
        };
        assert_eq!(
            Engine::open(dir.path(), verified)
                .unwrap()
                .snapshot()
                .revision(),
            1
        );
    }
}
