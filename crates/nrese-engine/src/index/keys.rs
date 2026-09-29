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
#[derive(Debug, Default)]
pub(crate) struct PackedKeys {
    len: usize,
    headers: Box<[Header]>,
    /// The first key of every block, for the block-level binary search.
    firsts: Box<[Key]>,
    /// Packed bits; one spare word at the end, so reads never cross the slice end.
    data: Box<[u64]>,
}

/// Arrays at least this long are packed in parallel.
const PARALLEL_PACK: usize = 1 << 16;

/// Bits needed for values `0..=max`.
fn width(max: u64) -> u8 {
    (64 - max.leading_zeros()) as u8
}

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
            for (i, key) in keys.iter_mut().enumerate() {
                key[c] = header.component(data, c, j0 + i);
            }
        }
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
}
