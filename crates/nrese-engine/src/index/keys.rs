//! Compressed sorted key arrays (Pf1): the storage of one permutation of a run.
//!
//! Keys are cut into blocks of [`BLOCK`]. Within a block each key position is stored in
//! the cheaper of two encodings:
//!
//! - frame of reference: two bit-packed sub-columns relative to the block's minimum, the
//!   4-bit term kind tag and the 60-bit payload (see [`TermId`](crate::TermId)). A position
//!   that is constant in the block (the graph, usually the first component) takes no bits;
//!   a position mixing kinds (IRIs and literals as objects) costs a few tag bits plus the
//!   payload range instead of 64 bits;
//! - a palette (format 10): the block's distinct values at that position, word-aligned at
//!   the start of the block's data, and per key the bit-packed index of its value. It wins
//!   where few values spread far apart: a subject's objects, identifiers in insertion
//!   order (DBpedia's 1.9, Wikidata's 3.1 bytes per quad).
//!
//! Every key stays randomly accessible in O(1) (a palette costs one more word read), so
//! binary searches run on the compressed data: first over the blocks' first keys, then
//! inside one block.
//!
//! Palettes are a trade-off ([`IndexEncoding`]): on the office PC, DBpedia core's store is
//! 3.3 % smaller (the index 8 %) and its queries 7 % slower (a dependent load per value in
//! scans); Wikidata lexemes' store 8.6 % smaller, queries even. Off by default.
//!
//! Typical cost on LUBM: 6 to 12 bytes per key instead of 32.

use std::sync::atomic::{AtomicBool, Ordering};

use rayon::prelude::*;

use crate::mapped::{Map, Mapped, Plain};
use crate::quad::Key;

/// How index blocks are encoded when they are built (`store.index_encoding`). Reading
/// takes either, so a store can switch: blocks built after the switch take the new one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum IndexEncoding {
    /// Frame of reference only: the fastest scans.
    #[default]
    Fast,
    /// Palettes where smaller: a smaller store, scans up to a tenth slower.
    Compact,
}

impl IndexEncoding {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Fast => "fast",
            Self::Compact => "compact",
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "fast" => Some(Self::Fast),
            "compact" => Some(Self::Compact),
            _ => None,
        }
    }
}

/// The encoding blocks are built with, for the process: the stores of one server share
/// their settings.
static COMPACT: AtomicBool = AtomicBool::new(false);

/// Sets the encoding of the index blocks built from now on, in every store of the process.
pub fn set_index_encoding(encoding: IndexEncoding) {
    COMPACT.store(encoding == IndexEncoding::Compact, Ordering::Relaxed);
}

fn current_encoding() -> IndexEncoding {
    match COMPACT.load(Ordering::Relaxed) {
        true => IndexEncoding::Compact,
        false => IndexEncoding::Fast,
    }
}

/// Keys per block.
pub(crate) const BLOCK: usize = 128;

const TAG_SHIFT: u32 = 60;
const PAYLOAD_MASK: u64 = (1 << TAG_SHIFT) - 1;

/// The tag width that marks a position stored as a palette (tags take at most 4 bits). In
/// a byte every access reads anyway: a flag elsewhere in the header cost random access a
/// cache line (12.6 -> 22 ns per key with `keys_bench`).
const PALETTE: u8 = 0x80;

/// Bits per key of a sub-column of width `w` (a palette marker has none).
#[inline]
fn bits_of(w: u8) -> usize {
    if w == PALETTE { 0 } else { w as usize }
}

/// The encoding of one block: per key position, the minimum tag and payload, the bit
/// widths of the packed offsets from them, and where each sub-column starts; or, where the
/// tag width is [`PALETTE`], a palette.
///
/// `repr(C)` with explicit padding: its memory image is what checkpoints store, so a
/// mapped checkpoint's headers are used in place. Formats before 10 have no palettes.
#[derive(Debug, Clone, Copy, Default)]
#[repr(C)]
struct Header {
    /// Word offset of the block's data in `data`.
    offset: u64,
    /// Per position: the minimum payload; for a palette, the word of its first value
    /// within the block's data.
    payload_min: [u64; 4],
    /// Per position: the minimum tag; for a palette, its number of values less one.
    tag_min: [u8; 4],
    /// Bit widths of the sub-columns: tag then payload, per key position (a palette: the
    /// marker [`PALETTE`], and the index width).
    widths: [u8; 8],
    /// Bit offsets of the sub-columns within the block's data, after the palettes (at most
    /// 4 positions × 128 keys × 64 bits, each at most what the frame of reference takes).
    starts: [u16; 8],
    _padding: [u8; 4],
}

/// The distinct values of position `c` in `block`, ascending.
fn distinct(block: &[Key], c: usize) -> Vec<u64> {
    let mut values: Vec<u64> = block.iter().map(|k| k[c]).collect();
    values.sort_unstable();
    values.dedup();
    values
}

impl Header {
    /// The encoding of `block` (at most [`BLOCK`] sorted keys): per position, the frame of
    /// reference, or (`Compact`) a palette where that takes fewer bits.
    fn of(block: &[Key], encoding: IndexEncoding) -> Self {
        let mut header = Header::default();
        let n = block.len();
        let mut palette_words = 0;
        for c in 0..4 {
            let (tag_min, tag_max) = block.iter().fold((u8::MAX, 0), |(lo, hi), k| {
                let t = (k[c] >> TAG_SHIFT) as u8;
                (lo.min(t), hi.max(t))
            });
            let (payload_min, payload_max) = block.iter().fold((u64::MAX, 0), |(lo, hi), k| {
                let p = k[c] & PAYLOAD_MASK;
                (lo.min(p), hi.max(p))
            });
            let (tw, pw) = (
                width(u64::from(tag_max - tag_min)),
                width(payload_max - payload_min),
            );
            let frame_bits = n * (tw as usize + pw as usize);
            // A palette only where the values vary, and only if it is smaller.
            if encoding == IndexEncoding::Compact && frame_bits > 0 {
                let values = distinct(block, c).len();
                let index = width(values as u64 - 1);
                if 64 * values + n * (index as usize) < frame_bits {
                    header.widths[2 * c] = PALETTE;
                    header.payload_min[c] = palette_words as u64;
                    header.tag_min[c] = (values - 1) as u8;
                    header.widths[2 * c + 1] = index;
                    palette_words += values;
                    continue;
                }
            }
            header.tag_min[c] = tag_min;
            header.payload_min[c] = payload_min;
            header.widths[2 * c] = tw;
            header.widths[2 * c + 1] = pw;
        }
        let mut bit = 64 * palette_words;
        for (start, &w) in header.starts.iter_mut().zip(&header.widths) {
            *start = bit as u16;
            bit += n * bits_of(w);
        }
        header
    }

    /// Whether position `c` is stored as a palette.
    #[inline]
    fn palette(&self, c: usize) -> bool {
        self.widths[2 * c] == PALETTE
    }

    /// Words of data (palettes, then packed bits) for a block of `n` keys.
    fn words(&self, n: usize) -> usize {
        (self.starts[7] as usize + n * self.widths[7] as usize).div_ceil(64)
    }

    /// Packs `block` into `bits` (zeroed, [`words`](Self::words) long).
    fn pack(&self, block: &[Key], bits: &mut [u64]) {
        for c in 0..4 {
            let (tw, pw) = (self.widths[2 * c], self.widths[2 * c + 1]);
            if self.palette(c) {
                let values = distinct(block, c);
                let first = self.payload_min[c] as usize;
                bits[first..first + values.len()].copy_from_slice(&values);
                for (j, key) in block.iter().enumerate() {
                    let index = values.binary_search(&key[c]).expect("in the palette") as u64;
                    write(
                        bits,
                        self.starts[2 * c + 1] as usize + j * pw as usize,
                        pw,
                        index,
                    );
                }
                continue;
            }
            for (j, key) in block.iter().enumerate() {
                let value = key[c];
                let tag = u64::from((value >> TAG_SHIFT) as u8 - self.tag_min[c]);
                let payload = (value & PAYLOAD_MASK) - self.payload_min[c];
                write(bits, self.starts[2 * c] as usize + j * tw as usize, tw, tag);
                write(
                    bits,
                    self.starts[2 * c + 1] as usize + j * pw as usize,
                    pw,
                    payload,
                );
            }
        }
    }

    /// Component `c` of key `j` of the block whose data starts at `data`.
    #[inline]
    fn component(&self, data: &[u64], c: usize, j: usize) -> u64 {
        let (tw, pw) = (self.widths[2 * c], self.widths[2 * c + 1]);
        if tw == PALETTE {
            let index = read(data, self.starts[2 * c + 1] as usize + j * pw as usize, pw);
            return data[self.payload_min[c] as usize + index as usize];
        }
        let tag = u64::from(self.tag_min[c])
            + read(data, self.starts[2 * c] as usize + j * tw as usize, tw);
        let payload =
            self.payload_min[c] + read(data, self.starts[2 * c + 1] as usize + j * pw as usize, pw);
        (tag << TAG_SHIFT) | payload
    }
}

/// [`PackedKeys::iter`].
pub(crate) struct Iter<'a> {
    keys: &'a PackedKeys,
    /// The first key not decoded yet.
    next: usize,
    block: Vec<Key>,
    at: usize,
}

impl Iterator for Iter<'_> {
    type Item = Key;

    fn next(&mut self) -> Option<Key> {
        if self.at == self.block.len() {
            if self.next == self.keys.len {
                return None;
            }
            let end = ((self.next / BLOCK + 1) * BLOCK).min(self.keys.len);
            self.block.clear();
            self.keys.decode_range(self.next, end, &mut self.block);
            self.next = end;
            self.at = 0;
        }
        self.at += 1;
        Some(self.block[self.at - 1])
    }
}

/// A sorted array of keys, compressed per block.
#[derive(Debug, Default, Clone)]
pub(crate) struct PackedKeys {
    len: usize,
    headers: Storage<Header>,
    /// The first key of every block, for the block-level binary search.
    firsts: Storage<Key>,
    /// Packed bits; one spare word at the end, so reads never cross the slice end.
    data: Storage<u64>,
}

/// An array on the heap, or in a memory-mapped checkpoint ([`crate::mapped`]).
#[derive(Debug, Clone)]
enum Storage<T: Plain> {
    Owned(Box<[T]>),
    Mapped(Mapped<T>),
}

impl<T: Plain> Default for Storage<T> {
    fn default() -> Self {
        Self::Owned(Box::default())
    }
}

impl<T: Plain> std::ops::Deref for Storage<T> {
    type Target = [T];

    #[inline]
    fn deref(&self) -> &[T] {
        match self {
            Self::Owned(values) => values,
            Self::Mapped(values) => values,
        }
    }
}

impl<T: Plain> Storage<T> {
    fn heap_bytes(&self) -> usize {
        match self {
            Self::Owned(values) => size_of_val::<[T]>(values),
            Self::Mapped(_) => 0,
        }
    }

    fn mapped_bytes(&self) -> usize {
        match self {
            Self::Owned(_) => 0,
            Self::Mapped(values) => size_of_val::<[T]>(values),
        }
    }
}

// SAFETY: `Header` is `repr(C)` of integers with its padding spelled out as a field.
unsafe impl Plain for Header {}
// SAFETY: a key is four `u64`s.
unsafe impl Plain for Key {}

/// Serialised size of a [`Header`] in format 5.
const HEADER_BYTES_V5: usize = 8 + 32 + 4 + 8 + 16;
/// Serialised size of a [`Header`] since format 6: its in-memory image, padding included.
const HEADER_BYTES: usize = size_of::<Header>();
const _: () = assert!(HEADER_BYTES == HEADER_BYTES_V5 + 4);

/// Arrays at least this long are packed in parallel.
const PARALLEL_PACK: usize = 1 << 16;

/// `count` values of `T` at `raw`, in place in `map` if `raw` lies in it and they can be
/// mapped.
fn in_place<T: Plain>(map: Option<&Map>, raw: &[u8], count: usize) -> Option<Mapped<T>> {
    let map = map?;
    let offset = (raw.as_ptr() as usize).checked_sub(map.as_ptr() as usize)?;
    Mapped::new(map, offset, count)
}

/// Zero bytes after `len` bytes written from file position `at`, up to the next multiple
/// of 8.
fn padding(at: u64, len: usize) -> usize {
    ((8 - (at + len as u64) % 8) % 8) as usize
}

/// Bits needed for values `0..=max`.
fn width(max: u64) -> u8 {
    (64 - max.leading_zeros()) as u8
}

/// The `width`-bit value at `bit`. Two predictable branches beat a branch-free `u128`
/// double-word shift here (measured with `keys_bench`: 13 against 32 ns per random key).
#[inline]
fn read(data: &[u64], bit: usize, width: u8) -> u64 {
    if width == 0 {
        return 0;
    }
    let (word, shift) = (bit / 64, bit % 64);
    let mut value = data[word] >> shift;
    if shift + width as usize > 64 {
        value |= data[word + 1] << (64 - shift);
    }
    value & (u64::MAX >> (64 - width))
}

fn write(data: &mut [u64], bit: usize, width: u8, value: u64) {
    if width == 0 {
        return;
    }
    let (word, shift) = (bit / 64, bit % 64);
    data[word] |= value << shift;
    if shift + width as usize > 64 {
        data[word + 1] |= value >> (64 - shift);
    }
}

impl PackedKeys {
    /// Packs `keys`, which must be sorted, in the process's encoding. Large arrays are
    /// packed in parallel.
    pub(crate) fn from_sorted(keys: &[Key]) -> Self {
        Self::from_sorted_as(keys, current_encoding())
    }

    /// Packs `keys`, which must be sorted, in `encoding`.
    pub(crate) fn from_sorted_as(keys: &[Key], encoding: IndexEncoding) -> Self {
        debug_assert!(keys.is_sorted(), "keys must be sorted");
        let parallel = keys.len() >= PARALLEL_PACK;
        let mut headers: Vec<Header> = if parallel {
            keys.par_chunks(BLOCK)
                .map(|block| Header::of(block, encoding))
                .collect()
        } else {
            keys.chunks(BLOCK)
                .map(|block| Header::of(block, encoding))
                .collect()
        };
        let mut words = 0;
        for (header, block) in headers.iter_mut().zip(keys.chunks(BLOCK)) {
            header.offset = words as u64;
            words += header.words(block.len());
        }
        // One spare word, so reads never cross the end.
        let mut data = vec![0u64; words + 1];
        let mut slices: Vec<&mut [u64]> = Vec::with_capacity(headers.len());
        let mut rest = data.as_mut_slice();
        for (header, block) in headers.iter().zip(keys.chunks(BLOCK)) {
            let (bits, tail) = rest.split_at_mut(header.words(block.len()));
            slices.push(bits);
            rest = tail;
        }
        if parallel {
            slices
                .into_par_iter()
                .zip(keys.par_chunks(BLOCK))
                .zip(headers.par_iter())
                .for_each(|((bits, block), header)| header.pack(block, bits));
        } else {
            for ((bits, block), header) in slices.into_iter().zip(keys.chunks(BLOCK)).zip(&headers)
            {
                header.pack(block, bits);
            }
        }
        Self {
            len: keys.len(),
            headers: Storage::Owned(headers.into_boxed_slice()),
            firsts: Storage::Owned(keys.chunks(BLOCK).map(|block| block[0]).collect()),
            data: Storage::Owned(data.into_boxed_slice()),
        }
    }

    /// Packs `len` sorted keys that `gather` produces a block at a time (it appends the
    /// keys at the positions of the range it is given), in parallel, without holding
    /// them all: a permutation derived through positions ([`super::derive`]). Each block
    /// is gathered twice, once for its header and once to pack it.
    pub(crate) fn from_sorted_gathered(
        len: usize,
        gather: impl Fn(std::ops::Range<usize>, &mut Vec<Key>) + Sync,
    ) -> Self {
        let encoding = current_encoding();
        let blocks = len.div_ceil(BLOCK);
        let range = |b: usize| b * BLOCK..((b + 1) * BLOCK).min(len);
        let block_of = |b: usize, keys: &mut Vec<Key>| {
            keys.clear();
            gather(range(b), keys);
            debug_assert!(keys.is_sorted(), "keys must be sorted");
        };
        let (mut headers, firsts): (Vec<Header>, Vec<Key>) = (0..blocks)
            .into_par_iter()
            .map_init(
                || Vec::with_capacity(BLOCK),
                |keys, b| {
                    block_of(b, keys);
                    (Header::of(keys, encoding), keys[0])
                },
            )
            .unzip();
        let mut words = 0;
        for (b, header) in headers.iter_mut().enumerate() {
            header.offset = words as u64;
            words += header.words(range(b).len());
        }
        let mut data = vec![0u64; words + 1];
        let mut slices: Vec<&mut [u64]> = Vec::with_capacity(blocks);
        let mut rest = data.as_mut_slice();
        for (b, header) in headers.iter().enumerate() {
            let (bits, tail) = rest.split_at_mut(header.words(range(b).len()));
            slices.push(bits);
            rest = tail;
        }
        slices
            .into_par_iter()
            .zip(headers.par_iter())
            .enumerate()
            .for_each_init(
                || Vec::with_capacity(BLOCK),
                |keys, (b, (bits, header))| {
                    block_of(b, keys);
                    header.pack(keys, bits);
                },
            );
        Self {
            len,
            headers: Storage::Owned(headers.into_boxed_slice()),
            firsts: Storage::Owned(firsts.into_boxed_slice()),
            data: Storage::Owned(data.into_boxed_slice()),
        }
    }

    /// Packs keys arriving in sorted order (a merged scan), without holding them all.
    pub(crate) fn from_sorted_iter(keys: impl Iterator<Item = Key>) -> Self {
        let mut packed = Self::default();
        let mut data: Vec<u64> = Vec::new();
        let (mut headers, mut firsts) = (Vec::new(), Vec::new());
        let mut block: Vec<Key> = Vec::with_capacity(BLOCK);
        let encoding = current_encoding();
        let mut flush = |block: &mut Vec<Key>, data: &mut Vec<u64>| {
            let mut header = Header::of(block, encoding);
            header.offset = data.len() as u64;
            let start = data.len();
            data.resize(start + header.words(block.len()), 0);
            header.pack(block, &mut data[start..]);
            headers.push(header);
            firsts.push(block[0]);
            block.clear();
        };
        for key in keys {
            debug_assert!(
                block.last().is_none_or(|last| *last < key),
                "keys must be sorted"
            );
            block.push(key);
            packed.len += 1;
            if block.len() == BLOCK {
                flush(&mut block, &mut data);
            }
        }
        if !block.is_empty() {
            flush(&mut block, &mut data);
        }
        data.push(0);
        packed.headers = Storage::Owned(headers.into_boxed_slice());
        packed.firsts = Storage::Owned(firsts.into_boxed_slice());
        packed.data = Storage::Owned(data.into_boxed_slice());
        packed
    }

    /// Serialises the array, little-endian; `emit` receives consecutive pieces (large
    /// arrays in chunks).
    ///
    /// With `at`, the position in the file where the array starts (format 6): `len u64 |
    /// blocks u64 | words u64 | padding | headers | firsts | data`, the headers as their
    /// 72-byte in-memory image, zero padding to an 8-byte boundary of the file, so all three
    /// can be used in place from a memory map. Without (format 5): no padding, 68-byte
    /// headers.
    pub(crate) fn write(
        &self,
        at: Option<u64>,
        emit: &mut dyn FnMut(&[u8]) -> std::io::Result<()>,
    ) -> std::io::Result<()> {
        let mut buffer: Vec<u8> = Vec::with_capacity(1 << 16);
        for value in [
            self.len as u64,
            self.headers.len() as u64,
            self.data.len() as u64,
        ] {
            buffer.extend_from_slice(&value.to_le_bytes());
        }
        if let Some(at) = at {
            buffer.resize(buffer.len() + padding(at, 24), 0);
        }
        let mut drain = |buffer: &mut Vec<u8>, force: bool| -> std::io::Result<()> {
            if force || buffer.len() >= 1 << 16 {
                emit(buffer)?;
                buffer.clear();
            }
            Ok(())
        };
        for header in self.headers.iter() {
            buffer.extend_from_slice(&header.offset.to_le_bytes());
            for min in header.payload_min {
                buffer.extend_from_slice(&min.to_le_bytes());
            }
            buffer.extend_from_slice(&header.tag_min);
            buffer.extend_from_slice(&header.widths);
            for start in header.starts {
                buffer.extend_from_slice(&start.to_le_bytes());
            }
            if at.is_some() {
                buffer.extend_from_slice(&[0; 4]);
            }
            drain(&mut buffer, false)?;
        }
        for key in self.firsts.iter() {
            for component in key {
                buffer.extend_from_slice(&component.to_le_bytes());
            }
            drain(&mut buffer, false)?;
        }
        for word in self.data.iter() {
            buffer.extend_from_slice(&word.to_le_bytes());
            drain(&mut buffer, false)?;
        }
        drain(&mut buffer, true)
    }

    /// Reads what [`write`](Self::write) wrote from `bytes`, returning the array and the
    /// bytes consumed.
    ///
    /// `file_at` is the file position of `bytes` for format 6 (as `write`'s `at`); `map` the
    /// mapped file `bytes` lie in, if any: the headers, first keys and packed bits are then
    /// used in place, and only the counts are checked, so opening reads nothing else. With
    /// `verify`, or when the array is copied, every block is checked (widths, offsets, first
    /// keys), so a damaged file fails here instead of in a later lookup.
    pub(crate) fn read(
        bytes: &[u8],
        file_at: Option<u64>,
        map: Option<&Map>,
        verify: bool,
    ) -> Result<(Self, usize), String> {
        let mut at: usize = 0;
        let mut take = |n: usize| -> Result<&[u8], String> {
            let piece = bytes
                .get(at..at.checked_add(n).ok_or("truncated packed keys")?)
                .ok_or("truncated packed keys")?;
            at += n;
            Ok(piece)
        };
        let u64_at = |piece: &[u8], i: usize| {
            u64::from_le_bytes(piece[8 * i..8 * i + 8].try_into().expect("8 bytes"))
        };
        let counts = take(24)?;
        let (len, blocks, words) = (u64_at(counts, 0), u64_at(counts, 1), u64_at(counts, 2));
        let len = usize::try_from(len).map_err(|_| "key count overflows")?;
        let blocks = usize::try_from(blocks).map_err(|_| "block count overflows")?;
        let words = usize::try_from(words).map_err(|_| "word count overflows")?;
        if blocks != len.div_ceil(BLOCK) || words == 0 {
            return Err("inconsistent packed key counts".into());
        }
        if let Some(file_at) = file_at {
            take(padding(file_at, 24))?;
        }
        let header_bytes = if file_at.is_some() {
            HEADER_BYTES
        } else {
            HEADER_BYTES_V5
        };
        let raw_headers = take(
            blocks
                .checked_mul(header_bytes)
                .ok_or("header size overflows")?,
        )?;
        let raw_firsts = take(blocks.checked_mul(32).ok_or("first keys overflow")?)?;
        let raw_data = take(words.checked_mul(8).ok_or("data size overflows")?)?;
        // Format 6 in a mapped file: the arrays in place.
        let map = map.filter(|_| file_at.is_some());
        let keys = match (
            in_place(map, raw_headers, blocks),
            in_place(map, raw_firsts, blocks),
            in_place(map, raw_data, words),
        ) {
            (Some(headers), Some(firsts), Some(data)) => Self {
                len,
                headers: Storage::Mapped(headers),
                firsts: Storage::Mapped(firsts),
                data: Storage::Mapped(data),
            },
            _ => {
                let headers: Vec<Header> = raw_headers
                    .chunks_exact(header_bytes)
                    .map(|piece| {
                        let mut header = Header {
                            offset: u64_at(piece, 0),
                            ..Header::default()
                        };
                        for c in 0..4 {
                            header.payload_min[c] = u64_at(piece, 1 + c);
                        }
                        header.tag_min.copy_from_slice(&piece[40..44]);
                        header.widths.copy_from_slice(&piece[44..52]);
                        for (i, start) in header.starts.iter_mut().enumerate() {
                            *start = u16::from_le_bytes([piece[52 + 2 * i], piece[53 + 2 * i]]);
                        }
                        header
                    })
                    .collect();
                let firsts: Vec<Key> = raw_firsts
                    .as_chunks::<32>()
                    .0
                    .iter()
                    .map(|piece| std::array::from_fn(|c| u64_at(piece, c)))
                    .collect();
                let data: Vec<u64> = raw_data
                    .as_chunks::<8>()
                    .0
                    .iter()
                    .map(|b| u64::from_le_bytes(*b))
                    .collect();
                Self {
                    len,
                    headers: Storage::Owned(headers.into_boxed_slice()),
                    firsts: Storage::Owned(firsts.into_boxed_slice()),
                    data: Storage::Owned(data.into_boxed_slice()),
                }
            }
        };
        let in_place = matches!(keys.data, Storage::Mapped(_));
        if verify || !in_place {
            keys.verify()?;
        } else if let Some(last) = keys.headers.last() {
            // In place: the last block must end where the data does (one header read).
            let n = len - (blocks - 1) * BLOCK;
            if last.offset + last.words(n) as u64 + 1 != words as u64 {
                return Err("packed data length does not match its blocks".into());
            }
        }
        Ok((keys, at))
    }

    /// Checks every block's header (widths, sub-column starts, word offsets) and first key
    /// against the data: reads all of it.
    pub(crate) fn verify(&self) -> Result<(), String> {
        let mut expected_offset = 0u64;
        for (b, header) in self.headers.iter().enumerate() {
            let n = BLOCK.min(self.len - b * BLOCK);
            let valid_widths = header
                .widths
                .chunks(2)
                .all(|w| (w[0] <= 4 || w[0] == PALETTE) && w[1] <= 60);
            // Palettes: indices within them, laid out one after the other.
            let mut palette_words = 0;
            let mut valid_palettes = true;
            for c in (0..4).filter(|&c| header.palette(c)) {
                let values = header.tag_min[c] as usize + 1;
                valid_palettes &= header.payload_min[c] == palette_words as u64
                    && values <= n
                    && header.widths[2 * c + 1] <= 7
                    && (1usize << header.widths[2 * c + 1]) >= values;
                palette_words += values;
            }
            let mut bit = 64 * palette_words;
            let valid_starts = header
                .starts
                .iter()
                .zip(&header.widths)
                .all(|(&start, &w)| {
                    let ok = start as usize == bit;
                    bit += n * bits_of(w);
                    ok
                });
            if header.offset != expected_offset || !valid_widths || !valid_palettes || !valid_starts
            {
                return Err(format!("damaged header of block {b}"));
            }
            expected_offset += header.words(n) as u64;
        }
        if expected_offset + 1 != self.data.len() as u64 {
            return Err("packed data length does not match its blocks".into());
        }
        if (0..self.headers.len()).any(|b| self.get(b * BLOCK) != self.firsts[b]) {
            return Err("packed first keys do not match their blocks".into());
        }
        Ok(())
    }

    #[inline]
    pub(crate) fn len(&self) -> usize {
        self.len
    }

    /// The keys in order, a block decoded at a time.
    pub(crate) fn iter(&self) -> Iter<'_> {
        Iter {
            keys: self,
            next: 0,
            block: Vec::with_capacity(BLOCK),
            at: 0,
        }
    }

    /// The key at `index`. O(1).
    #[inline]
    pub(crate) fn get(&self, index: usize) -> Key {
        debug_assert!(index < self.len, "key index out of range");
        let (block, j) = (index / BLOCK, index % BLOCK);
        let header = &self.headers[block];
        let data = &self.data[header.offset as usize..];
        [
            header.component(data, 0, j),
            header.component(data, 1, j),
            header.component(data, 2, j),
            header.component(data, 3, j),
        ]
    }

    /// Appends the keys `start..end`, which must lie in one block, to `out`: a column at a
    /// time, cheaper per key than [`get`](Self::get).
    pub(crate) fn decode_range(&self, start: usize, end: usize, out: &mut Vec<Key>) {
        debug_assert!(start < end && (end - 1) / BLOCK == start / BLOCK);
        let block = start / BLOCK;
        let header = &self.headers[block];
        let data = &self.data[header.offset as usize..];
        let first = out.len();
        out.resize(first + end - start, [0; 4]);
        let keys = &mut out[first..];
        let j0 = start - block * BLOCK;
        for c in 0..4 {
            let (tw, pw) = (header.widths[2 * c], header.widths[2 * c + 1]);
            if header.palette(c) {
                let palette = &data[header.payload_min[c] as usize..];
                let mut bit = header.starts[2 * c + 1] as usize + j0 * pw as usize;
                for key in keys.iter_mut() {
                    key[c] = palette[read(data, bit, pw) as usize];
                    bit += pw as usize;
                }
                continue;
            }
            if tw == 0 && pw == 0 {
                let constant = (u64::from(header.tag_min[c]) << TAG_SHIFT) | header.payload_min[c];
                keys.iter_mut().for_each(|key| key[c] = constant);
                continue;
            }
            // Tags first (usually constant), then payloads: sequential unpacking with a
            // running bit position, the payload loop a shift and mask per key.
            let tag_min = u64::from(header.tag_min[c]);
            let mut bit = header.starts[2 * c] as usize + j0 * tw as usize;
            for key in keys.iter_mut() {
                key[c] = (tag_min + read(data, bit, tw)) << TAG_SHIFT;
                bit += tw as usize;
            }
            let payload_min = header.payload_min[c];
            let mut bit = header.starts[2 * c + 1] as usize + j0 * pw as usize;
            for key in keys.iter_mut() {
                key[c] |= payload_min + read(data, bit, pw);
                bit += pw as usize;
            }
        }
    }

    /// Calls `f(value, from, to)` for each run `from..to` of keys with the same value at
    /// `position` among the keys `start..end`, in order. The values at `position` must be
    /// sorted in the range: the positions before it are the same throughout.
    ///
    /// No search per group. Runs of blocks that begin with the current value hold nothing
    /// else and are skipped by galloping over the blocks' first keys; the blocks where the
    /// value changes are decoded one column at a time. Few large groups cost
    /// O(d · log(n / d)) block probes, many small ones a sequential decode of the column:
    /// the cheaper of a search per group and a scan.
    pub(crate) fn groups(
        &self,
        start: usize,
        end: usize,
        position: usize,
        mut f: impl FnMut(u64, usize, usize),
    ) {
        if start >= end {
            return;
        }
        let mut value = self.get(start)[position];
        let mut from = start;
        let mut i = start;
        let mut column = Vec::with_capacity(BLOCK);
        while i < end {
            let mut block = i / BLOCK;
            if i == block * BLOCK {
                // Blocks whose first key has the current value: everything before the last
                // of them has it too.
                let (mut last, mut step) = (block, 1);
                while last + step < self.firsts.len()
                    && (last + step) * BLOCK < end
                    && self.firsts[last + step][position] == value
                {
                    last += step;
                    step *= 2;
                }
                while step > 1 {
                    step /= 2;
                    if last + step < self.firsts.len()
                        && (last + step) * BLOCK < end
                        && self.firsts[last + step][position] == value
                    {
                        last += step;
                    }
                }
                if last > block {
                    block = last;
                    i = block * BLOCK;
                }
            }
            let block_end = ((block + 1) * BLOCK).min(end);
            let header = &self.headers[block];
            let (tw, pw) = (header.widths[2 * position], header.widths[2 * position + 1]);
            if tw == 0 && pw == 0 && !header.palette(position) {
                let constant = (u64::from(header.tag_min[position]) << TAG_SHIFT)
                    | header.payload_min[position];
                if constant != value {
                    f(value, from, i);
                    (value, from) = (constant, i);
                }
            } else {
                column.clear();
                self.decode_column(
                    block,
                    i - block * BLOCK,
                    block_end - block * BLOCK,
                    position,
                    &mut column,
                );
                for (k, &v) in column.iter().enumerate() {
                    if v != value {
                        f(value, from, i + k);
                        (value, from) = (v, i + k);
                    }
                }
            }
            i = block_end;
        }
        f(value, from, end);
    }

    /// Appends, for each of `positions`, the key component at that position of the keys
    /// `start..end` to the matching vector of `out`: block by block, a column at a time.
    pub(crate) fn decode_columns(
        &self,
        start: usize,
        end: usize,
        positions: &[usize],
        out: &mut [Vec<u64>],
    ) {
        for column in out.iter_mut() {
            column.reserve(end.saturating_sub(start));
        }
        let mut i = start;
        while i < end {
            let block = i / BLOCK;
            let block_end = ((block + 1) * BLOCK).min(end);
            let (j0, j1) = (i - block * BLOCK, block_end - block * BLOCK);
            for (&position, column) in positions.iter().zip(out.iter_mut()) {
                self.decode_column(block, j0, j1, position, column);
            }
            i = block_end;
        }
    }

    /// Appends component `c` of the keys `j0..j1` of `block` (indices within the block).
    fn decode_column(&self, block: usize, j0: usize, j1: usize, c: usize, out: &mut Vec<u64>) {
        let header = &self.headers[block];
        let data = &self.data[header.offset as usize..];
        let (tw, pw) = (header.widths[2 * c], header.widths[2 * c + 1]);
        if header.palette(c) {
            let palette = &data[header.payload_min[c] as usize..];
            let mut bit = header.starts[2 * c + 1] as usize + j0 * pw as usize;
            out.extend((j0..j1).map(|_| {
                let value = palette[read(data, bit, pw) as usize];
                bit += pw as usize;
                value
            }));
            return;
        }
        let tag_min = u64::from(header.tag_min[c]);
        let payload_min = header.payload_min[c];
        let (mut tag_bit, mut payload_bit) = (
            header.starts[2 * c] as usize + j0 * tw as usize,
            header.starts[2 * c + 1] as usize + j0 * pw as usize,
        );
        out.extend((j0..j1).map(|_| {
            let value = ((tag_min + read(data, tag_bit, tw)) << TAG_SHIFT)
                | (payload_min + read(data, payload_bit, pw));
            tag_bit += tw as usize;
            payload_bit += pw as usize;
            value
        }));
    }

    /// The index range of keys in `[low, high]`: a lower bound, then the upper bound found
    /// by galloping over the block first keys from the lower bound's block (short ranges end
    /// in the same or a nearby block).
    pub(crate) fn range(&self, low: &Key, high: &Key) -> (usize, usize) {
        let start = self.bound_in(0, self.len, low, false);
        if start == self.len {
            return (start, start);
        }
        let blocks = self.firsts.len();
        let (mut from, mut step) = (start / BLOCK + 1, 1);
        // Blocks before `from` start at or below `high` (or hold `start`).
        while from + step - 1 < blocks && self.firsts[from + step - 1] <= *high {
            from += step;
            step *= 2;
        }
        let until = (from + step - 1).min(blocks);
        let failing = from + self.firsts[from..until].partition_point(|k| k <= high);
        let end = self.bound_in(start, (failing * BLOCK).min(self.len), high, true);
        (start, end)
    }

    /// The first index in `start..end` whose key is not below `key` (`inclusive`: not at or
    /// below it): a lower (upper) bound. A binary search over the blocks' first keys, then
    /// inside one block, where each probe decodes components only until the first
    /// difference.
    pub(crate) fn bound_in(&self, start: usize, end: usize, key: &Key, inclusive: bool) -> usize {
        if start >= end {
            return start;
        }
        let below = |k: &Key| if inclusive { k <= key } else { k < key };
        let (first_block, last_block) = (start / BLOCK, (end - 1) / BLOCK);
        let failing =
            first_block + 1 + self.firsts[first_block + 1..=last_block].partition_point(below);
        let block = failing - 1;
        let header = &self.headers[block];
        let data = &self.data[header.offset as usize..];
        let base = block * BLOCK;
        let (mut low, mut high) = (start.max(base), end.min(failing * BLOCK));
        while low < high {
            let mid = low + (high - low) / 2;
            let mut ordering = std::cmp::Ordering::Equal;
            for (c, &wanted) in key.iter().enumerate() {
                ordering = header.component(data, c, mid - base).cmp(&wanted);
                if ordering.is_ne() {
                    break;
                }
            }
            let is_below = ordering.is_lt() || (inclusive && ordering.is_eq());
            if is_below {
                low = mid + 1;
            } else {
                high = mid;
            }
        }
        low
    }

    /// Heap bytes held (headers, block firsts and packed bits); mapped ones are the file's.
    pub(crate) fn memory_bytes(&self) -> usize {
        self.headers.heap_bytes() + self.firsts.heap_bytes() + self.data.heap_bytes()
    }

    /// Bytes used in place from a mapped file.
    pub(crate) fn mapped_bytes(&self) -> usize {
        self.headers.mapped_bytes() + self.firsts.mapped_bytes() + self.data.mapped_bytes()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rng(state: &mut u64) -> u64 {
        *state = state
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1442695040888963407);
        *state >> 11
    }

    fn random_keys(state: &mut u64, n: usize) -> Vec<Key> {
        let mut keys: Vec<Key> = (0..n)
            .map(|_| {
                let term = |state: &mut u64| {
                    let tag = [1u64, 3, 4, 9, 10][(rng(state) % 5) as usize];
                    (tag << TAG_SHIFT) | (rng(state) % 5000)
                };
                [
                    term(state),
                    (1 << TAG_SHIFT) | (rng(state) % 8),
                    term(state),
                    0,
                ]
            })
            .collect();
        keys.sort_unstable();
        keys.dedup();
        keys
    }

    #[test]
    fn keys_round_trip_and_search_like_a_slice() {
        let mut state = 5;
        for n in [0, 1, 2, 127, 128, 129, 1000, 5000, 70_000] {
            let keys = random_keys(&mut state, n);
            let packed = PackedKeys::from_sorted(&keys);
            assert_eq!(packed.len(), keys.len());
            for (i, key) in keys.iter().enumerate() {
                assert_eq!(packed.get(i), *key, "n {n} index {i}");
            }
            let mut decoded = Vec::new();
            for start in (0..keys.len()).step_by(BLOCK) {
                let mid = (start + 37).min(keys.len());
                packed.decode_range(start, mid, &mut decoded);
                if mid < (start + BLOCK).min(keys.len()) {
                    packed.decode_range(mid, (start + BLOCK).min(keys.len()), &mut decoded);
                }
            }
            assert_eq!(decoded, keys, "decode_range, n {n}");
            for _ in 0..200 {
                if keys.is_empty() {
                    assert_eq!(packed.bound_in(0, 0, &[0; 4], false), 0);
                    break;
                }
                let probe = keys[(rng(&mut state) as usize) % keys.len()];
                let a = (rng(&mut state) as usize) % (keys.len() + 1);
                let b = (rng(&mut state) as usize) % (keys.len() + 1);
                let (start, end) = (a.min(b), a.max(b));
                let expected = start + keys[start..end].partition_point(|k| k < &probe);
                assert_eq!(packed.bound_in(start, end, &probe, false), expected);
                let high = [probe[0], u64::MAX, u64::MAX, u64::MAX];
                let from = keys.partition_point(|k| k < &probe);
                let to = keys.partition_point(|k| k <= &high);
                assert_eq!(packed.range(&probe, &high), (from, to));
                let upper = start + keys[start..end].partition_point(|k| k <= &probe);
                assert_eq!(packed.bound_in(start, end, &probe, true), upper);
                // A probe between stored keys, too.
                let between = [probe[0], probe[1], probe[2] + 1, 0];
                let expected = start + keys[start..end].partition_point(|k| k < &between);
                assert_eq!(packed.bound_in(start, end, &between, false), expected);
            }
        }
    }

    #[test]
    fn groups_are_the_runs_of_equal_values() {
        let mut state = 23;
        // Few large groups (position 1 has 8 values under each subject) and many small
        // ones (position 0), plus long constant stretches.
        let mut long: Vec<Key> = (0..20_000u64)
            .map(|i| {
                [
                    (1 << TAG_SHIFT) | (i / 3000),
                    (1 << TAG_SHIFT) | (i % 7),
                    i,
                    0,
                ]
            })
            .collect();
        long.sort_unstable();
        for keys in [
            random_keys(&mut state, 70_000),
            random_keys(&mut state, 300),
            long,
        ] {
            let packed = PackedKeys::from_sorted(&keys);
            for _ in 0..100 {
                // A range with a fixed first component, grouped on the second; or the whole
                // array grouped on the first.
                let (start, end, position) = if rng(&mut state).is_multiple_of(4) {
                    (0, keys.len(), 0)
                } else {
                    let probe = keys[(rng(&mut state) as usize) % keys.len()][0];
                    let start = keys.partition_point(|k| k[0] < probe);
                    let end = keys.partition_point(|k| k[0] <= probe);
                    (start, end, 1)
                };
                let mut expected = Vec::new();
                let mut i = start;
                while i < end {
                    let j = i + keys[i..end].partition_point(|k| k[position] == keys[i][position]);
                    expected.push((keys[i][position], i, j));
                    i = j;
                }
                let mut got = Vec::new();
                packed.groups(start, end, position, |v, from, to| got.push((v, from, to)));
                assert_eq!(got, expected, "{start}..{end} at {position}");
            }
        }
    }

    #[test]
    fn serialised_keys_read_back() {
        let mut state = 17;
        for n in [0, 1, 128, 1000, 70_000] {
            let keys = random_keys(&mut state, n);
            let packed = PackedKeys::from_sorted(&keys);
            let streamed = PackedKeys::from_sorted_iter(keys.iter().copied());
            let mut bytes = Vec::new();
            packed
                .write(Some(0), &mut |piece| {
                    bytes.extend_from_slice(piece);
                    Ok(())
                })
                .unwrap();
            let mut streamed_bytes = Vec::new();
            streamed
                .write(Some(0), &mut |piece| {
                    streamed_bytes.extend_from_slice(piece);
                    Ok(())
                })
                .unwrap();
            assert_eq!(bytes, streamed_bytes, "streaming packs identically, n {n}");
            bytes.extend_from_slice(b"trailing");
            let (read, used) = PackedKeys::read(&bytes, Some(0), None, true).unwrap();
            assert_eq!(used, bytes.len() - 8);
            assert_eq!(
                (0..read.len()).map(|i| read.get(i)).collect::<Vec<_>>(),
                keys
            );
            if n > 0 {
                let mut damaged = bytes.clone();
                damaged[24 + 44] = 61; // a tag width no header can have
                assert!(PackedKeys::read(&damaged, Some(0), None, true).is_err());
                assert!(PackedKeys::read(&bytes[..bytes.len() - 16], Some(0), None, true).is_err());
            }
        }
    }

    /// Written at any file position with padding, the data is used in place from a map
    /// of the file, and reads like the original.
    #[test]
    fn padded_keys_are_used_in_place_from_a_map() {
        let mut state = 29;
        let dir = tempfile::tempdir().unwrap();
        for (n, at) in [(1000, 0u64), (70_000, 3), (129, 13), (0, 5)] {
            let keys = random_keys(&mut state, n);
            let packed = PackedKeys::from_sorted(&keys);
            let mut bytes = vec![0xaa; at as usize];
            packed
                .write(Some(at), &mut |piece| {
                    bytes.extend_from_slice(piece);
                    Ok(())
                })
                .unwrap();
            let path = dir.path().join(format!("keys-{n}"));
            std::fs::write(&path, &bytes).unwrap();
            let map = crate::mapped::map(&path).unwrap();
            let (read, used) =
                PackedKeys::read(&map[at as usize..], Some(at), Some(&map), false).unwrap();
            assert_eq!(used, bytes.len() - at as usize);
            assert_eq!(read.memory_bytes(), 0, "n {n} at {at}");
            assert_eq!(read.mapped_bytes(), packed.memory_bytes(), "n {n} at {at}");
            read.verify().unwrap();
            assert_eq!(
                (0..read.len()).map(|i| read.get(i)).collect::<Vec<_>>(),
                keys
            );
            // Without the map, the same bytes are copied.
            let (copied, _) =
                PackedKeys::read(&bytes[at as usize..], Some(at), None, false).unwrap();
            assert_eq!(copied.mapped_bytes(), 0);
            assert_eq!(copied.len(), keys.len());
        }
    }

    /// Blocks where few values spread far apart (a subject's objects among millions of
    /// ids) take palettes; they read, decode, group and search like the keys.
    #[test]
    fn palettes_are_chosen_where_smaller_and_read_back() {
        let mut state = 31;
        let mut keys: Vec<Key> = (0..20_000u64)
            .map(|i| {
                // Per subject, objects from a handful of far-apart identifiers.
                let object = (1 << TAG_SHIFT) | ((rng(&mut state) % 6) * 40_000_000_000);
                [
                    (1 << TAG_SHIFT) | (i / 50),
                    (1 << TAG_SHIFT) | (i % 3),
                    object,
                    i % 2,
                ]
            })
            .collect();
        keys.sort_unstable();
        keys.dedup();
        let packed = PackedKeys::from_sorted_as(&keys, IndexEncoding::Compact);
        assert!(
            PackedKeys::from_sorted_as(&keys, IndexEncoding::Fast)
                .headers
                .iter()
                .all(|h| (0..4).all(|c| !h.palette(c))),
            "fast takes no palettes"
        );
        assert!(
            packed.headers.iter().any(|h| (0..4).any(|c| h.palette(c))),
            "no palette chosen"
        );
        for (i, key) in keys.iter().enumerate() {
            assert_eq!(packed.get(i), *key, "index {i}");
        }
        let mut decoded = Vec::new();
        for start in (0..keys.len()).step_by(BLOCK) {
            packed.decode_range(start, (start + BLOCK).min(keys.len()), &mut decoded);
        }
        assert_eq!(decoded, keys);
        let mut columns = vec![Vec::new(), Vec::new()];
        packed.decode_columns(5, keys.len() - 3, &[2, 3], &mut columns);
        assert_eq!(
            columns[0],
            keys[5..keys.len() - 3]
                .iter()
                .map(|k| k[2])
                .collect::<Vec<_>>()
        );
        for probe in keys.iter().step_by(97) {
            let expected = keys.partition_point(|k| k < probe);
            assert_eq!(packed.bound_in(0, keys.len(), probe, false), expected);
        }
        packed.verify().unwrap();
        let mut bytes = Vec::new();
        packed
            .write(Some(0), &mut |piece| {
                bytes.extend_from_slice(piece);
                Ok(())
            })
            .unwrap();
        let (read, _) = PackedKeys::read(&bytes, Some(0), None, true).unwrap();
        assert_eq!(
            (0..read.len()).map(|i| read.get(i)).collect::<Vec<_>>(),
            keys
        );
        // Smaller than the frame of reference alone.
        let frame_only: usize = keys
            .chunks(BLOCK)
            .map(|block| {
                (0..4)
                    .map(|c| {
                        let min = block.iter().map(|k| k[c] & PAYLOAD_MASK).min().unwrap();
                        let max = block.iter().map(|k| k[c] & PAYLOAD_MASK).max().unwrap();
                        block.len() * width(max - min) as usize
                    })
                    .sum::<usize>()
                    .div_ceil(64)
            })
            .sum();
        assert!(
            packed.data.len() < frame_only,
            "{} >= {frame_only}",
            packed.data.len()
        );
    }

    #[test]
    fn full_width_values_survive() {
        let keys = vec![
            [0, 0, 0, 0],
            [u64::MAX >> 4, u64::MAX, 1, 2],
            [u64::MAX, u64::MAX, u64::MAX, u64::MAX],
        ];
        let packed = PackedKeys::from_sorted(&keys);
        for (i, key) in keys.iter().enumerate() {
            assert_eq!(packed.get(i), *key);
        }
    }

    #[test]
    fn typical_keys_compress() {
        // A default-graph SPOG run: few subjects per block, few predicates, mixed objects.
        let mut state = 11;
        let mut keys: Vec<Key> = (0..100_000u64)
            .map(|i| {
                let object = if i % 3 == 0 {
                    (3 << TAG_SHIFT) | (rng(&mut state) % 1_000_000)
                } else {
                    (1 << TAG_SHIFT) | (rng(&mut state) % 1_000_000)
                };
                [
                    (1 << TAG_SHIFT) | (i / 7),
                    (1 << TAG_SHIFT) | (i % 20),
                    object,
                    0,
                ]
            })
            .collect();
        keys.sort_unstable();
        let packed = PackedKeys::from_sorted(&keys);
        let per_key = packed.memory_bytes() as f64 / keys.len() as f64;
        assert!(per_key < 8.0, "{per_key} bytes per key");
    }

    /// Micro-benchmark of the hot paths (random access, bound search, block decode) over a
    /// million realistic keys: `cargo test --release -p nrese-engine --lib keys_bench --
    /// --ignored --nocapture`.
    #[test]
    #[ignore = "benchmark"]
    fn keys_bench() {
        let mut state = 23;
        let mut keys: Vec<Key> = (0..1_000_000u64)
            .map(|i| {
                let object = if i % 3 == 0 {
                    (3 << TAG_SHIFT) | (rng(&mut state) % 3_000_000)
                } else {
                    (1 << TAG_SHIFT) | (rng(&mut state) % 3_000_000)
                };
                [
                    (1 << TAG_SHIFT) | (i / 7),
                    (1 << TAG_SHIFT) | (i % 20),
                    object,
                    0,
                ]
            })
            .collect();
        keys.sort_unstable();
        let packed = PackedKeys::from_sorted(&keys);
        let probes: Vec<Key> = (0..1_000_000)
            .map(|_| keys[(rng(&mut state) % 1_000_000) as usize])
            .collect();
        let positions: Vec<usize> = (0..10_000_000)
            .map(|_| (rng(&mut state) % 1_000_000) as usize)
            .collect();
        for _ in 0..3 {
            let start = std::time::Instant::now();
            let mut sum = 0u64;
            for &i in &positions {
                sum = sum.wrapping_add(packed.get(i)[2]);
            }
            let get = start.elapsed().as_secs_f64() * 1e9 / positions.len() as f64;
            let start = std::time::Instant::now();
            for probe in &probes {
                sum = sum.wrapping_add(packed.bound_in(0, packed.len(), probe, false) as u64);
            }
            let bound = start.elapsed().as_secs_f64() * 1e9 / probes.len() as f64;
            // A pattern range: subject and predicate bound (as counts ask for it).
            let start = std::time::Instant::now();
            for probe in &probes {
                let (low, high) = (
                    [probe[0], probe[1], 0, 0],
                    [probe[0], probe[1], u64::MAX, u64::MAX],
                );
                let (from, to) = packed.range(&low, &high);
                sum = sum.wrapping_add((to - from) as u64);
            }
            let range = start.elapsed().as_secs_f64() * 1e9 / probes.len() as f64;
            let start = std::time::Instant::now();
            let mut out = Vec::with_capacity(BLOCK);
            for round in 0..10 {
                for block in 0..packed.len() / BLOCK {
                    out.clear();
                    packed.decode_range(block * BLOCK, block * BLOCK + BLOCK, &mut out);
                    sum = sum.wrapping_add(out[round % BLOCK][2]);
                }
            }
            let decode = start.elapsed().as_secs_f64() * 1e9 / (10 * packed.len()) as f64;
            eprintln!(
                "get {get:.1} ns | bound_in {bound:.1} ns | range {range:.1} ns | decode {decode:.2} ns/key | {sum}"
            );
        }
    }
}
