//! Where each key of a dictionary arena ends.
//!
//! Checkpoints before format 11 store an 8-byte end offset per key. Format 11 stores block
//! offsets: per block of [`BLOCK`] keys its start in the arena and where its keys' ends
//! are, then each key's end relative to its block's start in 2 bytes, or in 4 where the
//! block's keys take 64 KiB or more. That is about 2.25 bytes a key instead of 8 (Wikidata
//! lexemes: 105 → 28 MiB), read with the same cache misses: one for the block (whose
//! table is 1/32 of the keys' count in bytes) and one for the key's end.

use crate::mapped::Mapped;

/// Keys per block of block offsets.
pub(crate) const BLOCK: usize = 64;

/// The ends of an arena's keys: key `i` spans `start(i)..end(i)`.
pub(crate) trait Ends: Sync {
    /// The number of keys.
    fn count(&self) -> usize;

    /// Where key `i` ends in the arena.
    fn end(&self, i: usize) -> usize;

    /// Where key `i` starts in the arena.
    fn start(&self, i: usize) -> usize {
        if i == 0 { 0 } else { self.end(i - 1) }
    }

    /// The first key in `from..to` that ends after `at` (`to` if none): the key holding
    /// the arena byte `at`. Binary search; the ends are ascending.
    fn holding(&self, from: usize, to: usize, at: usize) -> usize {
        let (mut low, mut high) = (from, to);
        while low < high {
            let middle = low + (high - low) / 2;
            if self.end(middle) <= at {
                low = middle + 1;
            } else {
                high = middle;
            }
        }
        low
    }
}

impl Ends for [u64] {
    fn count(&self) -> usize {
        self.len()
    }

    #[inline]
    fn end(&self, i: usize) -> usize {
        self[i] as usize
    }
}

/// Block offsets (module docs), mapped from a checkpoint.
pub(crate) struct BlockEnds {
    pub(crate) count: usize,
    /// Two words per block: its start in the arena, and the byte position of its keys'
    /// ends in `ends` shifted left by one, the low bit set where they take 4 bytes.
    pub(crate) blocks: Mapped<u64>,
    /// The keys' ends relative to their block's start, 2 or 4 bytes each, little-endian.
    pub(crate) ends: Mapped<u8>,
}

impl Ends for BlockEnds {
    fn count(&self) -> usize {
        self.count
    }

    #[inline]
    fn end(&self, i: usize) -> usize {
        let block = i / BLOCK;
        let start = self.blocks[2 * block] as usize;
        let at = self.blocks[2 * block + 1];
        let wide = at & 1 == 1;
        let at = (at >> 1) as usize;
        let j = i % BLOCK;
        let end = if wide {
            let bytes = &self.ends[at + 4 * j..at + 4 * j + 4];
            u32::from_le_bytes(bytes.try_into().expect("4 bytes")) as usize
        } else {
            let bytes = &self.ends[at + 2 * j..at + 2 * j + 2];
            usize::from(u16::from_le_bytes(bytes.try_into().expect("2 bytes")))
        };
        start + end
    }

    fn start(&self, i: usize) -> usize {
        // The first key of a block starts at the block's start, without a second lookup.
        if i.is_multiple_of(BLOCK) {
            return self.blocks[2 * (i / BLOCK)] as usize;
        }
        self.end(i - 1)
    }
}

impl BlockEnds {
    /// Bytes on file.
    pub(crate) fn bytes(&self) -> usize {
        self.blocks.len() * 8 + self.ends.len()
    }

    /// Checks that the ends ascend and stay within `arena_len`: reads all of them.
    pub(crate) fn verify(&self, arena_len: usize) -> Result<(), String> {
        if self.blocks.len() != 2 * self.count.div_ceil(BLOCK) {
            return Err("dictionary block offsets don't match its length".into());
        }
        let mut previous = 0;
        for i in 0..self.count {
            let block = i / BLOCK;
            let at = (self.blocks[2 * block + 1] >> 1) as usize;
            let width = if self.blocks[2 * block + 1] & 1 == 1 {
                4
            } else {
                2
            };
            if at + width * (i % BLOCK + 1) > self.ends.len() {
                return Err("dictionary block offsets point past their ends".into());
            }
            let end = self.end(i);
            if end < previous || end > arena_len {
                return Err("dictionary end offsets are out of order".into());
            }
            previous = end;
        }
        Ok(())
    }
}

/// Block offsets for keys of `lengths` (module docs): the blocks' two words each, and the
/// keys' relative ends; `None` if a block's keys take 4 GiB or more.
pub(crate) fn encode(lengths: &[u32]) -> Option<(Vec<u64>, Vec<u8>)> {
    let mut blocks = Vec::with_capacity(2 * lengths.len().div_ceil(BLOCK));
    let mut ends = Vec::with_capacity(2 * lengths.len());
    let mut start = 0u64;
    for block in lengths.chunks(BLOCK) {
        let text: u64 = block.iter().map(|&n| u64::from(n)).sum();
        if text > u64::from(u32::MAX) {
            return None;
        }
        let wide = text >= 1 << 16;
        blocks.push(start);
        blocks.push(((ends.len() as u64) << 1) | u64::from(wide));
        let mut end = 0u64;
        for &n in block {
            end += u64::from(n);
            if wide {
                ends.extend_from_slice(&(end as u32).to_le_bytes());
            } else {
                ends.extend_from_slice(&(end as u16).to_le_bytes());
            }
        }
        start += text;
    }
    Some((blocks, ends))
}

/// The offsets of a mapped dictionary base, as its checkpoint's format has them.
pub(crate) enum Offsets {
    /// Before format 11: each key's end.
    Ends(Mapped<u64>),
    Blocks(BlockEnds),
}

impl Offsets {
    pub(crate) fn bytes(&self) -> usize {
        match self {
            Offsets::Ends(ends) => ends.len() * 8,
            Offsets::Blocks(blocks) => blocks.bytes(),
        }
    }
}

impl Ends for Offsets {
    fn count(&self) -> usize {
        match self {
            Offsets::Ends(ends) => ends.len(),
            Offsets::Blocks(blocks) => blocks.count,
        }
    }

    #[inline]
    fn end(&self, i: usize) -> usize {
        match self {
            Offsets::Ends(ends) => ends[i] as usize,
            Offsets::Blocks(blocks) => blocks.end(i),
        }
    }

    #[inline]
    fn start(&self, i: usize) -> usize {
        match self {
            Offsets::Ends(ends) => {
                if i == 0 {
                    0
                } else {
                    ends[i - 1] as usize
                }
            }
            Offsets::Blocks(blocks) => blocks.start(i),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The ends `encode` writes, read back as [`BlockEnds`] reads them.
    fn ends_of(lengths: &[u32]) -> Vec<usize> {
        let (blocks, ends) = encode(lengths).unwrap();
        let end = |i: usize| {
            let block = i / BLOCK;
            let at = blocks[2 * block + 1];
            let (wide, at) = (at & 1 == 1, (at >> 1) as usize);
            let j = i % BLOCK;
            let relative = if wide {
                u32::from_le_bytes(ends[at + 4 * j..at + 4 * j + 4].try_into().unwrap()) as usize
            } else {
                usize::from(u16::from_le_bytes(
                    ends[at + 2 * j..at + 2 * j + 2].try_into().unwrap(),
                ))
            };
            blocks[2 * block] as usize + relative
        };
        (0..lengths.len()).map(end).collect()
    }

    #[test]
    fn block_offsets_give_every_end() {
        // Short keys, a block with a key of 70 000 bytes (4-byte ends), empty keys, and a
        // partial last block.
        let mut lengths: Vec<u32> = (0..200).map(|i| i % 50).collect();
        lengths[70] = 70_000;
        lengths.extend([0, 0, 3]);
        let mut expected = Vec::new();
        let mut end = 0usize;
        for &n in &lengths {
            end += n as usize;
            expected.push(end);
        }
        assert_eq!(ends_of(&lengths), expected);
        let (blocks, ends) = encode(&lengths).unwrap();
        assert_eq!(blocks.len(), 2 * lengths.len().div_ceil(BLOCK));
        // Two bytes a key, four in the wide block.
        assert_eq!(ends.len(), 2 * lengths.len() + 2 * BLOCK);
        assert!(<[u64] as Ends>::holding(&[3, 3, 7, 9][..], 0, 4, 3) == 2);
    }
}
