//! The one k-way merge over sorted permutation runs. Scans and compaction both use it, so
//! the visibility rule lives in exactly one place.
//!
//! Each input is a key range of one [`PermutationRun`]. The merge yields every distinct key
//! once, together with the sum of its signs across the inputs (+1 insert, -1 tombstone).
//! Because of the exact-delta invariant (see [`run`](super::run)), the sum over any
//! contiguous range of runs is -1, 0 or +1, and summation is order-independent, so a key's
//! occurrences can be taken in any order.
//!
//! One step ([`SignedMerge::step`]) serves the plain reads and the counted ones: its work is
//! counted on an [`Account`] a portion at a time, each portion before it is done (REAS-3a fix
//! check r1 F1). While two or more inputs are live, their heads are kept in a binary heap
//! with the least key on top, so each occurrence a step takes costs O(log k) comparisons for
//! k inputs. A last live input is read without comparing.

use std::collections::BinaryHeap;
use std::collections::binary_heap::PeekMut;

use super::keys::BLOCK;
use super::run::PermutationRun;
use crate::quad::Key;
use crate::visit::{Account, Free, Stopped};

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

/// A live cursor's key and its index in the merge's list, ordered so that the heap's top
/// holds the least key. Each comparison is counted where it is made, in a build with the
/// work counters.
#[derive(PartialEq, Eq)]
struct Head {
    key: Key,
    cursor: usize,
}

impl Ord for Head {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        worked(1);
        other
            .key
            .cmp(&self.key)
            .then(other.cursor.cmp(&self.cursor))
    }
}

impl PartialOrd for Head {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

/// The most a merge over `runs` runs holds at once. That is its list of cursors and its
/// heap of their heads, each grown by doubling from a capacity of 4, so at most an old and
/// a new array of each at once. Each cursor's buffer of decoded keys counts too: a buffer's
/// first block is at most [`BLOCK`] keys, and the next grows it to twice that. With one
/// live input the heap holds nothing.
pub(crate) fn scan_bytes(runs: usize) -> usize {
    let capacity = runs.next_power_of_two().max(4);
    let buffer = 3 * BLOCK * std::mem::size_of::<Key>();
    3 * capacity * (std::mem::size_of::<Cursor<'_>>() + std::mem::size_of::<Head>()) + runs * buffer
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

/// The levels of a binary heap of `len` entries: what a push onto it compares at most. A
/// pop or a change of its top compares at most twice that.
#[inline]
fn levels(len: usize) -> u64 {
    u64::from(usize::BITS - len.leading_zeros())
}

impl Cursor<'_> {
    /// The units [`Self::advance`] does, and what a step does with the key it was at. Moving
    /// and reading the sign is 1, and handing the key on is 1. Then 1 more for reading the
    /// next key, or the keys of its block it decodes (at most [`BLOCK`]).
    #[inline]
    fn advance_cost(&self) -> u64 {
        let next = self.pos + 1;
        if next == self.end || next - self.buffered < self.buffer.len() {
            return 2;
        }
        let block_end = self.end.min((next / BLOCK + 1) * BLOCK);
        match block_end - next {
            decoded if decoded >= BUFFER_FROM => 2 + decoded as u64,
            _ => 3,
        }
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
            worked(1);
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
    /// Every cursor made, each at a fixed index; an exhausted one is in neither `heads` nor
    /// `single`, and its buffer is freed.
    cursors: Vec<Cursor<'a>>,
    /// The live cursors' heads while two or more are live; empty otherwise.
    heads: BinaryHeap<Head>,
    /// The live cursor while just one is: read without comparing.
    single: Option<usize>,
    /// The entries the live cursors have left: summed as they are made, less one for each
    /// occurrence a step takes, so [`Self::remaining`] reads no cursor (REAS-3a review r1 R2).
    left: usize,
    /// A step its account stopped. Every later step answers [`Stopped`] and does nothing,
    /// whatever its account, until a refill makes this merge a new one (design attack D6).
    stopped: bool,
}

impl<'a> SignedMerge<'a> {
    /// `parts` are `(run, start, end)` index ranges; empty ranges are dropped up front.
    pub(crate) fn new(parts: impl IntoIterator<Item = (&'a PermutationRun, usize, usize)>) -> Self {
        let mut merge = Self {
            cursors: Vec::new(),
            heads: BinaryHeap::new(),
            single: None,
            left: 0,
            stopped: false,
        };
        merge.refill(parts);
        merge
    }

    /// The merge of `parts` in place of what this one had left, its cursors' and heads'
    /// memory reused (a probe cursor's per-probe merge: [`super::cursor`]).
    pub(crate) fn refill(
        &mut self,
        parts: impl IntoIterator<Item = (&'a PermutationRun, usize, usize)>,
    ) {
        self.cursors.clear();
        self.heads.clear();
        (self.single, self.left, self.stopped) = (None, 0, false);
        for part in parts {
            let added = self.add(part, &mut Free);
            debug_assert!(added, "nothing is counted");
        }
    }

    /// Adds the range `start..end` of `run`; an empty one adds nothing. Its first key's read,
    /// and the push of its head onto the others', are counted on `account` before they are
    /// done. False once the account has stopped, and then the merge is stopped too.
    pub(crate) fn add(
        &mut self,
        (run, pos, end): (&'a PermutationRun, usize, usize),
        account: &mut impl Account,
    ) -> bool {
        if pos >= end {
            return true;
        }
        let push = match self.single {
            Some(_) => 1,
            None => levels(self.heads.len()),
        };
        if self.stopped || !account.take(1 + push) {
            self.stopped = true;
            return false;
        }
        worked(1);
        let (key, cursor) = (run.keys.get(pos), self.cursors.len());
        self.cursors.push(Cursor {
            run,
            pos,
            end,
            key,
            buffer: Vec::new(),
            buffered: pos + 1,
        });
        self.left += end - pos;
        if let Some(other) = self.single.take() {
            let other_key = self.cursors[other].key;
            self.heads.push(Head {
                key: other_key,
                cursor: other,
            });
            self.heads.push(Head { key, cursor });
        } else if self.heads.is_empty() {
            self.single = Some(cursor);
        } else {
            self.heads.push(Head { key, cursor });
        }
        true
    }

    /// The entries left: an upper bound on the keys still to come. O(1).
    pub(crate) fn remaining(&self) -> usize {
        self.left
    }

    /// The next distinct key and the sum of its signs, or `None` at the end. Its work is
    /// counted on `account` a portion at a time, each before it is done: for each occurrence
    /// it takes, the cursor's advance with what it reads ([`Cursor::advance_cost`]), then
    /// the heap's repair together with the test of whether the next head holds the same key.
    /// The end is counted too. [`Stopped`] once the account has stopped, and the partial
    /// sum is dropped. After that every step answers [`Stopped`] and does nothing.
    #[inline]
    pub(crate) fn step(
        &mut self,
        account: &mut impl Account,
    ) -> Result<Option<(Key, i64)>, Stopped> {
        if self.stopped {
            return Err(Stopped);
        }
        if self.heads.len() == 1 {
            // The last other head went in the step before. Popping a heap's last head
            // compares nothing.
            self.single = self.heads.pop().map(|head| head.cursor);
        }
        if let Some(i) = self.single {
            let cursor = &mut self.cursors[i];
            if !account.take(cursor.advance_cost()) {
                return self.stop();
            }
            let item = (cursor.key, cursor.sign());
            self.left -= 1;
            if !cursor.advance() {
                cursor.buffer = Vec::new();
                self.single = None;
            }
            return Ok(Some(item));
        }
        let Some(key) = self.heads.peek().map(|head| head.key) else {
            // Counted too, so a stopped account's read answers Stopped, not the end.
            return match account.take(1) {
                true => Ok(None),
                false => self.stop(),
            };
        };
        let mut sign = 0;
        loop {
            let repair = 2 * levels(self.heads.len()) + 1;
            let Some(mut top) = self.heads.peek_mut() else {
                break;
            };
            let cursor = &mut self.cursors[top.cursor];
            if !account.take(cursor.advance_cost()) || !account.take(repair) {
                drop(top);
                return self.stop();
            }
            sign += cursor.sign();
            self.left -= 1;
            if cursor.advance() {
                top.key = cursor.key;
                drop(top);
            } else {
                cursor.buffer = Vec::new();
                let _ = PeekMut::pop(top);
            }
            worked(1);
            if self.heads.peek().is_none_or(|head| head.key != key) {
                break;
            }
        }
        Ok(Some((key, sign)))
    }

    fn stop<T>(&mut self) -> Result<T, Stopped> {
        self.stopped = true;
        Err(Stopped)
    }
}

impl Iterator for SignedMerge<'_> {
    /// A distinct key and the sum of its signs.
    type Item = (Key, i64);

    /// [`SignedMerge::step`], counting nothing. A merge that a counted read stopped reads as
    /// ended here: the plain reads never stop one, and the counted reader keeps getting
    /// [`Stopped`] from its own steps (design attack r2, E3).
    #[inline]
    fn next(&mut self) -> Option<Self::Item> {
        self.step(&mut Free).unwrap_or(None)
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        (0, Some(self.remaining()))
    }
}
