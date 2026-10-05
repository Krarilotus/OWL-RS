//! Immutable sorted runs.
//!
//! A run stores one sorted key array per [`Permutation`], compressed ([`PackedKeys`]), plus
//! a tombstone bitset (empty for runs without deletes). Runs are built once and never mutated; versions share them via
//! `Arc`, which is what makes snapshots free.
//!
//! Invariant (exact deltas): an insert entry never shadows a visible quad and a tombstone
//! always shadows exactly one visible quad in older runs. Therefore the visibility of a key
//! is the *sum* of its entries across runs (+1 insert, -1 tombstone) and is either 0 or 1,
//! independent of run order. Merges of any adjacent runs just add the signs.

use rayon::prelude::*;

use super::Layout;
use super::derive::Ordered;
use super::keys::PackedKeys;
use crate::quad::{EncodedQuad, Key, Permutation};

/// Deltas at least this large are sorted with all of the layout's permutations in parallel.
const PARALLEL_BUILD_THRESHOLD: usize = 16 * 1024;

#[derive(Debug, Default, Clone)]
pub(crate) struct PermutationRun {
    pub(crate) keys: PackedKeys,
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

    /// Tombstones among the positions `start..end`. O(1) without tombstones, else
    /// O((end - start) / 64).
    pub(crate) fn tombstones_in(&self, start: usize, end: usize) -> u64 {
        if self.tombstones.is_empty() || start >= end {
            return 0;
        }
        let (first, last) = (start / 64, (end - 1) / 64);
        let mask_from = |bit: usize| u64::MAX << (bit % 64);
        let mask_to = |bit: usize| u64::MAX >> (63 - bit % 64);
        if first == last {
            return u64::from(
                (self.tombstones[first] & mask_from(start) & mask_to(end - 1)).count_ones(),
            );
        }
        let head = u64::from((self.tombstones[first] & mask_from(start)).count_ones());
        let tail = u64::from((self.tombstones[last] & mask_to(end - 1)).count_ones());
        let middle: u64 = self.tombstones[first + 1..last]
            .iter()
            .map(|word| u64::from(word.count_ones()))
            .sum();
        head + middle + tail
    }

    /// Index range of keys in `[low, high]`. O(log n).
    #[inline]
    pub(crate) fn range(&self, low: &Key, high: &Key) -> (usize, usize) {
        self.keys.range(low, high)
    }

    /// Builds a run from entries sorted by key, each flagged as tombstone or not.
    pub(crate) fn from_sorted(entries: Vec<(Key, bool)>) -> Self {
        let has_tombstones = entries.iter().any(|(_, tomb)| *tomb);
        let mut tombstones = if has_tombstones {
            vec![0u64; entries.len().div_ceil(64)]
        } else {
            Vec::new()
        };
        let keys: Vec<Key> = entries
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
            keys: PackedKeys::from_sorted(&keys),
            tombstones: tombstones.into_boxed_slice(),
        }
    }
}

/// Invariant: exactly the permutations of `layout` are populated; the others are empty.
#[derive(Debug, Default)]
pub(crate) struct Run {
    layout: Layout,
    perms: [PermutationRun; Permutation::COUNT],
    inserts: u64,
    deletes: u64,
}

impl Run {
    /// Builds a run from an exact delta. `inserts` and `deletes` must be disjoint and free of
    /// duplicates (the transaction guarantees this). O(d log d).
    pub(crate) fn from_delta(
        layout: Layout,
        inserts: &[EncodedQuad],
        deletes: &[EncodedQuad],
    ) -> Self {
        let build = |permutation: Permutation| {
            let mut entries: Vec<(Key, bool)> = Vec::with_capacity(inserts.len() + deletes.len());
            entries.extend(inserts.iter().map(|q| (permutation.to_key(q), false)));
            entries.extend(deletes.iter().map(|q| (permutation.to_key(q), true)));
            entries.sort_unstable_by_key(|entry| entry.0);
            PermutationRun::from_sorted(entries)
        };
        Self::from_permutations(
            layout,
            build_all(layout, inserts.len() + deletes.len(), build),
        )
    }

    /// Builds a tombstone-free run from arbitrary quads, removing duplicates. Used for bulk
    /// replacement and checkpoint loading. O(n log n), parallel over permutations and within
    /// each sort.
    pub(crate) fn from_quads(layout: Layout, quads: Vec<EncodedQuad>) -> Self {
        let mut builder = PermutationBuilder::planned(quads, layout.permutations());
        let mut perms: [PermutationRun; Permutation::COUNT] = Default::default();
        for &permutation in layout.permutations() {
            perms[permutation as usize] = PermutationRun {
                keys: builder.packed(permutation),
                tombstones: Box::default(),
            };
        }
        Self::from_permutations(layout, perms)
    }

    /// Assembles a run from the permutation runs of `layout`, which hold the same entries.
    /// The counts are derived from SPOG (part of every layout), so they can't disagree with
    /// the data.
    pub(crate) fn from_permutations(
        layout: Layout,
        perms: [PermutationRun; Permutation::COUNT],
    ) -> Self {
        let spog = &perms[Permutation::Spog as usize];
        let deletes = spog.tombstone_count();
        let inserts = spog.keys.len() as u64 - deletes;
        Self {
            layout,
            perms,
            inserts,
            deletes,
        }
    }

    pub(crate) fn layout(&self) -> Layout {
        self.layout
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

    /// This run (of the default-graph layout) in the quad layout: SPOG, POSG and OSPG
    /// shared or copied, the graph-first permutations from the graph-last ones with each
    /// key's graph moved to the front, the same order since every graph is the default
    /// one. Tombstones stay where they are.
    pub(crate) fn with_quads_layout(&self) -> Self {
        debug_assert_eq!(self.layout, Layout::DefaultGraph);
        let built: Vec<(Permutation, PermutationRun)> = Layout::Quads
            .permutations()
            .par_iter()
            .map(|&permutation| {
                let run = match permutation.graph_last() {
                    None => self.perms[permutation as usize].clone(),
                    Some(last) => {
                        let source = &self.perms[last as usize];
                        PermutationRun {
                            keys: PackedKeys::from_sorted_iter(
                                source.keys.iter().map(|[a, b, c, g]| [g, a, b, c]),
                            ),
                            tombstones: source.tombstones.clone(),
                        }
                    }
                };
                (permutation, run)
            })
            .collect();
        let mut perms: [PermutationRun; Permutation::COUNT] = Default::default();
        for (permutation, run) in built {
            perms[permutation as usize] = run;
        }
        Self {
            layout: Layout::Quads,
            perms,
            inserts: self.inserts,
            deletes: self.deletes,
        }
    }

    /// Net contribution to the visible quad count.
    pub(crate) fn net(&self) -> i64 {
        self.inserts as i64 - self.deletes as i64
    }

    /// Heap bytes held.
    pub(crate) fn memory_bytes(&self) -> u64 {
        self.perms
            .iter()
            .map(|p| (p.keys.memory_bytes() + p.tombstones.len() * 8) as u64)
            .sum()
    }

    /// Bytes used in place from a mapped checkpoint.
    pub(crate) fn mapped_bytes(&self) -> u64 {
        self.perms
            .iter()
            .map(|p| p.keys.mapped_bytes() as u64)
            .sum()
    }

    /// Sign of `quad` in this run: +1 insert, -1 tombstone, 0 absent. O(log n).
    pub(crate) fn sign_of(&self, quad: &EncodedQuad) -> i64 {
        let perm = self.permutation(Permutation::Spog);
        let key = Permutation::Spog.to_key(quad);
        match perm.range(&key, &key) {
            (start, end) if start == end => 0,
            (start, _) if perm.is_tombstone(start) => -1,
            _ => 1,
        }
    }
}

/// The packed permutations of a set of quads, one after another, from one array of keys,
/// 32 bytes per quad, in the quads' place (the same size). A permutation that is another's
/// partitioned by a leading component with few values (PSOG from SPOG, POSG from OSPG,
/// graph-first from graph-last) is derived by a stable counting partition of the keys'
/// positions, 4 bytes each ([`super::derive`]); the others reorder the keys in place and
/// sort them. Knowing
/// the permutations it will be asked for ([`Self::planned`]), it sorts the ones the others
/// derive from and derives those as soon as it can, keeping them packed until asked:
/// default-graph quads take two sorts instead of four, quads in named graphs three
/// instead of seven.
pub(crate) struct PermutationBuilder {
    keys: Vec<Key>,
    /// The order the keys are in.
    order: Permutation,
    /// The permutations still to be asked for and not built yet.
    wanted: Vec<Permutation>,
    /// Permutations built ahead of being asked for.
    ready: Vec<(Permutation, PackedKeys)>,
}

impl PermutationBuilder {
    /// For `quads`, in any order and with duplicates, nothing planned: every permutation
    /// asked for in the layout's order is sorted (the tests' oracle).
    #[cfg(test)]
    pub(crate) fn new(quads: Vec<EncodedQuad>) -> Self {
        Self::planned(quads, &[])
    }

    /// For `quads`, to be asked for the permutations in `wanted`, in any order.
    pub(crate) fn planned(mut quads: Vec<EncodedQuad>, wanted: &[Permutation]) -> Self {
        quads.par_sort_unstable();
        quads.dedup();
        Self {
            keys: quads.into_iter().map(EncodedQuad::components).collect(),
            order: Permutation::Spog,
            wanted: wanted.to_vec(),
            ready: Vec::new(),
        }
    }

    /// The quads' keys in `permutation`, packed.
    pub(crate) fn packed(&mut self, permutation: Permutation) -> PackedKeys {
        self.wanted.retain(|&p| p != permutation);
        if let Some(at) = self.ready.iter().position(|(p, _)| *p == permutation) {
            return self.ready.swap_remove(at).1;
        }
        if permutation == self.order {
            let packed = PackedKeys::from_sorted(&self.keys);
            self.derive_ahead();
            return packed;
        }
        let own = Ordered {
            keys: &self.keys,
            base: self.order,
            order: self.order,
            positions: None,
        };
        if let Some(positions) = own.partitioned(permutation) {
            let packed = self.ordered(permutation, &positions).packed();
            self.derive_ahead_from(permutation, Some(&positions));
            return packed;
        }
        // Sorted: a wanted permutation this one derives from, built ahead, else this one.
        let source = self
            .wanted
            .iter()
            .copied()
            .find(|&source| super::derive::derivable(source, permutation).is_some())
            .unwrap_or(permutation);
        self.sort_into(source);
        if source == permutation {
            let packed = PackedKeys::from_sorted(&self.keys);
            self.derive_ahead();
            return packed;
        }
        self.wanted.retain(|&p| p != source);
        self.ready
            .push((source, PackedKeys::from_sorted(&self.keys)));
        self.derive_ahead();
        self.packed(permutation)
    }

    /// The keys visited in `order` through `positions`.
    fn ordered<'k>(&'k self, order: Permutation, positions: &'k [u32]) -> Ordered<'k> {
        Ordered {
            keys: &self.keys,
            base: self.order,
            order,
            positions: Some(positions),
        }
    }

    /// Reorders the keys into `permutation` and sorts them.
    fn sort_into(&mut self, permutation: Permutation) {
        let order = self.order;
        self.keys.par_iter_mut().for_each(|key| {
            *key = permutation.to_key(&order.key_to_quad(key));
        });
        self.keys.par_sort_unstable();
        self.order = permutation;
    }

    /// Builds the wanted permutations derivable from the keys as they are.
    fn derive_ahead(&mut self) {
        self.derive_ahead_from(self.order, None);
    }

    /// Builds the wanted permutations derivable from the keys visited in `order` through
    /// `positions` (`None`: as they are), and those derivable from them in turn.
    fn derive_ahead_from(&mut self, order: Permutation, positions: Option<&[u32]>) {
        let derivable: Vec<Permutation> = self
            .wanted
            .iter()
            .copied()
            .filter(|&p| super::derive::derivable(order, p).is_some())
            .collect();
        for permutation in derivable {
            let from = Ordered {
                keys: &self.keys,
                base: self.order,
                order,
                positions,
            };
            if let Some(derived) = from.partitioned(permutation) {
                let packed = self.ordered(permutation, &derived).packed();
                self.wanted.retain(|&p| p != permutation);
                self.ready.push((permutation, packed));
                self.derive_ahead_from(permutation, Some(&derived));
            }
        }
    }
}

/// Builds the permutations of `layout` (the others stay empty), in parallel once `size`
/// makes it worthwhile.
pub(crate) fn build_all(
    layout: Layout,
    size: usize,
    build: impl Fn(Permutation) -> PermutationRun + Sync,
) -> [PermutationRun; Permutation::COUNT] {
    let wanted = layout.permutations();
    let mut perms: [PermutationRun; Permutation::COUNT] = Default::default();
    if size >= PARALLEL_BUILD_THRESHOLD {
        let built: Vec<PermutationRun> = wanted.par_iter().map(|&p| build(p)).collect();
        for (&permutation, run) in wanted.iter().zip(built) {
            perms[permutation as usize] = run;
        }
    } else {
        for &permutation in wanted {
            perms[permutation as usize] = build(permutation);
        }
    }
    perms
}
