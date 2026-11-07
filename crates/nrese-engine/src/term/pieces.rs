//! The dictionary's storage, in pieces that never change once made: the mapped base, the
//! segments between it and the heap ([`Segment`]), and the heap of entries interned one by one
//! ([`Heap`]): sealed blocks of [`BLOCK`] keys and a tail of fewer, each key its own `Arc`.
//! [`Inner`] holds them behind the dictionary's lock; a [`View`] holds the same `Arc`s with no
//! lock, for work over the keys that runs in parallel (a scan, a text order, an index build).
//!
//! A view taken under a brief read guard clones the pieces' `Arc`s and the tail keys' (at most
//! [`BLOCK`] of them): it copies no key bytes, however long the keys. Ids are append-only and a
//! piece never changes, so a view answers for every id below its length the key the dictionary
//! had when it was taken, for as long as it lives, across interns, adoptions and rebases. A piece
//! is freed when its last `Arc` drops: a view pins exactly the pieces it holds.

use std::sync::Arc;

use hashbrown::HashTable;
use nrese_exec::{Budget, BudgetExceeded, ChargedVec};

use super::dictionary::Base;
use super::order::Key;
use super::pending::Adopted;

/// Keys in a heap's sealed block: every block holds exactly this many.
pub(super) const BLOCK: usize = 1 << BLOCK_SHIFT;
const BLOCK_SHIFT: u32 = 12;

/// A heap's keys, sealed: never changed once made, shared by the heap and the views.
pub(super) struct Block {
    /// The keys, one after another.
    pub(super) bytes: Box<[u8]>,
    /// Their end offsets in `bytes`.
    pub(super) ends: Box<[u64]>,
}

impl Block {
    /// The keys `keys`, one after another.
    pub(super) fn of<'k>(keys: impl Iterator<Item = &'k [u8]> + Clone) -> Self {
        let mut bytes = Vec::with_capacity(keys.clone().map(<[u8]>::len).sum());
        let mut ends = Vec::with_capacity(keys.size_hint().0);
        for key in keys {
            bytes.extend_from_slice(key);
            ends.push(bytes.len() as u64);
        }
        Self {
            bytes: bytes.into_boxed_slice(),
            ends: ends.into_boxed_slice(),
        }
    }

    pub(super) fn len(&self) -> usize {
        self.ends.len()
    }

    pub(super) fn key(&self, local: usize) -> &[u8] {
        heap_key(&self.bytes, &self.ends, local)
    }
}

/// Entries kept on the heap, from id `start` on: sealed blocks of [`BLOCK`] keys, then the
/// tail, fewer than [`BLOCK`], each key its own `Arc` (a view shares them).
#[derive(Default)]
pub(super) struct Heap {
    pub(super) start: u64,
    pub(super) blocks: Vec<Arc<Block>>,
    pub(super) tail: Vec<Arc<[u8]>>,
    /// Their `key_hash`es: the table grows by moving numbers, without reading and
    /// hashing every key again under the write lock (a bulk load stalled on it). Empty in a
    /// view's heap, which never looks keys up.
    hashes: Vec<u64>,
    /// Their ids, by `key_hash`; empty in a view's heap.
    table: HashTable<u64>,
}

impl Heap {
    pub(super) fn at(start: u64) -> Self {
        Self {
            start,
            ..Self::default()
        }
    }

    pub(super) fn len(&self) -> u64 {
        (self.blocks.len() * BLOCK + self.tail.len()) as u64
    }

    pub(super) fn key(&self, id: u64) -> &[u8] {
        let local = (id - self.start) as usize;
        match self.blocks.get(local >> BLOCK_SHIFT) {
            Some(block) => block.key(local & (BLOCK - 1)),
            None => &self.tail[local - self.blocks.len() * BLOCK],
        }
    }

    pub(super) fn find(&self, hash: u64, key: &[u8]) -> Option<u64> {
        self.table.find(hash, |&id| self.key(id) == key).copied()
    }

    /// Appends `key` (not an entry yet), whose hash is `hash`, and returns its id. The tail is
    /// sealed into a block when it holds [`BLOCK`] keys: its keys are copied once, under the
    /// write lock that appends.
    pub(super) fn push(&mut self, key: &[u8], hash: u64) -> u64 {
        let id = self.start + self.len();
        self.tail.push(Arc::from(key));
        self.hashes.push(hash);
        let Heap {
            table,
            hashes,
            start,
            ..
        } = self;
        table.insert_unique(hash, id, |&i| hashes[(i - *start) as usize]);
        if self.tail.len() == BLOCK {
            let block = Block::of(self.tail.iter().map(|key| &**key));
            self.blocks.push(Arc::new(block));
            self.tail.clear();
        }
        id
    }

    /// The heap of `keys` (none an entry yet, their hashes `hashes`), from id `start`; `None`
    /// if two of them are equal (the duplicate's index). Full blocks are sealed at once.
    pub(super) fn of_keys(start: u64, keys: &[&[u8]], hashes: Vec<u64>) -> Result<Self, u64> {
        let full = keys.len() / BLOCK * BLOCK;
        let mut heap = Heap::at(start);
        heap.blocks = keys[..full]
            .chunks(BLOCK)
            .map(|chunk| Arc::new(Block::of(chunk.iter().copied())))
            .collect();
        heap.tail = keys[full..].iter().map(|&key| Arc::from(key)).collect();
        let mut table = HashTable::with_capacity(keys.len());
        for (index, &hash) in hashes.iter().enumerate() {
            let id = start + index as u64;
            if table
                .find(hash, |&other| heap.key(other) == heap.key(id))
                .is_some()
            {
                return Err(id);
            }
            table.insert_unique(hash, id, |&i| hashes[(i - start) as usize]);
        }
        heap.table = table;
        heap.hashes = hashes;
        Ok(heap)
    }

    /// Its keys as a view reads them: the blocks' and tail keys' `Arc`s, no table.
    fn keys(&self) -> Self {
        Self {
            start: self.start,
            blocks: self.blocks.clone(),
            tail: self.tail.clone(),
            hashes: Vec::new(),
            table: HashTable::new(),
        }
    }

    /// Bytes of keys, and of what finds them: end offsets, hashes, the table, the tail's
    /// `Arc`s (header and pointer).
    fn bytes(&self) -> (u64, u64) {
        let keys = self.blocks.iter().map(|b| b.bytes.len()).sum::<usize>()
            + self.tail.iter().map(|key| key.len()).sum::<usize>();
        let index = self.blocks.len() * BLOCK * 8
            + self.hashes.capacity() * 8
            + self.table.capacity() * 9
            + self.tail.capacity() * 16
            + self.tail.len() * 16;
        (keys as u64, index as u64)
    }
}

/// Entries between the base and the live heap: a heap frozen when a bulk load's terms
/// came after it, or a bulk load's terms in the arenas they were interned into
/// ([`super::pending`]).
pub(super) enum Segment {
    Heap(Heap),
    Adopted(Adopted),
}

impl Segment {
    pub(super) fn key(&self, id: u64) -> &[u8] {
        match self {
            Self::Heap(heap) => heap.key(id),
            Self::Adopted(adopted) => adopted.key(id - adopted.start),
        }
    }

    pub(super) fn find(&self, hash: u64, key: &[u8]) -> Option<u64> {
        match self {
            Self::Heap(heap) => heap.find(hash, key),
            Self::Adopted(adopted) => adopted.find(hash, key),
        }
    }
}

#[derive(Default)]
pub(super) struct Inner {
    /// Entries `0..base.len` from a mapped checkpoint.
    pub(super) base: Option<Arc<Base>>,
    /// The base's length (0 without one), inline: every key's lookup by id reads it first,
    /// and reads no `Arc` to get it.
    base_len: u64,
    /// Entries after the base, before the heap, by id.
    pub(super) segments: Vec<Arc<Segment>>,
    /// Each segment's first id, inline: a key's lookup by id searches these, reading no
    /// segment's `Arc` until it has found the one holding the key.
    starts: Vec<u64>,
    /// The entries interned one by one since, at the end.
    pub(super) heap: Heap,
    /// Advanced by every rebase (the one change of the base): a text order built for one
    /// generation is never installed in another ([`super::order::TextOrder`]).
    pub(super) generation: u64,
}

impl Inner {
    pub(super) fn with_base(base: Base, generation: u64) -> Self {
        Self {
            heap: Heap::at(base.len),
            base_len: base.len,
            base: Some(Arc::new(base)),
            segments: Vec::new(),
            starts: Vec::new(),
            generation,
        }
    }

    pub(super) fn base_len(&self) -> u64 {
        self.base_len
    }

    pub(super) fn len(&self) -> u64 {
        self.heap.start + self.heap.len()
    }

    /// Entry `index`'s plain key: in place, or decoded from a compressed base.
    pub(super) fn key(&self, index: u64) -> Key<'_> {
        if index < self.base_len() {
            return self
                .base
                .as_ref()
                .expect("an index below the base")
                .plain(index);
        }
        Key::Borrowed(self.added_key(index))
    }

    /// Entry `index`'s key, above the base: on the heap or in a segment, in place.
    pub(super) fn added_key(&self, index: u64) -> &[u8] {
        if index >= self.heap.start {
            return self.heap.key(index);
        }
        let at = self.starts.partition_point(|&start| start <= index) - 1;
        self.segments[at].key(index)
    }

    /// [`Self::key`], a key of a compressed base decoded into `decoded`, its room charged
    /// first; every other key is read in place.
    pub(super) fn key_within<'k, B: std::ops::Deref<Target = Budget>>(
        &'k self,
        index: u64,
        decoded: &'k mut ChargedVec<u8, B>,
    ) -> Result<&'k [u8], BudgetExceeded> {
        if index >= self.base_len {
            return Ok(self.added_key(index));
        }
        let base = self.base.as_ref().expect("an index below the base");
        match &base.codec {
            None => Ok(base.key(index)),
            Some(codec) => {
                decoded.clear();
                codec.decompress_within(base.key(index), decoded)?;
                Ok(decoded)
            }
        }
    }

    /// The most bytes decoding entry `index` allocates: its text's, and for a key stored
    /// compressed the buffer it is decompressed into ([`Base::decode_bound`]).
    pub(super) fn decode_bound(&self, index: u64) -> usize {
        match &self.base {
            Some(base) if index < self.base_len => base.decode_bound(index),
            _ => self.key(index).len(),
        }
    }

    /// The index of `key`, whose hash is `hash`, if it is an entry.
    pub(super) fn find(&self, hash: u64, key: &[u8]) -> Option<u64> {
        if let Some(index) = self.base.as_ref().and_then(|base| base.find(hash, key)) {
            return Some(index);
        }
        self.segments
            .iter()
            .find_map(|segment| segment.find(hash, key))
            .or_else(|| self.heap.find(hash, key))
    }

    /// Appends `key` (not an entry yet), whose hash is `hash`, and returns its index.
    pub(super) fn push(&mut self, key: &[u8], hash: u64) -> u64 {
        self.heap.push(key, hash)
    }

    /// Takes a bulk load's numbered terms, ids `adopted.start..` (the next ones): the heap
    /// so far is frozen before them, a new one begins after. Returns their segment.
    pub(super) fn adopt(&mut self, adopted: Adopted) -> Arc<Segment> {
        assert_eq!(adopted.start, self.len(), "adopted terms take the next ids");
        let after = adopted.start + adopted.len();
        let heap = std::mem::replace(&mut self.heap, Heap::at(after));
        if heap.len() > 0 {
            self.starts.push(heap.start);
            self.segments.push(Arc::new(Segment::Heap(heap)));
        }
        self.starts.push(adopted.start);
        let segment = Arc::new(Segment::Adopted(adopted));
        self.segments.push(Arc::clone(&segment));
        segment
    }

    /// The heaps' and segments' key bytes and index bytes.
    pub(super) fn heap_bytes(&self) -> (u64, u64) {
        let mut total = self.heap.bytes();
        for segment in &self.segments {
            let (keys, index) = match &**segment {
                Segment::Heap(h) => h.bytes(),
                Segment::Adopted(a) => a.bytes(),
            };
            total.0 += keys;
            total.1 += index;
        }
        total
    }

    /// The bytes [`Self::view`] allocates: its lists of `Arc`s and the segments' first ids
    /// (cloned with their lengths as capacities), no key bytes.
    pub(super) fn view_bytes(&self) -> usize {
        self.segments.len() * (size_of::<Arc<Segment>>() + size_of::<u64>())
            + self.heap.blocks.len() * size_of::<Arc<Block>>()
            + self.heap.tail.len() * size_of::<Arc<[u8]>>()
    }

    /// The view of its keys now ([`View`]).
    pub(super) fn view(&self) -> View {
        View(Self {
            base: self.base.clone(),
            base_len: self.base_len,
            segments: self.segments.clone(),
            starts: self.starts.clone(),
            heap: self.heap.keys(),
            generation: self.generation,
        })
    }
}

/// The dictionary's keys `0..len` at one moment, held with no lock (the module docs). It reads
/// keys; it finds none (its heap has no table).
pub(super) struct View(Inner);

impl View {
    /// The pieces, for scans over them.
    pub(super) fn pieces(&self) -> &Inner {
        &self.0
    }

    pub(super) fn generation(&self) -> u64 {
        self.0.generation
    }
}

/// Heap entry `local` (counted from the end of the base).
pub(super) fn heap_key<'a>(bytes: &'a [u8], ends: &[u64], local: usize) -> &'a [u8] {
    let start = if local == 0 {
        0
    } else {
        ends[local - 1] as usize
    };
    &bytes[start..ends[local] as usize]
}
