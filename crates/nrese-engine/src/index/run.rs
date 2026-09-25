//! Immutable sorted runs.
//!
//! A run stores one sorted key array per [`Permutation`] plus a tombstone bitset (empty for
//! runs without deletes). Runs are built once and never mutated; versions share them via
//! `Arc`, which is what makes snapshots free.
//!
//! Invariant (exact deltas): an insert entry never shadows a visible quad and a tombstone
//! always shadows exactly one visible quad in older runs. Therefore the visibility of a key
//! is the *sum* of its entries across runs (+1 insert, -1 tombstone) and is either 0 or 1,
//! independent of run order. Merges of any adjacent runs just add the signs.

use rayon::prelude::*;

use crate::quad::{EncodedQuad, Key, Permutation};

/// Deltas at least this large are sorted with all six permutations in parallel.
const PARALLEL_BUILD_THRESHOLD: usize = 16 * 1024;

#[derive(Debug, Default)]
pub(crate) struct PermutationRun {
    pub(crate) keys: Box<[Key]>,
    /// Bitset over `keys` positions; empty when the run has no tombstones.
    pub(crate) tombstones: Box<[u64]>,
}

impl PermutationRun {
    #[inline]
    pub(crate) fn is_tombstone(&self, index: usize) -> bool {
        !self.tombstones.is_empty() && (self.tombstones[index / 64] >> (index % 64)) & 1 == 1
    }

    pub(crate) fn tombstone_count(&self) -> u64 {
        self.tombstones
            .iter()
            .map(|word| u64::from(word.count_ones()))
            .sum()
    }

    /// Index range of keys in `[low, high]`. O(log n).
    #[inline]
    pub(crate) fn range(&self, low: &Key, high: &Key) -> (usize, usize) {
        let start = self.keys.partition_point(|k| k < low);
        let end = start + self.keys[start..].partition_point(|k| k <= high);
        (start, end)
    }

    /// Builds a run from entries sorted by key, each flagged as tombstone or not.
    pub(crate) fn from_sorted(entries: Vec<(Key, bool)>) -> Self {
        let has_tombstones = entries.iter().any(|(_, tomb)| *tomb);
        let mut tombstones = if has_tombstones {
            vec![0u64; entries.len().div_ceil(64)]
        } else {
            Vec::new()
        };
        let keys = entries
            .into_iter()
            .enumerate()
            .map(|(i, (key, tomb))| {
                if tomb {
                    tombstones[i / 64] |= 1 << (i % 64);
                }
                key
            })
            .collect();
        Self {
            keys,
            tombstones: tombstones.into_boxed_slice(),
        }
    }
}

#[derive(Debug, Default)]
pub(crate) struct Run {
    perms: [PermutationRun; 6],
    inserts: u64,
    deletes: u64,
}

impl Run {
    /// Builds a run from an exact delta. `inserts` and `deletes` must be disjoint and free of
    /// duplicates (the transaction guarantees this). O(d log d).
    pub(crate) fn from_delta(inserts: &[EncodedQuad], deletes: &[EncodedQuad]) -> Self {
        let build = |permutation: Permutation| {
            let mut entries: Vec<(Key, bool)> = Vec::with_capacity(inserts.len() + deletes.len());
            entries.extend(inserts.iter().map(|q| (permutation.to_key(q), false)));
            entries.extend(deletes.iter().map(|q| (permutation.to_key(q), true)));
            entries.sort_unstable_by(|a, b| a.0.cmp(&b.0));
            PermutationRun::from_sorted(entries)
        };
        Self::from_permutations(build_all(inserts.len() + deletes.len(), build))
    }

    /// Builds a tombstone-free run from arbitrary quads, removing duplicates. Used for bulk
    /// replacement and checkpoint loading. O(n log n), parallel over permutations and within
    /// each sort.
    pub(crate) fn from_quads(mut quads: Vec<EncodedQuad>) -> Self {
        quads.par_sort_unstable();
        quads.dedup();
        let build = |permutation: Permutation| {
            let mut keys: Vec<Key> = quads.par_iter().map(|q| permutation.to_key(q)).collect();
            keys.par_sort_unstable();
            PermutationRun {
                keys: keys.into_boxed_slice(),
                tombstones: Box::default(),
            }
        };
        Self::from_permutations(build_all(quads.len(), build))
    }

    /// Assembles a run from six permutation runs holding the same entries. The counts are
    /// derived from SPOG, so they can't disagree with the data.
    pub(crate) fn from_permutations(perms: [PermutationRun; 6]) -> Self {
        let spog = &perms[Permutation::Spog as usize];
        let deletes = spog.tombstone_count();
        let inserts = spog.keys.len() as u64 - deletes;
        Self {
            perms,
            inserts,
            deletes,
        }
    }

    #[inline]
    pub(crate) fn permutation(&self, permutation: Permutation) -> &PermutationRun {
        &self.perms[permutation as usize]
    }

    #[cfg(test)]
    pub(crate) fn inserts(&self) -> u64 {
        self.inserts
    }

    #[cfg(test)]
    pub(crate) fn deletes(&self) -> u64 {
        self.deletes
    }

    /// Number of entries (inserts + tombstones); the size used by the compaction policy.
    pub(crate) fn entries(&self) -> u64 {
        self.inserts + self.deletes
    }

    /// Net contribution to the visible quad count.
    pub(crate) fn net(&self) -> i64 {
        self.inserts as i64 - self.deletes as i64
    }

    pub(crate) fn memory_bytes(&self) -> u64 {
        self.perms
            .iter()
            .map(|p| (p.keys.len() * size_of::<Key>() + p.tombstones.len() * 8) as u64)
            .sum()
    }

    /// Sign of `quad` in this run: +1 insert, -1 tombstone, 0 absent. O(log n).
    pub(crate) fn sign_of(&self, quad: &EncodedQuad) -> i64 {
        let perm = self.permutation(Permutation::Spog);
        let key = Permutation::Spog.to_key(quad);
        match perm.keys.binary_search(&key) {
            Ok(index) if perm.is_tombstone(index) => -1,
            Ok(_) => 1,
            Err(_) => 0,
        }
    }
}

/// Builds all six permutations, in parallel once `size` makes it worthwhile.
pub(crate) fn build_all(
    size: usize,
    build: impl Fn(Permutation) -> PermutationRun + Sync,
) -> [PermutationRun; 6] {
    if size >= PARALLEL_BUILD_THRESHOLD {
        let mut built: Vec<PermutationRun> =
            Permutation::ALL.par_iter().map(|&p| build(p)).collect();
        std::array::from_fn(|i| std::mem::take(&mut built[i]))
    } else {
        Permutation::ALL.map(build)
    }
}
