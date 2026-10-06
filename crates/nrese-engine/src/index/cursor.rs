//! Cursors with seek over a stack of runs (execution-core.md §3.4): forward-only per run,
//! each seek a finger search from where the last one landed, kept across probes.
//!
//! A scan of a key range binary-searches every run from the root and builds a k-way merge
//! ([`super::merge`]). An index join, a worst-case-optimal join's checks and a membership
//! test probe one range after another, mostly in ascending key order: a [`StackCursor`]
//! remembers per run the lower bound of its last seek and where it landed, and finds the
//! next range by galloping from there ([`PackedKeys::range_from`]): O(log d) for a range d
//! blocks on instead of O(log n). A seek below the last one starts from the root again, so
//! any order is answered, ascending order fast. The merge's cursors are reused, so a probe
//! allocates nothing.
//!
//! [`PackedKeys::range_from`]: super::keys::PackedKeys::range_from

use super::IndexVersion;
use super::keys::BLOCK;
use super::merge::SignedMerge;
use super::run::PermutationRun;
use crate::quad::{Key, Permutation};

/// What a cursor's seeks cost: every seek in a run, and those that searched it from the
/// root (the first in a run, or one below the last).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SeekStats {
    /// Range lookups, one per run per probe.
    pub seeks: u64,
    /// Of those, the searches from the root.
    pub root_searches: u64,
}

impl SeekStats {
    /// Adds `other`'s counts.
    pub fn add(&mut self, other: SeekStats) {
        self.seeks += other.seeks;
        self.root_searches += other.root_searches;
    }
}

/// Keys at least this many left in a block are decoded block-wise.
const DECODE_FROM: usize = 16;

/// A forward cursor over the runs of one version in one permutation.
pub(crate) struct StackCursor<'a> {
    version: &'a IndexVersion,
    permutation: Permutation,
    /// Per run: the lower bound of the last seek and the index it landed on.
    fingers: Vec<Option<(Key, usize)>>,
    parts: Vec<(&'a PermutationRun, usize, usize)>,
    merge: SignedMerge<'a>,
    block: Vec<Key>,
}

impl<'a> StackCursor<'a> {
    pub(crate) fn new(version: &'a IndexVersion, permutation: Permutation) -> Self {
        Self {
            version,
            permutation,
            fingers: vec![None; version.runs().len()],
            parts: Vec::with_capacity(version.runs().len()),
            merge: SignedMerge::new(std::iter::empty()),
            block: Vec::new(),
        }
    }

    pub(crate) fn permutation(&self) -> Permutation {
        self.permutation
    }

    /// The first visible key at or above `low` and at most `high`: a membership test or a
    /// leapfrog step. Where no run has deletions, one finger search per run and the
    /// smallest key they land on; otherwise the merged scan's first key.
    pub(crate) fn first(&mut self, low: &Key, high: &Key, stats: &mut SeekStats) -> Option<Key> {
        let deletions = self
            .version
            .runs()
            .iter()
            .any(|run| !run.permutation(self.permutation).tombstones.is_empty());
        if deletions {
            let mut first = None;
            self.scan(low, high, stats, |key| {
                first = Some(*key);
                false
            });
            return first;
        }
        let mut first: Option<Key> = None;
        for (run, finger) in self.version.runs().iter().zip(&mut self.fingers) {
            let keys = &run.permutation(self.permutation).keys;
            stats.seeks += 1;
            let start = match *finger {
                Some((last, at)) if *low >= last => keys.bound_from(at, low),
                _ => {
                    stats.root_searches += 1;
                    keys.bound_in(0, keys.len(), low, false)
                }
            };
            *finger = Some((*low, start));
            if start < keys.len() {
                let key = keys.get(start);
                if key <= *high && first.is_none_or(|f| key < f) {
                    first = Some(key);
                }
            }
        }
        first
    }

    /// The number of visible keys in `[low, high]`: per run its range's length less twice
    /// its deletions there (the exact-delta invariant, as `IndexVersion::count_plan`), each
    /// range found from the run's finger.
    pub(crate) fn count(&mut self, low: &Key, high: &Key, stats: &mut SeekStats) -> u64 {
        let mut count = 0i64;
        for (run, finger) in self.version.runs().iter().zip(&mut self.fingers) {
            let perm = run.permutation(self.permutation);
            stats.seeks += 1;
            let (start, end) = match *finger {
                Some((last, at)) if *low >= last => perm.keys.range_from(at, low, high),
                _ => {
                    stats.root_searches += 1;
                    perm.range(low, high)
                }
            };
            *finger = Some((*low, start));
            count += (end - start) as i64 - 2 * perm.tombstones_in(start, end) as i64;
        }
        u64::try_from(count).expect("exact deltas never make a range count negative")
    }

    /// Calls `f` with each visible key in `[low, high]`, in order, while it returns true.
    pub(crate) fn scan(
        &mut self,
        low: &Key,
        high: &Key,
        stats: &mut SeekStats,
        mut f: impl FnMut(&Key) -> bool,
    ) {
        self.parts.clear();
        for (run, finger) in self.version.runs().iter().zip(&mut self.fingers) {
            let perm = run.permutation(self.permutation);
            stats.seeks += 1;
            let (start, end) = match *finger {
                Some((last, at)) if *low >= last => perm.keys.range_from(at, low, high),
                _ => {
                    stats.root_searches += 1;
                    perm.range(low, high)
                }
            };
            *finger = Some((*low, start));
            if start < end {
                self.parts.push((perm, start, end));
            }
        }
        match self.parts[..] {
            [] => {}
            // One run without deletions in range: its keys as they are.
            [(perm, start, end)] if perm.tombstones_in(start, end) == 0 => {
                let mut i = start;
                while i < end {
                    let block_end = end.min((i / BLOCK + 1) * BLOCK);
                    if block_end - i >= DECODE_FROM {
                        self.block.clear();
                        perm.keys.decode_range(i, block_end, &mut self.block);
                        if !self.block.iter().all(&mut f) {
                            return;
                        }
                    } else {
                        for j in i..block_end {
                            if !f(&perm.keys.get(j)) {
                                return;
                            }
                        }
                    }
                    i = block_end;
                }
            }
            _ => {
                self.merge.refill(self.parts.drain(..));
                for (key, sign) in &mut self.merge {
                    if sign > 0 && !f(&key) {
                        return;
                    }
                }
            }
        }
    }
}
