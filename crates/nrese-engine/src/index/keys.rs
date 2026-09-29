//! Compressed sorted key arrays (Pf1): the storage of one permutation of a run.
//!
//! Keys are cut into blocks of [`BLOCK`]. Within a block each key position is stored as
//! two bit-packed sub-columns, relative to the block's minimum (frame of reference): the
//! 4-bit term kind tag and the 60-bit payload (see [`TermId`](crate::TermId)). A position
//! that is constant in the block (the graph, usually the first component) takes no bits; a
//! position mixing kinds (IRIs and literals as objects) costs a few tag bits plus the
//! payload range instead of 64 bits. Every key stays randomly accessible in O(1), so binary
//! searches run on the compressed data: first over the blocks' first keys, then inside one
//! block.
//!
//! Typical cost on LUBM: 6 to 12 bytes per key instead of 32.

use rayon::prelude::*;

use crate::quad::Key;

/// Keys per block.
pub(crate) const BLOCK: usize = 128;

const TAG_SHIFT: u32 = 60;
const PAYLOAD_MASK: u64 = (1 << TAG_SHIFT) - 1;

/// Frame of reference of one block: per key position, the minimum tag and payload, the bit
/// widths of the packed offsets from them, and where each sub-column starts.
#[derive(Debug, Clone, Copy, Default)]
struct Header {
    /// Word offset of the block's bits in `data`.
    offset: u64,
    payload_min: [u64; 4],
    tag_min: [u8; 4],
    /// Bit widths of the sub-columns: tag then payload, per key position.
    widths: [u8; 8],
    /// Bit offsets of the sub-columns within the block (at most 128 keys × 256 bits).
    starts: [u16; 8],
}

impl Header {
    /// The frame of reference of `block` (at most [`BLOCK`] sorted keys).
    fn of(block: &[Key]) -> Self {
        let mut header = Header::default();
        for c in 0..4 {
            let (tag_min, tag_max) = block.iter().fold((u8::MAX, 0), |(lo, hi), k| {
                let t = (k[c] >> TAG_SHIFT) as u8;
                (lo.min(t), hi.max(t))
            });
            let (payload_min, payload_max) = block.iter().fold((u64::MAX, 0), |(lo, hi), k| {
                let p = k[c] & PAYLOAD_MASK;
                (lo.min(p), hi.max(p))
            });
            header.tag_min[c] = tag_min;
            header.payload_min[c] = payload_min;
            header.widths[2 * c] = width(u64::from(tag_max - tag_min));
            header.widths[2 * c + 1] = width(payload_max - payload_min);
        }
        let mut bit = 0;
        for (start, &w) in header.starts.iter_mut().zip(&header.widths) {
            *start = bit as u16;
            bit += block.len() * w as usize;
        }
        header
    }

    /// Words of packed bits for a block of `n` keys.
    fn words(&self, n: usize) -> usize {
        let bits: usize = self.widths.iter().map(|&w| w as usize).sum();
        (n * bits).div_ceil(64)
    }

    /// Packs `block` into `bits` (zeroed, [`words`](Self::words) long).
    fn pack(&self, block: &[Key], bits: &mut [u64]) {
        for (j, key) in block.iter().enumerate() {
            for (c, &value) in key.iter().enumerate() {
                let tag = u64::from((value >> TAG_SHIFT) as u8 - self.tag_min[c]);
                let payload = (value & PAYLOAD_MASK) - self.payload_min[c];
                let (tw, pw) = (self.widths[2 * c], self.widths[2 * c + 1]);
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

    /// Component `c` of key `j` of the block whose bits start at `data`.
    #[inline]
    fn component(&self, data: &[u64], c: usize, j: usize) -> u64 {
        let (tw, pw) = (self.widths[2 * c], self.widths[2 * c + 1]);
        let tag = u64::from(self.tag_min[c])
            + read(data, self.starts[2 * c] as usize + j * tw as usize, tw);
        let payload =
            self.payload_min[c] + read(data, self.starts[2 * c + 1] as usize + j * pw as usize, pw);
        (tag << TAG_SHIFT) | payload
    }
}

/// A sorted array of keys, compressed per block.
#[derive(Debug, Default, Clone)]
pub(crate) struct PackedKeys {
    len: usize,
    headers: Box<[Header]>,
    /// The first key of every block, for the block-level binary search.
    firsts: Box<[Key]>,
    /// Packed bits; one spare word at the end, so reads never cross the slice end.
    data: Box<[u64]>,
}

/// Serialised size of a [`Header`].
const HEADER_BYTES: usize = 8 + 32 + 4 + 8 + 16;

/// Arrays at least this long are packed in parallel.
const PARALLEL_PACK: usize = 1 << 16;

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
    /// Packs `keys`, which must be sorted. Large arrays are packed in parallel.
    pub(crate) fn from_sorted(keys: &[Key]) -> Self {
        debug_assert!(keys.is_sorted(), "keys must be sorted");
        let parallel = keys.len() >= PARALLEL_PACK;
        let mut headers: Vec<Header> = if parallel {
            keys.par_chunks(BLOCK).map(Header::of).collect()
        } else {
            keys.chunks(BLOCK).map(Header::of).collect()
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
            headers: headers.into_boxed_slice(),
            firsts: keys.chunks(BLOCK).map(|block| block[0]).collect(),
            data: data.into_boxed_slice(),
        }
    }

    /// Packs keys arriving in sorted order (a merged scan), without holding them all.
    pub(crate) fn from_sorted_iter(keys: impl Iterator<Item = Key>) -> Self {
        let mut packed = Self::default();
        let mut data: Vec<u64> = Vec::new();
        let (mut headers, mut firsts) = (Vec::new(), Vec::new());
        let mut block: Vec<Key> = Vec::with_capacity(BLOCK);
        let mut flush = |block: &mut Vec<Key>, data: &mut Vec<u64>| {
            let mut header = Header::of(block);
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
        packed.headers = headers.into_boxed_slice();
        packed.firsts = firsts.into_boxed_slice();
        packed.data = data.into_boxed_slice();
        packed
    }

    /// Serialises the array: `len | blocks | words | headers | firsts | data`, little-endian;
    /// `emit` receives consecutive pieces (large arrays in chunks).
    pub(crate) fn write(
        &self,
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
        let mut drain = |buffer: &mut Vec<u8>, force: bool| -> std::io::Result<()> {
            if force || buffer.len() >= 1 << 16 {
                emit(buffer)?;
                buffer.clear();
            }
            Ok(())
        };
        for header in &self.headers {
            buffer.extend_from_slice(&header.offset.to_le_bytes());
            for min in header.payload_min {
                buffer.extend_from_slice(&min.to_le_bytes());
            }
            buffer.extend_from_slice(&header.tag_min);
            buffer.extend_from_slice(&header.widths);
            for start in header.starts {
                buffer.extend_from_slice(&start.to_le_bytes());
            }
            drain(&mut buffer, false)?;
        }
        for key in &self.firsts {
            for component in key {
                buffer.extend_from_slice(&component.to_le_bytes());
            }
            drain(&mut buffer, false)?;
        }
        for word in &self.data {
            buffer.extend_from_slice(&word.to_le_bytes());
            drain(&mut buffer, false)?;
        }
        drain(&mut buffer, true)
    }

    /// Reads what [`write`](Self::write) wrote from `bytes`, returning the array and the
    /// bytes consumed. The structure is checked (block counts, widths, offsets), so a
    /// damaged file fails here instead of in a later lookup.
    pub(crate) fn read(bytes: &[u8]) -> Result<(Self, usize), String> {
        let mut at = 0;
        let mut take = |n: usize| -> Result<&[u8], String> {
            let piece = bytes.get(at..at + n).ok_or("truncated packed keys")?;
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
        let raw = take(
            blocks
                .checked_mul(HEADER_BYTES)
                .ok_or("header size overflows")?,
        )?;
        let mut headers = Vec::with_capacity(blocks);
        let mut expected_offset = 0u64;
        for (b, piece) in raw.as_chunks::<HEADER_BYTES>().0.iter().enumerate() {
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
            let n = BLOCK.min(len - b * BLOCK);
            let valid_widths = header.widths.chunks(2).all(|w| w[0] <= 4 && w[1] <= 60);
            let mut bit = 0;
            let valid_starts = header
                .starts
                .iter()
                .zip(&header.widths)
                .all(|(&start, &w)| {
                    let ok = start as usize == bit;
                    bit += n * w as usize;
                    ok
                });
            if header.offset != expected_offset || !valid_widths || !valid_starts {
                return Err(format!("damaged header of block {b}"));
            }
            expected_offset += header.words(n) as u64;
            headers.push(header);
        }
        if expected_offset + 1 != words as u64 {
            return Err("packed data length does not match its blocks".into());
        }
        let raw = take(blocks.checked_mul(32).ok_or("first keys overflow")?)?;
        let firsts: Vec<Key> = raw
            .as_chunks::<32>()
            .0
            .iter()
            .map(|piece| std::array::from_fn(|c| u64_at(piece, c)))
            .collect();
        let raw = take(words.checked_mul(8).ok_or("data size overflows")?)?;
        let data: Vec<u64> = raw
            .as_chunks::<8>()
            .0
            .iter()
            .map(|b| u64::from_le_bytes(*b))
            .collect();
        let keys = Self {
            len,
            headers: headers.into_boxed_slice(),
            firsts: firsts.into_boxed_slice(),
            data: data.into_boxed_slice(),
        };
        if (0..blocks).any(|b| keys.get(b * BLOCK) != keys.firsts[b]) {
            return Err("packed first keys do not match their blocks".into());
        }
        Ok((keys, at))
    }

    #[inline]
    pub(crate) fn len(&self) -> usize {
        self.len
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
    /// below it): a lower (upper) bound. Like [`partition_point_in`](Self::partition_point_in)
    /// with a lexicographic comparison, but inside the block each probe decodes components
    /// only until the first difference.
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

    /// The first index in `start..end` whose key fails `pred`, or `end` (as
    /// `slice::partition_point` on `keys[start..end]`, but as an absolute index). `pred` must
    /// hold for a prefix of the range. O(log n).
    pub(crate) fn partition_point_in(
        &self,
        start: usize,
        end: usize,
        pred: impl Fn(&Key) -> bool,
    ) -> usize {
        if start >= end {
            return start;
        }
        // The first block starting inside the range whose first key fails `pred`: the answer
        // lies between the start of the block before it and that block's start.
        let (first_block, last_block) = (start / BLOCK, (end - 1) / BLOCK);
        let failing = first_block
            + 1
            + self.firsts[first_block + 1..=last_block].partition_point(|k| pred(k));
        let mut low = start.max((failing - 1) * BLOCK);
        let mut high = end.min(failing * BLOCK);
        while low < high {
            let mid = low + (high - low) / 2;
            if pred(&self.get(mid)) {
                low = mid + 1;
            } else {
                high = mid;
            }
        }
        low
    }

    /// Bytes held (headers, block firsts and packed bits).
    pub(crate) fn memory_bytes(&self) -> usize {
        self.headers.len() * size_of::<Header>()
            + self.firsts.len() * size_of::<Key>()
            + self.data.len() * 8
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
                    assert_eq!(packed.partition_point_in(0, 0, |_| true), 0);
                    break;
                }
                let probe = keys[(rng(&mut state) as usize) % keys.len()];
                let a = (rng(&mut state) as usize) % (keys.len() + 1);
                let b = (rng(&mut state) as usize) % (keys.len() + 1);
                let (start, end) = (a.min(b), a.max(b));
                let expected = start + keys[start..end].partition_point(|k| k < &probe);
                assert_eq!(
                    packed.partition_point_in(start, end, |k| k < &probe),
                    expected
                );
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
                let position = (rng(&mut state) % 4) as usize;
                let expected = start
                    + keys[start..end]
                        .partition_point(|k| k[0] <= probe[0] && k[position] < u64::MAX);
                assert_eq!(
                    packed.partition_point_in(start, end, |k| k[0] <= probe[0]
                        && k[position] < u64::MAX),
                    expected
                );
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
                .write(&mut |piece| {
                    bytes.extend_from_slice(piece);
                    Ok(())
                })
                .unwrap();
            let mut streamed_bytes = Vec::new();
            streamed
                .write(&mut |piece| {
                    streamed_bytes.extend_from_slice(piece);
                    Ok(())
                })
                .unwrap();
            assert_eq!(bytes, streamed_bytes, "streaming packs identically, n {n}");
            bytes.extend_from_slice(b"trailing");
            let (read, used) = PackedKeys::read(&bytes).unwrap();
            assert_eq!(used, bytes.len() - 8);
            assert_eq!(
                (0..read.len()).map(|i| read.get(i)).collect::<Vec<_>>(),
                keys
            );
            if n > 0 {
                let mut damaged = bytes.clone();
                damaged[24 + 44] = 61; // a tag width no header can have
                assert!(PackedKeys::read(&damaged).is_err());
                assert!(PackedKeys::read(&bytes[..bytes.len() - 16]).is_err());
            }
        }
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
