//! The new terms of a bulk load, interned without the dictionary's lock and numbered when
//! the load finishes (performance.md §6; investigation of 6 October 2026, #8).
//!
//! A bulk load interned each batch's new terms under the dictionary's write lock, in the
//! order the threads came: on DBpedia core 8 % of the load's samples waited for the lock,
//! and the ids differed from load to load. Here the terms the dictionary doesn't hold yet
//! go to [`SHARDS`] tables by hash, each behind a lock of its own, and each records where
//! in the input it occurs first: (chunk, batch, key in the batch), in file order whatever
//! the chunks and batches are, since a loader numbers its chunks in input order. The
//! quads get provisional ids meanwhile ([`provisional`]).
//!
//! At the end, [`Pending::number`] sorts each shard by first occurrence and merges them:
//! the terms take the next dense ids in the order they first occur in the input, the same
//! at any thread count. Only ids move: the text stays in the shards' arenas, which become
//! a segment of the dictionary ([`Adopted`]), and the quads are renumbered through the
//! shards ([`Adopted::renumber`]).

use std::cmp::Reverse;
use std::collections::BinaryHeap;
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};

use hashbrown::HashTable;
use parking_lot::Mutex;
use rayon::prelude::*;

/// Tables new terms are spread over, by hash.
pub(crate) const SHARDS: usize = 256;

/// The payload bit of a provisional id (dictionary indexes stay far below it).
pub(crate) const PROVISIONAL: u64 = 1 << 59;

/// Bits of a position for the key within its batch, and for the batch within its chunk.
const KEY_BITS: u32 = 20;
const BATCH_BITS: u32 = 24;

/// The position of key `key` of batch `batch` of chunk `chunk`: positions compare as the
/// input does.
pub(crate) fn position(chunk: u32, batch: u32, key: usize) -> u64 {
    debug_assert!(chunk < 1 << (64 - KEY_BITS - BATCH_BITS));
    debug_assert!(batch < 1 << BATCH_BITS && key < 1 << KEY_BITS);
    (u64::from(chunk) << (KEY_BITS + BATCH_BITS)) | (u64::from(batch) << KEY_BITS) | key as u64
}

/// The shard of a key with hash `hash`: middle bits, since the shards' tables probe by
/// the low ones and filter by the top ones.
pub(crate) fn shard_of(hash: u64) -> usize {
    (hash >> 32) as usize & (SHARDS - 1)
}

/// The provisional payload of entry `local` of shard `shard`.
pub(crate) fn provisional(shard: usize, local: u32) -> u64 {
    PROVISIONAL | ((shard as u64) << 32) | u64::from(local)
}

/// The shard and entry of a provisional payload; `None` for any other.
pub(crate) fn split(payload: u64) -> Option<(usize, u32)> {
    (payload & PROVISIONAL != 0)
        .then_some((((payload >> 32) as usize) & (SHARDS - 1), payload as u32))
}

/// One shard's keys, one after another, as the dictionary's heap holds them.
#[derive(Default)]
pub(crate) struct Shard {
    pub(crate) bytes: Vec<u8>,
    /// Each key's end offset in `bytes` (a shard's text stays under 4 GiB: the
    /// dictionary's under 1 TiB).
    pub(crate) ends: Vec<u32>,
    /// Each key's hash, for the table to grow by (dropped once numbered).
    hashes: Vec<u64>,
    /// Each key's first position while loading; its final id once numbered (atomic so
    /// that the ranges of the numbering merge write their entries' ids in parallel).
    slots: Vec<AtomicU64>,
    /// Indexes into the keys, by hash.
    table: HashTable<u32>,
}

impl Shard {
    pub(crate) fn len(&self) -> usize {
        self.ends.len()
    }

    pub(crate) fn key(&self, local: usize) -> &[u8] {
        let start = match local {
            0 => 0,
            _ => self.ends[local - 1] as usize,
        };
        &self.bytes[start..self.ends[local] as usize]
    }

    fn find(&self, hash: u64, key: &[u8]) -> Option<u32> {
        self.table
            .find(hash, |&local| self.key(local as usize) == key)
            .copied()
    }

    /// `key`'s entry, added if new, with `at` as its first position if it is earlier.
    fn intern(&mut self, key: &[u8], hash: u64, at: u64) -> u32 {
        if let Some(local) = self.find(hash, key) {
            let first = self.slots[local as usize].get_mut();
            *first = (*first).min(at);
            return local;
        }
        let local = u32::try_from(self.ends.len()).expect("a shard holds fewer than 2^32 terms");
        self.bytes.extend_from_slice(key);
        let end = u32::try_from(self.bytes.len()).expect("a shard's text under 4 GiB");
        self.ends.push(end);
        self.hashes.push(hash);
        self.slots.push(AtomicU64::new(at));
        let Shard { table, hashes, .. } = self;
        table.insert_unique(hash, local, |&i| hashes[i as usize]);
        local
    }
}

/// A bulk load's new terms while it runs.
pub(crate) struct Pending {
    shards: Box<[Mutex<Shard>]>,
}

impl Default for Pending {
    fn default() -> Self {
        Self {
            shards: (0..SHARDS).map(|_| Mutex::default()).collect(),
        }
    }
}

/// A key of a batch for [`Pending::intern`]: where its bytes lie in the batch's arena,
/// its hash, and its position in the input.
pub(crate) struct NewKey {
    pub(crate) start: usize,
    pub(crate) end: usize,
    pub(crate) hash: u64,
    pub(crate) at: u64,
}

impl Pending {
    /// The provisional payload of each of `keys` (bytes in `arena`), interned: each shard
    /// locked once for the batch.
    pub(crate) fn intern(&self, arena: &[u8], keys: &[NewKey]) -> Vec<u64> {
        let mut by_shard: Vec<(u16, u32)> = keys
            .iter()
            .enumerate()
            .map(|(i, key)| (shard_of(key.hash) as u16, i as u32))
            .collect();
        by_shard.sort_unstable();
        let mut out = vec![0u64; keys.len()];
        for run in by_shard.chunk_by(|a, b| a.0 == b.0) {
            let shard = run[0].0 as usize;
            let mut table = self.shards[shard].lock();
            for &(_, i) in run {
                let key = &keys[i as usize];
                let local = table.intern(&arena[key.start..key.end], key.hash, key.at);
                out[i as usize] = provisional(shard, local);
            }
        }
        out
    }

    /// Whether no term is pending.
    pub(crate) fn is_empty(&self) -> bool {
        self.shards.iter().all(|shard| shard.lock().len() == 0)
    }

    /// Numbers the pending terms from `start` in the order of their first occurrence; a
    /// term `existing` finds (interned meanwhile by another path: a triple term's
    /// components) keeps that id and takes no new one. Each shard is sorted on its own
    /// thread; then the merge of the shards hands out the ids, cut into ranges of first
    /// positions (splitters from a sample) merged in parallel, each range's ids starting
    /// after the sizes of those before it. One sequential merge took 2 s of LUBM 1000's
    /// load (33 M terms) while the other threads waited.
    pub(crate) fn number(
        self,
        start: u64,
        existing: impl Fn(u64, &[u8]) -> Option<u64> + Sync,
    ) -> Adopted {
        let mut shards: Vec<Shard> = self
            .shards
            .into_vec()
            .into_iter()
            .map(Mutex::into_inner)
            .collect();
        // Per shard: its new entries by first position; known ones take their ids.
        let orders: Vec<Vec<u32>> = shards
            .par_iter_mut()
            .map(|shard| {
                let mut order: Vec<u32> = Vec::with_capacity(shard.len());
                for local in 0..shard.len() {
                    match existing(shard.hashes[local], shard.key(local)) {
                        Some(id) => *shard.slots[local].get_mut() = id,
                        None => order.push(local as u32),
                    }
                }
                order.sort_unstable_by_key(|&local| shard.slots[local as usize].load(Relaxed));
                // The table finds keys without the hashes from now on, and the arrays
                // shed what growing by doubling left over (a shard at a time, so the
                // copies are small).
                shard.hashes = Vec::new();
                shard.bytes.shrink_to_fit();
                shard.ends.shrink_to_fit();
                shard.slots.shrink_to_fit();
                let Shard {
                    table, bytes, ends, ..
                } = shard;
                table.shrink_to_fit(|&local| {
                    let local = local as usize;
                    let start = if local == 0 {
                        0
                    } else {
                        ends[local - 1] as usize
                    };
                    super::hash::key_hash(&bytes[start..ends[local] as usize])
                });
                order
            })
            .collect();
        let total: usize = orders.iter().map(Vec::len).sum();
        let first = |s: usize, local: u32| shards[s].slots[local as usize].load(Relaxed);
        // Ranges of first positions, a few per thread, from a sample of each shard.
        let ranges = (rayon::current_num_threads() * 4).min(total / 4096 + 1);
        let mut sample: Vec<u64> = orders
            .iter()
            .enumerate()
            .flat_map(|(s, order)| {
                let step = (order.len() / ranges).max(1);
                order
                    .iter()
                    .step_by(step)
                    .map(move |&local| first(s, local))
            })
            .collect();
        sample.sort_unstable();
        let splitters: Vec<u64> = (1..ranges)
            .map(|r| sample[r * sample.len() / ranges])
            .collect();
        // Per shard, where each range begins in its order.
        let bounds: Vec<Vec<usize>> = orders
            .par_iter()
            .enumerate()
            .map(|(s, order)| {
                let mut bounds = Vec::with_capacity(ranges + 1);
                bounds.push(0);
                for &splitter in &splitters {
                    bounds.push(order.partition_point(|&local| first(s, local) < splitter));
                }
                bounds.push(order.len());
                bounds
            })
            .collect();
        let mut locals = vec![0u64; total];
        let mut parts: Vec<(usize, &mut [u64])> = Vec::with_capacity(ranges);
        let (mut rest, mut offset) = (&mut locals[..], 0);
        for r in 0..ranges {
            let size: usize = bounds.iter().map(|b| b[r + 1] - b[r]).sum();
            let (part, tail) = std::mem::take(&mut rest).split_at_mut(size);
            parts.push((offset, part));
            rest = tail;
            offset += size;
        }
        parts
            .into_par_iter()
            .enumerate()
            .for_each(|(r, (offset, out))| {
                let mut heads: BinaryHeap<Reverse<(u64, usize, usize)>> = (0..orders.len())
                    .filter(|&s| bounds[s][r] < bounds[s][r + 1])
                    .map(|s| {
                        let i = bounds[s][r];
                        Reverse((first(s, orders[s][i]), s, i))
                    })
                    .collect();
                let mut n = 0;
                while let Some(Reverse((_, s, i))) = heads.pop() {
                    let local = orders[s][i];
                    shards[s].slots[local as usize].store(start + (offset + n) as u64, Relaxed);
                    out[n] = ((s as u64) << 32) | u64::from(local);
                    n += 1;
                    if i + 1 < bounds[s][r + 1] {
                        heads.push(Reverse((first(s, orders[s][i + 1]), s, i + 1)));
                    }
                }
            });
        Adopted {
            start,
            shards,
            locals,
        }
    }
}

/// A bulk load's terms in the dictionary: the shards' arenas as they were filled, the
/// ids `start..start + len` in the order of first occurrence.
pub(crate) struct Adopted {
    pub(crate) start: u64,
    pub(crate) shards: Vec<Shard>,
    /// Per id from `start`: its shard (high 32 bits) and entry.
    locals: Vec<u64>,
}

impl Adopted {
    /// No terms (a load that interned none).
    pub(crate) fn empty(start: u64) -> Self {
        Self {
            start,
            shards: Vec::new(),
            locals: Vec::new(),
        }
    }

    /// Entries in the segment (not counting keys that turned out to exist already).
    pub(crate) fn len(&self) -> u64 {
        self.locals.len() as u64
    }

    /// The key of id `start + offset`.
    pub(crate) fn key(&self, offset: u64) -> &[u8] {
        let local = self.locals[offset as usize];
        self.shards[(local >> 32) as usize].key(local as u32 as usize)
    }

    /// The id of `key` (hash `hash`), if the load interned it.
    pub(crate) fn find(&self, hash: u64, key: &[u8]) -> Option<u64> {
        let shard = &self.shards[shard_of(hash)];
        shard
            .find(hash, key)
            .map(|local| shard.slots[local as usize].load(Relaxed))
    }

    /// The final id of a provisional one; any other id as it is (an inline value's payload
    /// may have the provisional bit: only dictionary kinds are provisional).
    pub(crate) fn id(&self, id: super::TermId) -> super::TermId {
        if !id.kind().is_dictionary() {
            return id;
        }
        match split(id.payload()) {
            Some((shard, local)) => super::TermId::new(
                id.kind(),
                self.shards[shard].slots[local as usize].load(Relaxed),
            ),
            None => id,
        }
    }

    /// `quads` with their provisional ids replaced by their final ones, in parallel.
    pub(crate) fn renumber(&self, quads: &mut [crate::quad::EncodedQuad]) {
        quads.par_iter_mut().for_each(|quad| {
            *quad = crate::quad::EncodedQuad::new(
                self.id(quad.subject),
                self.id(quad.predicate),
                self.id(quad.object),
                self.id(quad.graph),
            );
        });
    }

    /// Bytes of keys, and of what finds them (offsets, ids, tables).
    pub(crate) fn bytes(&self) -> (u64, u64) {
        let keys = self.shards.iter().map(|s| s.bytes.len() as u64).sum();
        let index = self
            .shards
            .iter()
            .map(|s| {
                (s.ends.capacity() * 4 + s.slots.capacity() * 8 + s.table.capacity() * 5) as u64
            })
            .sum::<u64>()
            + self.locals.capacity() as u64 * 8;
        (keys, index)
    }

    /// The id of entry `local` of shard `shard`, for scans of a shard's arena.
    pub(crate) fn id_of(&self, shard: usize, local: usize) -> u64 {
        self.shards[shard].slots[local].load(Relaxed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hash(key: &[u8]) -> u64 {
        super::super::hash::key_hash(key)
    }

    /// Ids follow first occurrences, whichever order the batches came in (enough terms
    /// that the numbering merge cuts them into several ranges).
    #[test]
    fn ids_follow_first_occurrence_whatever_the_arrival() {
        let words: Vec<Vec<u8>> = (0..50_000)
            .map(|i| format!("Iterm{}", i % 17_000).into_bytes())
            .collect();
        let numbered = |order: &[usize]| {
            let pending = Pending::default();
            // Batches of 100 words; batch b at position (chunk 1, batch b).
            for &b in order {
                let mut arena = Vec::new();
                let keys: Vec<NewKey> = words[b * 100..(b + 1) * 100]
                    .iter()
                    .enumerate()
                    .map(|(k, word)| {
                        let start = arena.len();
                        arena.extend_from_slice(word);
                        NewKey {
                            start,
                            end: arena.len(),
                            hash: hash(word),
                            at: position(1, b as u32, k),
                        }
                    })
                    .collect();
                pending.intern(&arena, &keys);
            }
            let adopted = pending.number(10, |_, _| None);
            (0..adopted.len())
                .map(|i| {
                    let key = adopted.key(i);
                    assert_eq!(adopted.find(hash(key), key), Some(10 + i));
                    key.to_vec()
                })
                .collect::<Vec<_>>()
        };
        let forward: Vec<usize> = (0..500).collect();
        let backward: Vec<usize> = (0..500).rev().collect();
        let a = numbered(&forward);
        assert_eq!(a, numbered(&backward));
        // First occurrences in input order: term0, term1, ... term16999.
        let expected: Vec<Vec<u8>> = (0..17_000)
            .map(|i| format!("Iterm{i}").into_bytes())
            .collect();
        assert_eq!(a, expected);
    }

    #[test]
    fn provisional_payloads_split_back() {
        for (shard, local) in [(0, 0), (255, u32::MAX), (17, 123_456)] {
            assert_eq!(split(provisional(shard, local)), Some((shard, local)));
        }
        assert_eq!(split(12345), None);
    }
}
