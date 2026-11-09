//! A publisher's new entries before they are the dictionary's: the lifecycle contract's
//! private heap (`notes/2026-11-08-dictionary-lifecycle-contract.md` P3–P4, package 1's design
//! note `notes/2026-11-08-p1-prepared-install-design.md`).
//!
//! A [`Suffix`] holds the keys one writer slot holder adds, numbered from the dictionary's
//! length when it opened: their final ids, since no one else adds entries meanwhile. They stay
//! here, charged to the publisher's budget, until its durable step succeeds; then
//! [`Dictionary::prepare`](super::Dictionary::prepare)'s [`Prepared`] hands their `Arc`s to the
//! dictionary without copying or allocating. Dropped instead, they leave nothing behind.
//!
//! Its keys are never sealed into byte blocks: each stays its own `Arc`, so the dictionary can
//! take it at whatever offset its tail has. [`Suffix::freeze`] closes the keys since the last
//! freeze into an immutable [`Chunk`] and returns a [`Frozen`] view holding only chunks: every
//! byte a view can reach is charged by a chunk it keeps alive, and released when the last
//! holder drops.

use std::sync::Arc;

use hashbrown::HashTable;
use nrese_exec::{Budget, BudgetExceeded, ChargedVec, Reservation};

use super::charged::ChargedArc;

use super::dictionary::Dictionary;

/// What an `Arc<[u8]>` of `len` bytes allocates: its two counts, then the bytes, rounded up to
/// the counts' alignment.
fn key_cost(len: usize) -> usize {
    2 * size_of::<usize>() + len.next_multiple_of(align_of::<usize>())
}

/// Keys closed by a freeze: never changed once made, shared by the suffix, its frozen views
/// and a prepared install.
#[cfg_attr(
    not(test),
    expect(dead_code, reason = "the vocabulary freezes a suffix in package 2")
)]
pub(crate) struct Chunk {
    /// The keys' first id.
    start: u64,
    // Declared before the charges: freed before they are released.
    keys: Vec<Arc<[u8]>>,
    hashes: Vec<u64>,
    _lists: [Reservation<Arc<Budget>>; 2],
    /// The keys' bytes and counts.
    _bytes: Reservation<Arc<Budget>>,
}

/// A holder's share of a [`Chunk`], its block charged and released after it is freed
/// ([`ChargedArc`]).
type ChunkRef = ChargedArc<Chunk>;

/// A publisher's new entries, ids `start..start + len`, in the order it added them.
pub(crate) struct Suffix {
    start: u64,
    budget: Arc<Budget>,
    chunks: ChargedVec<ChunkRef>,
    /// Each chunk's first id, inline.
    starts: ChargedVec<u64>,
    /// The keys since the last freeze.
    open: ChargedVec<Arc<[u8]>>,
    open_hashes: ChargedVec<u64>,
    open_bytes: Reservation<Arc<Budget>>,
    /// Every entry's id with its hash, by hash: what finds a key ([`Self::find`]).
    table: HashTable<(u64, u64)>,
    /// The table's allocation, as [`table_cost`] bounds it.
    table_bytes: Reservation<Arc<Budget>>,
    len: u64,
}

/// The entries the open lists and the table first make room for: a few, so a handful of
/// terms (a request's own, in a private vocabulary) grow each once, not at every push
/// (`ChargedVec::reserve` doubles from the capacity it has; dictionary lifecycle V1, count 1).
const FIRST_ROOM: usize = 8;

/// What a `HashTable<(u64, u64)>` with room for `capacity` entries allocates: hashbrown's
/// buckets for it (`capacity_to_buckets`: 4 below 4, 8 below 8, else the power of two at or
/// past `capacity · 8/7`), each a 16-byte slot and a control byte, plus one control group (at
/// most 16 bytes on any target).
fn table_cost(capacity: usize) -> usize {
    let buckets = match capacity {
        0..4 => 4,
        4..8 => 8,
        _ => (capacity * 8 / 7).next_power_of_two(),
    };
    buckets * (size_of::<(u64, u64)>() + 1) + 16
}

#[cfg_attr(
    not(test),
    expect(dead_code, reason = "the vocabulary interns into it in package 2")
)]
impl Suffix {
    /// No entries yet; the first gets id `start` (the dictionary's length when its publisher
    /// took the writer slot). What it holds is charged to `budget`.
    pub(crate) fn at(start: u64, budget: Arc<Budget>) -> Self {
        Self {
            start,
            chunks: ChargedVec::new(Arc::clone(&budget)),
            starts: ChargedVec::new(Arc::clone(&budget)),
            open: ChargedVec::new(Arc::clone(&budget)),
            open_hashes: ChargedVec::new(Arc::clone(&budget)),
            open_bytes: Reservation::new(Arc::clone(&budget)),
            table: HashTable::new(),
            table_bytes: Reservation::new(Arc::clone(&budget)),
            budget,
            len: 0,
        }
    }

    /// The id of `key` (whose `key_hash` is `hash`) if it is an entry here.
    pub(crate) fn find(&self, hash: u64, key: &[u8]) -> Option<u64> {
        self.table
            .find(hash, |&(id, _)| self.key(id) == Some(key))
            .map(|&(id, _)| id)
    }

    /// Room for one more entry in the table, charged first: the new table's bytes beside the
    /// old one's while it grows, the old one's released once it is freed.
    fn table_room(&mut self) -> Result<(), BudgetExceeded> {
        if self.table.len() < self.table.capacity() {
            return Ok(());
        }
        let target = (2 * self.table.capacity())
            .max(self.table.len() + 1)
            .max(FIRST_ROOM);
        let held = self.table_bytes.bytes();
        self.table_bytes.charge(table_cost(target))?;
        self.table
            .reserve(target - self.table.len(), |&(_, hash)| hash);
        self.table_bytes.release(held);
        Ok(())
    }

    pub(crate) fn start(&self) -> u64 {
        self.start
    }

    pub(crate) fn len(&self) -> u64 {
        self.len
    }

    /// Adds `key` (an entry neither here nor in the dictionary below `start`: the caller's
    /// lookups) whose `key_hash` is `hash`; returns its id. What it allocates is charged first.
    /// Refused, no entry is added and the entries are as they were; room one of its lists grew
    /// to before the other's was refused stays, charged, for the next push.
    pub(crate) fn push(&mut self, key: &[u8], hash: u64) -> Result<u64, BudgetExceeded> {
        let cost = key_cost(key.len());
        self.open_bytes.charge(cost)?;
        let room = match self.open.is_empty() {
            true => FIRST_ROOM,
            false => 1,
        };
        if let Err(refused) = self
            .open
            .reserve(room)
            .and_then(|()| self.open_hashes.reserve(room))
            .and_then(|()| self.table_room())
        {
            self.open_bytes.release(cost);
            return Err(refused);
        }
        let id = self.start + self.len;
        // Room is reserved and charged: neither push grows its vector, nor the insert its table.
        self.open.push(Arc::from(key))?;
        self.open_hashes.push(hash)?;
        self.table
            .insert_unique(hash, (id, hash), |&(_, hash)| hash);
        self.len += 1;
        Ok(id)
    }

    /// Entry `id`'s key, `None` outside `start..start + len`.
    pub(crate) fn key(&self, id: u64) -> Option<&[u8]> {
        let open_start = self.start + self.len - self.open.len() as u64;
        if id >= self.start + self.len || id < self.start {
            return None;
        }
        if id >= open_start {
            return Some(&self.open[(id - open_start) as usize]);
        }
        chunk_key(&self.chunks, &self.starts, id)
    }

    /// The keys with their hashes, in id order.
    pub(super) fn keys(&self) -> impl Iterator<Item = (&Arc<[u8]>, u64)> {
        self.chunks
            .iter()
            .flat_map(|chunk| chunk.keys.iter().zip(chunk.hashes.iter().copied()))
            .chain(self.open.iter().zip(self.open_hashes.iter().copied()))
    }

    /// A view of the entries now, which later pushes don't change. The keys since the last
    /// freeze become a chunk first; the view holds the chunks, nothing else. Its lists are
    /// charged to this suffix's budget. Refused, no view is made and the entries are as they
    /// were (their keys may be a chunk now: the same keys at the same ids, charged as before).
    pub(crate) fn freeze(&mut self) -> Result<Frozen, BudgetExceeded> {
        self.close()?;
        let mut chunks = ChargedVec::with_capacity(Arc::clone(&self.budget), self.chunks.len())?;
        let mut starts = ChargedVec::with_capacity(Arc::clone(&self.budget), self.starts.len())?;
        chunks.extend_from_slice(&self.chunks)?;
        starts.extend_from_slice(&self.starts)?;
        Ok(Frozen {
            start: self.start,
            len: self.len,
            chunks,
            starts,
        })
    }

    /// Makes the keys since the last freeze a chunk. Refused, the keys stay open, as they were;
    /// room one of its lists grew to before the other's was refused stays, charged.
    fn close(&mut self) -> Result<(), BudgetExceeded> {
        if self.open.is_empty() {
            return Ok(());
        }
        // The chunk's block charged before the keys move into it: refused, they stay open.
        let mut block = Reservation::new(Arc::clone(&self.budget));
        block.charge(ChunkRef::COST)?;
        self.chunks
            .reserve(1)
            .and_then(|()| self.starts.reserve(1))?;
        let start = self.start + self.len - self.open.len() as u64;
        let budget = &self.budget;
        let (keys, keys_charge) =
            std::mem::replace(&mut self.open, ChargedVec::new(Arc::clone(budget))).into_parts();
        let (hashes, hashes_charge) =
            std::mem::replace(&mut self.open_hashes, ChargedVec::new(Arc::clone(budget)))
                .into_parts();
        let bytes = std::mem::replace(&mut self.open_bytes, Reservation::new(Arc::clone(budget)));
        let chunk = ChunkRef::with_reserved(
            Chunk {
                start,
                keys,
                hashes,
                _lists: [keys_charge, hashes_charge],
                _bytes: bytes,
            },
            block,
        );
        // Room is reserved and charged: neither push grows its vector.
        self.chunks.push(chunk)?;
        self.starts.push(start)?;
        Ok(())
    }
}

/// [`Suffix::freeze`]'s view: the entries `start..start + len` when it was taken.
pub(crate) struct Frozen {
    start: u64,
    len: u64,
    chunks: ChargedVec<ChunkRef>,
    starts: ChargedVec<u64>,
}

impl Frozen {
    pub(crate) fn len(&self) -> u64 {
        self.len
    }

    /// Entry `id`'s key, `None` outside `start..start + len`.
    pub(crate) fn key(&self, id: u64) -> Option<&[u8]> {
        if id < self.start || id >= self.start + self.len {
            return None;
        }
        chunk_key(&self.chunks, &self.starts, id)
    }

    /// The entries with their ids, in id order.
    pub(crate) fn entries(&self) -> impl Iterator<Item = (u64, &[u8])> {
        self.chunks.iter().flat_map(|chunk| {
            (chunk.start..)
                .zip(chunk.keys.iter())
                .map(|(id, key)| (id, &**key))
        })
    }
}

/// Entry `id`'s key among `chunks`, which start at `starts`.
fn chunk_key<'c>(chunks: &'c [ChunkRef], starts: &[u64], id: u64) -> Option<&'c [u8]> {
    let at = starts
        .partition_point(|&start| start <= id)
        .checked_sub(1)?;
    let chunk = &chunks[at];
    chunk
        .keys
        .get((id - chunk.start) as usize)
        .map(|key| &**key)
}

/// Why [`Dictionary::prepare`](super::Dictionary::prepare) prepared nothing. Each comes before
/// the publisher's durable step, so the publication is definitely uncommitted.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(
    not(test),
    expect(dead_code, reason = "publications read it in package 9 (activation)")
)]
pub(crate) enum PrepareError {
    /// The dictionary isn't where the suffix began: something else added entries (a broken
    /// invariant: only the writer slot's holder adds them).
    Diverged { start: u64, len: u64 },
    /// The ids would reach the provisional bit bulk loads number with
    /// ([`super::pending::PROVISIONAL`]).
    Full,
    /// The allocator had no room to reserve.
    NoRoom,
}

/// A suffix's entries, room for them reserved in the dictionary: what its publisher installs
/// after its durable step ([`Self::install`]), allocating nothing. Dropped uninstalled (the
/// publication failed), it gives the promise up and the dictionary is as it was.
#[cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "publications install with it in package 9 (activation)"
    )
)]
pub(crate) struct Prepared<'s> {
    pub(super) dictionary: &'s Dictionary,
    pub(super) suffix: &'s Suffix,
    pub(super) installed: bool,
    /// The writer slot it was prepared under: it can't outlive the slot.
    pub(super) _slot: &'s super::slot::WriterGuard<'s>,
    /// Not `Send`: it installs on the thread that prepared it, whose parking state `prepare`
    /// made ([`super::slot::warm_parking`]). A reference to the guard alone would be `Send`
    /// (`MutexGuard<()>` is `Sync`); this field makes the type say it.
    pub(super) _here: std::marker::PhantomData<*const ()>,
}

/// `Prepared` is not `Send`, checked when the crate compiles: were it `Send`, both impls below
/// would apply and the item's trait would be ambiguous (the form `static_assertions`'
/// `assert_not_impl_any!` expands to, without the dependency).
const _: fn() = || {
    trait AmbiguousIfSend<A> {
        fn some_item() {}
    }
    impl<T: ?Sized> AmbiguousIfSend<()> for T {}
    struct Invalid;
    impl<T: ?Sized + Send> AmbiguousIfSend<Invalid> for T {}
    let _ = <Prepared<'static> as AmbiguousIfSend<_>>::some_item;
};

#[cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "publications install with it in package 9 (activation)"
    )
)]
impl Prepared<'_> {
    /// Adds the entries, ids `start..start + len`: the dictionary's next. Allocates nothing
    /// and can't fail.
    pub(crate) fn install(mut self) {
        self.dictionary.install_suffix(self.suffix);
        self.installed = true;
    }
}

impl Drop for Prepared<'_> {
    fn drop(&mut self) {
        if !self.installed {
            self.dictionary.unpromise();
        }
    }
}

#[cfg(test)]
#[path = "suffix_tests.rs"]
mod tests;
