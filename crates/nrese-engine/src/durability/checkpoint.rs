//! Checkpoints: a full image of one revision (dictionary plus both index stacks).
//!
//! Format 11 (little-endian): `magic | revision u64 | flags u64 | dictionary | stack* | crc32
//! u32`. The CRC covers everything before it. Flag bit 1 ([`COMPRESSED_KEYS`], format 11):
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

use super::codec::{Reader, put_u32, put_u64};
use crate::engine::{Snapshot, Stack};
use crate::error::{EngineError, EngineResult};
use crate::index::keys::PackedKeys;
use crate::index::{IndexVersion, Layout};
use crate::mapped::{Map, Mapped};
use crate::quad::{EncodedQuad, EncodedTriple, Permutation};
use crate::term::Dictionary;
use crate::term::dictionary::{Base, base_slots};
use crate::term::offsets::{BlockEnds, Ends, Offsets};
use crate::term::vocabulary::{Codec, VocabularyEncoding, vocabulary_encoding};
use crate::term::hash::key_hash;

/// Version 2 added the inferred stack (roadmap E6); version 3 changed term encoding (E1);
/// version 4 made integer ids order-preserving and split literal kinds (XC1); version 5
/// stores the packed permutations (Pf2); version 6 aligns them for mapping; version 7 adds
/// the dictionary's text order; version 8 the flags (integer-derived literals inline);
/// version 9 lets the asserted stack have the default-graph layout; version 10 lets blocks
/// of the packed permutations store a position as a palette; version 11 stores the
/// dictionary's offsets in blocks.
const MAGIC: &[u8; 8] = b"NRESECKB";
/// Format 10: 8-byte dictionary end offsets.
const MAGIC_V10: &[u8; 8] = b"NRESECKA";
/// Formats 9 and 8 read as 10: their blocks have no palettes (the byte was zero padding).
const MAGIC_V9: &[u8; 8] = b"NRESECK9";
const MAGIC_V8: &[u8; 8] = b"NRESECK8";
/// Earlier formats, still read (their integer-derived literals are in the dictionary):
/// without the flags, without the text order, unaligned packed permutations, quad lists.
const MAGIC_V7: &[u8; 8] = b"NRESECK7";
const MAGIC_V6: &[u8; 8] = b"NRESECK6";
/// Flags bit: integer-derived literals are dictionary entries.
const INTEGERS_IN_DICTIONARY: u64 = 1;
/// Flags bit: the dictionary's keys are compressed (format 11).
const COMPRESSED_KEYS: u64 = 2;
const MAGIC_V5: &[u8; 8] = b"NRESECK5";
const MAGIC_V4: &[u8; 8] = b"NRESECK4";
/// Magic prefix shared by every checkpoint format version.
const FAMILY: &[u8; 7] = b"NRESECK";
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
    /// Bytes handed to `inner` so far.
    written: u64,
}

impl<W: Write> Checksummed<W> {
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
        if self.buffer.len() >= 1 << 16 {
            self.flush_buffer()?;
        }
        Ok(())
    }
}

/// Writes the dictionary section (module docs) of the entries `0..len`, reading the
/// dictionary in chunks so interning is never blocked for long.
fn write_dictionary<W: Write>(
    out: &mut Checksummed<W>,
    dictionary: &Dictionary,
    len: u64,
    codec: Option<&Codec>,
) -> EngineResult<()> {
    if len >= u64::from(u32::MAX) {
        return Err(EngineError::Configuration(
            "a checkpoint holds at most 2^32 - 1 dictionary entries".to_owned(),
        ));
    }
    let chunks = || {
        (0..len.div_ceil(KEY_CHUNK)).map(move |c| (c * KEY_CHUNK, ((c + 1) * KEY_CHUNK).min(len)))
    };
    // The key lengths and the hash table, in one pass.
    let slot_count = base_slots(len) as usize;
    let mask = slot_count - 1;
    let mut slots = vec![0u32; slot_count];
    let mut lengths: Vec<u32> = Vec::with_capacity(len as usize);
    // A key as stored: plain, or compressed into `stored`.
    let mut stored = Vec::new();
    for (from, to) in chunks() {
        dictionary.for_each_key(from, to, |key| {
            let mut slot = key_hash(key) as usize & mask;
            while slots[slot] != 0 {
                slot = (slot + 1) & mask;
            }
            let length = match codec {
                None => key.len(),
                Some(codec) => {
                    stored.clear();
                    codec.compress(key, &mut stored);
                    stored.len()
                }
            };
            lengths.push(length as u32);
            slots[slot] = lengths.len() as u32;
        });
    }
    if lengths.len() as u64 != len {
        return Err(EngineError::Corruption(
            "the dictionary is shorter than the snapshot".to_owned(),
        ));
    }
    let arena_len: u64 = lengths.iter().map(|&n| u64::from(n)).sum();
    let (blocks, ends) = crate::term::offsets::encode(&lengths).ok_or_else(|| {
        EngineError::Configuration(
            "64 consecutive dictionary keys take 4 GiB or more; a checkpoint can't hold them"
                .to_owned(),
        )
    })?;
    drop(lengths);
    put_u64(&mut out.buffer, len);
    put_u64(&mut out.buffer, arena_len);
    put_u64(&mut out.buffer, slot_count as u64);
    put_u64(&mut out.buffer, ends.len() as u64);
    if let Some(codec) = codec {
        codec.write(&mut out.buffer);
    }
    out.pad();
    for block in blocks {
        put_u64(&mut out.buffer, block);
        out.drain()?;
    }
    for (from, to) in chunks() {
        dictionary.for_each_key(from, to, |key| match codec {
            None => out.buffer.extend_from_slice(key),
            Some(codec) => codec.compress(key, &mut out.buffer),
        });
        out.drain()?;
    }
    out.write_bytes(&ends)?;
    drop(ends);
    out.pad();
    for slot in slots {
        put_u32(&mut out.buffer, slot);
        out.drain()?;
    }
    let order = dictionary.text_order(len);
    out.pad();
    put_u64(&mut out.buffer, order.len() as u64);
    for index in order {
        put_u32(&mut out.buffer, index);
        out.drain()?;
    }
    Ok(())
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
            blocks: Mapped::new(map, blocks_at, 2 * block_count as usize)
                .ok_or_else(unmappable)?,
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
            let needed = (at >> 1) as usize
                + width * ((len as usize - 1) % crate::term::offsets::BLOCK + 1);
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
pub(crate) fn exists(dir: &Path, revision: u64) -> bool {
    checkpoint_path(dir, revision, EXTENSION).exists()
}

/// Writes a checkpoint of `snapshot` and returns its path.
pub(crate) fn write(dir: &Path, snapshot: &Snapshot) -> EngineResult<PathBuf> {
    let version = snapshot.version();
    write_parts(
        dir,
        snapshot.revision(),
        snapshot.dictionary(),
        snapshot.dictionary_len(),
        [version.asserted.layout(), version.inferred.layout()],
        &mut |stack, permutation| Ok(snapshot.version().stack(stack).packed(permutation)),
    )
}

/// Writes a checkpoint of `revision` from its parts and returns its path: the dictionary's
/// entries `0..dictionary_len`, and each stack's permutations as `packed` gives them, in
/// the order of the stack's layout (`layouts`: asserted, inferred). Each is written before the next is asked for, so a
/// caller can build one at a time and drop it (bulk loads).
pub(crate) fn write_parts<'a>(
    dir: &Path,
    revision: u64,
    dictionary: &Dictionary,
    dictionary_len: u64,
    layouts: [Layout; 2],
    packed: &mut dyn FnMut(Stack, Permutation) -> EngineResult<Cow<'a, PackedKeys>>,
) -> EngineResult<PathBuf> {
    let tmp = checkpoint_path(dir, revision, "tmp");
    let mut file = File::create(&tmp)?;
    // The magic is written last, once everything else is on file; it is checksummed apart
    // and the checksums combined.
    file.write_all(MAGIC)?;
    let mut out = Checksummed {
        inner: BufWriter::with_capacity(1 << 20, file),
        crc: crc32fast::Hasher::new(),
        buffer: Vec::with_capacity(1 << 16),
        written: MAGIC.len() as u64,
    };
    put_u64(&mut out.buffer, revision);
    // Keys compressed with tables trained on a sample of them ([`crate::term::vocabulary`]).
    let codec = (vocabulary_encoding() == VocabularyEncoding::Fsst)
        .then(|| Codec::train(&dictionary.sample_keys(dictionary_len)));
    let flags = match dictionary.integers_in_dictionary() {
        true => INTEGERS_IN_DICTIONARY,
        false => 0,
    } | match codec {
        Some(_) => COMPRESSED_KEYS,
        None => 0,
    };
    put_u64(&mut out.buffer, flags);
    write_dictionary(&mut out, dictionary, dictionary_len, codec.as_ref())?;
    for (stack, layout) in Stack::ALL.into_iter().zip(layouts) {
        let permutations = layout.permutations();
        put_u32(&mut out.buffer, permutations.len() as u32);
        for &permutation in permutations {
            out.buffer.push(permutation as u8);
            let keys = packed(stack, permutation)?;
            let at = out.position();
            keys.write(Some(at), &mut |piece| out.write_bytes(piece))?;
        }
    }
    out.flush_buffer()?;
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
    file.sync_all()?;
    drop(file);
    let path = checkpoint_path(dir, revision, EXTENSION);
    fs::rename(&tmp, &path)?;
    super::sync_dir(dir)?;
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
    let timing = std::env::var_os("NRESE_RECOVERY_TIMING").is_some();
    let clock = std::time::Instant::now();
    let lap = |what: &str| {
        if timing {
            eprintln!(
                "  checkpoint {what}: {:.3} s",
                clock.elapsed().as_secs_f64()
            );
        }
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
    let (packed, aligned, with_order, with_flags) = match reader.bytes(MAGIC.len()) {
        Some(magic) if magic == MAGIC => {
            blocks = true;
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
    let map = crate::mapped::map(path)?;
    let bytes: &[u8] = &map;
    let body = &bytes[..bytes.len().saturating_sub(4)];
    let mut reader = Reader::new(body);
    // Format 8 is format 9 with the quad layout only; 9 is 10 without palettes.
    let blocks = match reader.bytes(MAGIC.len()) {
        Some(magic) if magic == MAGIC => true,
        Some(magic) if magic == MAGIC_V10 || magic == MAGIC_V9 || magic == MAGIC_V8 => false,
        _ => return Err(corrupt("bad magic")),
    };
    reader.u64().ok_or_else(|| corrupt("truncated header"))?;
    // The flags: this engine's, but whether the keys are compressed.
    let flags = reader.u64().ok_or_else(|| corrupt("truncated header"))?;
    let base = read_dictionary(&mut reader, &map, true, blocks, flags & COMPRESSED_KEYS != 0)
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
