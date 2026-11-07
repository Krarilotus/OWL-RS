//! The dictionary's storage: the entries interned one by one ([`Heap`]), the segments
//! between the mapped base and the heap ([`Segment`]), and the three together ([`Inner`]),
//! which [`super::dictionary::Dictionary`] keeps behind its lock.

use hashbrown::HashTable;
use nrese_exec::{Budget, BudgetExceeded, ChargedVec};

use super::dictionary::Base;
use super::order::Key;
use super::pending::Adopted;

/// Entries kept on the heap, one after another, from id `start` on.
#[derive(Default)]
pub(super) struct Heap {
    pub(super) start: u64,
    /// The keys, one after another.
    pub(super) bytes: Vec<u8>,
    /// Their end offsets in `bytes`.
    pub(super) ends: Vec<u64>,
    /// Their [`key_hash`]es: the table grows by moving numbers, without reading and
    /// hashing every key again under the write lock (a bulk load stalled on it).
    pub(super) hashes: Vec<u64>,
    /// Their ids, by [`key_hash`].
    pub(super) table: HashTable<u64>,
}

impl Heap {
    pub(super) fn at(start: u64) -> Self {
        Self {
            start,
            ..Self::default()
        }
    }

    pub(super) fn len(&self) -> u64 {
        self.ends.len() as u64
    }

    pub(super) fn key(&self, id: u64) -> &[u8] {
        heap_key(&self.bytes, &self.ends, (id - self.start) as usize)
    }

    pub(super) fn find(&self, hash: u64, key: &[u8]) -> Option<u64> {
        self.table.find(hash, |&id| self.key(id) == key).copied()
    }

    /// Appends `key` (not an entry yet), whose hash is `hash`, and returns its id.
    pub(super) fn push(&mut self, key: &[u8], hash: u64) -> u64 {
        let id = self.start + self.len();
        self.bytes.extend_from_slice(key);
        self.ends.push(self.bytes.len() as u64);
        self.hashes.push(hash);
        let Heap {
            table,
            hashes,
            start,
            ..
        } = self;
        table.insert_unique(hash, id, |&i| hashes[(i - *start) as usize]);
        id
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
    pub(super) fn start(&self) -> u64 {
        match self {
            Self::Heap(heap) => heap.start,
            Self::Adopted(adopted) => adopted.start,
        }
    }

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
    pub(super) base: Option<Base>,
    /// Entries after the base, before the heap, by id.
    pub(super) segments: Vec<Segment>,
    /// The entries interned one by one since, at the end.
    pub(super) heap: Heap,
}

impl Inner {
    pub(super) fn with_base(base: Base) -> Self {
        Self {
            heap: Heap::at(base.len),
            base: Some(base),
            segments: Vec::new(),
        }
    }

    pub(super) fn base_len(&self) -> u64 {
        self.base.as_ref().map_or(0, |base| base.len)
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
        let at = self.segments.partition_point(|s| s.start() <= index) - 1;
        self.segments[at].key(index)
    }

    /// [`Self::key`], a key of a compressed base decoded into `decoded`, its room charged
    /// first; every other key is read in place.
    pub(super) fn key_within<'k, B: std::ops::Deref<Target = Budget>>(
        &'k self,
        index: u64,
        decoded: &'k mut ChargedVec<u8, B>,
    ) -> Result<&'k [u8], BudgetExceeded> {
        let Some(base) = self.base.as_ref().filter(|base| index < base.len) else {
            return Ok(self.added_key(index));
        };
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
            Some(base) if index < base.len => base.decode_bound(index),
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
    /// so far is frozen before them, a new one begins after.
    pub(super) fn adopt(&mut self, adopted: Adopted) {
        assert_eq!(adopted.start, self.len(), "adopted terms take the next ids");
        let after = adopted.start + adopted.len();
        let heap = std::mem::replace(&mut self.heap, Heap::at(after));
        if heap.len() > 0 {
            self.segments.push(Segment::Heap(heap));
        }
        self.segments.push(Segment::Adopted(adopted));
    }

    /// The heaps' and segments' key bytes and index bytes.
    pub(super) fn heap_bytes(&self) -> (u64, u64) {
        let heap = |h: &Heap| {
            (
                h.bytes.len() as u64,
                (h.ends.capacity() * 16 + h.table.capacity() * 9) as u64,
            )
        };
        let mut total = heap(&self.heap);
        for segment in &self.segments {
            let (keys, index) = match segment {
                Segment::Heap(h) => heap(h),
                Segment::Adopted(a) => a.bytes(),
            };
            total.0 += keys;
            total.1 += index;
        }
        total
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
