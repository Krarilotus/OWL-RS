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

use super::run::PermutationRun;
use crate::quad::Key;

struct Cursor<'a> {
    run: &'a PermutationRun,
    pos: usize,
    end: usize,
}

impl Cursor<'_> {
    #[inline]
    fn key(&self) -> &Key {
        &self.run.keys[self.pos]
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
}

impl<'a> SignedMerge<'a> {
    /// `parts` are `(run, start, end)` index ranges; empty ranges are dropped up front.
    pub(crate) fn new(parts: impl IntoIterator<Item = (&'a PermutationRun, usize, usize)>) -> Self {
        let cursors = parts
            .into_iter()
            .filter(|&(_, start, end)| start < end)
            .map(|(run, pos, end)| Cursor { run, pos, end })
            .collect();
        Self { cursors }
    }

    /// Upper bound on the remaining entries.
    pub(crate) fn remaining(&self) -> usize {
        self.cursors.iter().map(|c| c.end - c.pos).sum()
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
            cursor.pos += 1;
            if cursor.pos == cursor.end {
                self.cursors.clear();
            }
            return Some(item);
        }
        let key = *self.cursors.iter().map(Cursor::key).min()?;
        let mut sign = 0;
        let mut i = 0;
        while i < self.cursors.len() {
            let cursor = &mut self.cursors[i];
            if *cursor.key() == key {
                sign += cursor.sign();
                cursor.pos += 1;
                if cursor.pos == cursor.end {
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
