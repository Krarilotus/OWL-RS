//! What a BGP's planning read for its overlap counts ([`super::Context::overlaps`]), kept
//! and handed to the step that reads the same data, never read and paid again: a pattern's
//! list becomes its scan's table, or the check of the running rows in place of a probe;
//! the optional intersection buffer becomes the first two steps' result. Owned by one
//! `bgp` frame: whatever no step takes is released when [`Reads`] drops, on any exit.
//!
//! Everything here is charged to an optional part of the request
//! ([`nrese_exec::Envelope::optional_part`]): planning can do without it, so a refusal marks
//! only that part, never the request. A step that takes a list or the buffer charges its
//! bytes to the request's own part as it hands them on ([`super::Context::read_solutions`]).

use nrese_exec::{Budget, Reservation};
use std::sync::Arc;

/// The most lists one BGP's planning keeps. Their directory is then a constant (a few KiB),
/// whatever the pattern count; every BGP of the benchmark suites has far fewer patterns. A
/// pair past it stays only counted, as a pair past the allowance does.
pub(super) const MAX_LISTS: usize = 32;

/// The most values of the optional intersection buffer (32 KiB): the counted pairs are at
/// most [`super::plan::OVERLAP_VALUES`] values together, so their intersection is at most
/// half of that.
pub(super) const BUFFER_VALUES: usize = 4096;

/// One pattern's list: the sorted column of its one variable, as its scan on that variable
/// decodes it, with that column's charge on the optional part.
pub(super) struct List {
    pub(super) pattern: usize,
    /// Declared before `held`: freed before its charge is released.
    pub(super) values: Vec<u64>,
    pub(super) held: Reservation<Arc<Budget>>,
}

/// The shared values of the patterns `pair` (both lists' intersection), sorted, with their
/// charge on the optional part.
pub(super) struct Buffer {
    pub(super) pair: (usize, usize),
    pub(super) values: Vec<u64>,
    pub(super) held: Reservation<Arc<Budget>>,
}

/// The lists and the buffer of one BGP's planning. The directory's charge is taken once,
/// from the number of lists admitted (a length already held).
#[derive(Default)]
pub(super) struct Reads {
    lists: Vec<List>,
    buffer: Option<Buffer>,
    /// The charge of `lists`' capacity, held until it drops; declared after it, so released
    /// after it is freed.
    _directory: Option<Reservation<Arc<Budget>>>,
}

impl Reads {
    /// Room for `lists` lists, charged to `optional` (the optional part): `None` where it
    /// is refused (planning then reads no list, as for a read the budget refuses).
    pub(super) fn with_room(optional: &Arc<Budget>, lists: usize) -> Option<Self> {
        let mut directory = Reservation::new(Arc::clone(optional));
        directory
            .charge(lists.saturating_mul(size_of::<List>()))
            .ok()?;
        Some(Self {
            lists: Vec::with_capacity(lists),
            buffer: None,
            _directory: Some(directory),
        })
    }

    /// Keeps `list`, within the room [`Self::with_room`] made.
    pub(super) fn keep(&mut self, list: List) {
        debug_assert!(
            self.lists.len() < self.lists.capacity(),
            "a list past the room charged"
        );
        self.lists.push(list);
    }

    pub(super) fn list(&self, pattern: usize) -> Option<&[u64]> {
        self.lists
            .iter()
            .find(|list| list.pattern == pattern)
            .map(|list| list.values.as_slice())
    }

    /// Pattern `pattern`'s list, handed to its step.
    pub(super) fn take(&mut self, pattern: usize) -> Option<List> {
        let at = self.lists.iter().position(|list| list.pattern == pattern)?;
        let list = self.lists.swap_remove(at);
        self.settle();
        Some(list)
    }

    /// With no list and no buffer left, frees the directory, then releases its charge: its
    /// room is not held through the steps that follow.
    fn settle(&mut self) {
        if self.lists.is_empty() && self.buffer.is_none() {
            self.lists = Vec::new();
            self._directory = None;
        }
    }

    /// Releases pattern `pattern`'s list (and the buffer if it was one of its pair): its step
    /// reads otherwise, or the result cache replaces it.
    pub(super) fn release(&mut self, pattern: usize) {
        drop(self.take(pattern));
        if self
            .buffer
            .as_ref()
            .is_some_and(|b| b.pair.0 == pattern || b.pair.1 == pattern)
        {
            self.buffer = None;
        }
        self.settle();
    }

    #[cfg(test)]
    pub(super) fn has_buffer(&self) -> bool {
        self.buffer.is_some()
    }

    pub(super) fn set_buffer(&mut self, buffer: Buffer) {
        self.buffer = Some(buffer);
    }

    /// Keeps the buffer only if the order starts with its pair (`first`, then `second`, in
    /// either order of the pair): released at once otherwise, before the cache is consulted.
    /// Step 2 is then a plain join on the pair's one variable: its pattern has no variable
    /// step 1 leaves free, so no triple-term link, and shares one, so no `?a = ?b` link.
    pub(super) fn keep_buffer_for(&mut self, first: usize, second: Option<usize>) {
        let starts = |b: &Buffer| {
            second.is_some_and(|second| b.pair == (first, second) || b.pair == (second, first))
        };
        if !self.buffer.as_ref().is_some_and(starts) {
            self.buffer = None;
            self.settle();
        }
    }

    /// The buffer, for the first two steps, with the length of pattern `first`'s list (the
    /// first step's rows); both lists are released with it taken.
    pub(super) fn take_buffer(&mut self, first: usize) -> Option<(Buffer, usize)> {
        let buffer = self.buffer.take()?;
        let rows = self.list(first).map_or(0, <[u64]>::len);
        drop(self.take(buffer.pair.0));
        drop(self.take(buffer.pair.1));
        self.settle();
        Some((buffer, rows))
    }
}
