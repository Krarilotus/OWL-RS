//! The one k-way merge over sorted permutation runs. Scans and compaction both use it, so
//! the visibility rule lives in exactly one place.
//!
//! Each input is a key range of one [`PermutationRun`]. The merge yields every distinct key
//! once, together with the sum of its signs across the inputs (+1 insert, -1 tombstone).
//! Because of the exact-delta invariant (see [`run`](super::run)), the sum over any
//! contiguous range of runs is -1, 0 or +1, and summation is order-independent, so cursors
//! can be kept in any order and dropped with `swap_remove`.
//!
//! Cost: O(k) per distinct key for k inputs. The engine keeps k at O(log_F n) runs via
//! compaction; a loser tree only pays off for larger k and is left to the Pf work packages.

use super::keys::BLOCK;
use super::run::PermutationRun;
use crate::quad::Key;

struct Cursor<'a> {
    run: &'a PermutationRun,
    pos: usize,
    end: usize,
    /// The key at `pos`, decoded once.
    key: Key,
    /// Keys decoded ahead, starting at index `buffered`; filled on long ranges only, so
    /// point probes decode just the keys they read.
    buffer: Vec<Key>,
    buffered: usize,
}

/// The most a merge over `runs` runs holds at once: its list of cursors and each cursor's
/// buffer of decoded keys, both grown by doubling, so at most an old and a new block of
/// each at once (a buffer's first block is one key short of [`super::keys::BLOCK`], the
/// next grows it to twice that).
pub(crate) fn scan_bytes(runs: usize) -> usize {
    let cursors = runs.next_power_of_two().max(4);
    let buffer = 3 * super::keys::BLOCK * std::mem::size_of::<Key>();
    3 * cursors * std::mem::size_of::<Cursor<'_>>() + runs * buffer
}

/// A range at least this long left in the current block is decoded block-wise.
const BUFFER_FROM: usize = 16;

/// `n` units of a merge's work, counted where they are done in a build with the work
/// counters ([`crate::witness`]; REAS-3a fix check r1 F1); nothing elsewhere.
#[inline(always)]
fn worked(n: usize) {
    #[cfg(any(test, all(feature = "witness", debug_assertions)))]
    crate::witness::add(crate::witness::Site::MergeWork, n as u64);
    let _ = n;
}

impl Cursor<'_> {
    #[inline]
    fn key(&self) -> &Key {
        &self.key
    }

    /// Moves to the next entry; false at the end.
    #[inline]
    fn advance(&mut self) -> bool {
        worked(1);
        self.pos += 1;
        if self.pos == self.end {
            return false;
        }
        let offset = self.pos - self.buffered;
        if offset < self.buffer.len() {
            self.key = self.buffer[offset];
            return true;
        }
        let block_end = self.end.min((self.pos / BLOCK + 1) * BLOCK);
        if block_end - self.pos >= BUFFER_FROM {
            worked(block_end - self.pos);
            self.buffer.clear();
            self.run
                .keys
                .decode_range(self.pos, block_end, &mut self.buffer);
            self.buffered = self.pos;
            self.key = self.buffer[0];
        } else {
            self.key = self.run.keys.get(self.pos);
        }
        true
    }

    #[inline]
    fn sign(&self) -> i64 {
        if self.run.is_tombstone(self.pos) {
            -1
        } else {
            1
        }
    }
}

pub(crate) struct SignedMerge<'a> {
    cursors: Vec<Cursor<'a>>,
    /// The entries the cursors have left: summed as they are made, less one for each entry a
    /// step passes, so [`Self::remaining`] reads no cursor (REAS-3a review r1 R2: a counted
    /// scan asks it before it asks its visitor).
    left: usize,
}

impl<'a> SignedMerge<'a> {
    /// `parts` are `(run, start, end)` index ranges; empty ranges are dropped up front.
    pub(crate) fn new(parts: impl IntoIterator<Item = (&'a PermutationRun, usize, usize)>) -> Self {
        let mut merge = Self {
            cursors: Vec::new(),
            left: 0,
        };
        merge.refill(parts);
        merge
    }

    /// The merge of `parts` in place of what this one had left, its cursors' memory reused
    /// (a probe cursor's per-probe merge: [`super::cursor`]).
    pub(crate) fn refill(
        &mut self,
        parts: impl IntoIterator<Item = (&'a PermutationRun, usize, usize)>,
    ) {
        let mut left = 0;
        self.cursors.clear();
        self.cursors.extend(
            parts
                .into_iter()
                .filter(|&(_, start, end)| start < end)
                .map(|(run, pos, end)| {
                    worked(1);
                    left += end - pos;
                    Cursor {
                        run,
                        pos,
                        end,
                        key: run.keys.get(pos),
                        buffer: Vec::new(),
                        buffered: pos + 1,
                    }
                }),
        );
        self.left = left;
    }

    /// The entries left: an upper bound on the keys still to come. O(1).
    pub(crate) fn remaining(&self) -> usize {
        self.left
    }

    /// The cursors the next step compares: what a step costs ([`crate::visit`]: a unit each).
    pub(crate) fn active(&self) -> usize {
        self.cursors.len()
    }
}

impl Iterator for SignedMerge<'_> {
    /// A distinct key and the sum of its signs.
    type Item = (Key, i64);

    #[inline]
    fn next(&mut self) -> Option<Self::Item> {
        if let [cursor] = self.cursors.as_mut_slice() {
            // Single input: no comparisons needed.
            let item = (*cursor.key(), cursor.sign());
            self.left -= 1;
            if !cursor.advance() {
                self.cursors.clear();
            }
            return Some(item);
        }
        let key = *self.cursors.iter().map(Cursor::key).min()?;
        worked(self.cursors.len());
        let mut sign = 0;
        let mut i = 0;
        while i < self.cursors.len() {
            worked(1);
            let cursor = &mut self.cursors[i];
            if *cursor.key() == key {
                sign += cursor.sign();
                self.left -= 1;
                if !cursor.advance() {
                    self.cursors.swap_remove(i);
                    continue;
                }
            }
            i += 1;
        }
        Some((key, sign))
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        (0, Some(self.remaining()))
    }
}
