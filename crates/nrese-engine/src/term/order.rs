//! The dictionary's terms in the order of their text: prefix searches by binary search.
//!
//! `STRSTARTS(?label, "wind")` asks for the terms whose text starts with `wind`. Tested per
//! row, or by one pass over the whole dictionary ([`super::strings`]), that costs the
//! rows or the dictionary's size. With the entries sorted by their text (the lexical
//! form of a literal, the IRI string), the terms with a prefix are one contiguous range of
//! the order, found by two binary searches: what QLever's sorted vocabulary gives it.
//!
//! The order lists dictionary indices (u32: a checkpoint holds fewer than 2^32 entries),
//! sorted by text (bytes, which for UTF-8 is code point order, SPARQL's string order),
//! ties by index. Blank nodes and triple terms have no text and aren't listed.
//!
//! A checkpoint stores the order of its entries, mapped with the rest
//! ([`super::dictionary::Base`]); the entries interned since are sorted in memory
//! ([`TextOrder`]), lazily, when a search needs them, and merged as the dictionary grows.

use std::borrow::Cow;

use rayon::prelude::*;

/// A key as the dictionary hands it out: in place, or decoded ([`super::vocabulary`]).
pub(crate) type Key<'a> = Cow<'a, [u8]>;

/// The text of `key`, empty if it has none; borrowed where the key is.
pub(crate) fn text_in(key: Key<'_>) -> Key<'_> {
    match key {
        Cow::Borrowed(key) => Cow::Borrowed(text_of(key).unwrap_or_default()),
        Cow::Owned(key) => Cow::Owned(text_of(&key).unwrap_or_default().to_vec()),
    }
}

/// The text of a dictionary key, if it has one: an IRI's string or a literal's lexical
/// form (after its language, direction or datatype).
pub(crate) fn text_of(key: &[u8]) -> Option<&[u8]> {
    let after = |n: usize| -> Option<&[u8]> {
        let mut at = 1;
        for _ in 0..n {
            at += memchr::memchr(0, key.get(at..)?)? + 1;
        }
        key.get(at..)
    };
    match key.first()? {
        b'I' | b'S' => key.get(1..),
        b'L' | b'T' => after(1),
        b'D' => after(2),
        _ => None,
    }
}

/// The indices `range` with a text, sorted by it (ties by index). Parallel, as a
/// most-significant-word radix sort: each entry is (the 8 bytes of its text at the current
/// depth as a big-endian number, its index, where its text starts in its key), 16 bytes;
/// the entries are sorted by that word, and each run of equal words whose texts go on is
/// sorted again by the next 8 bytes, until the runs are single entries or their texts end.
/// RDF texts share long prefixes (`http://yago-knowledge.org/resource/`): a comparison
/// sort met them in every one of its n·log n comparisons, reading both keys from the
/// arena; here a prefix costs one read of each entry's key per 8 bytes. Transient memory:
/// 16 bytes per entry (640 MB at DBpedia's 40 M terms), freed when the order is built.
pub(crate) fn sorted<'a, T>(
    range: std::ops::Range<u64>,
    key: &(dyn Fn(u64) -> Key<'a> + Sync),
) -> Vec<T>
where
    T: Copy + Ord + Send + Into<u64> + TryFrom<u64>,
{
    if range.end > u64::from(u32::MAX) {
        // Beyond what a checkpoint holds: by the texts directly.
        let mut entries: Vec<T> = range
            .into_par_iter()
            .filter(|&index| text_of(&key(index)).is_some())
            .map(|index| {
                T::try_from(index)
                    .ok()
                    .expect("an index of the order's width")
            })
            .collect();
        let text = |index: T| text_in(key(index.into()));
        entries.par_sort_unstable_by(|&a, &b| text(a).cmp(&text(b)).then(a.cmp(&b)));
        return entries;
    }
    let mut entries: Vec<Entry> = range
        .into_par_iter()
        .filter_map(|index| {
            let key = key(index);
            let text = text_of(&key)?;
            let skip = (text.as_ptr() as usize - key.as_ptr() as usize) as u32;
            Some(Entry::at(text, 0, index as u32, skip))
        })
        .collect();
    refine(&mut entries, 0, key);
    entries
        .into_iter()
        .map(|entry| {
            T::try_from(u64::from(entry.index))
                .ok()
                .expect("an index of the order's width")
        })
        .collect()
}

/// An entry of [`sorted`] at some depth of its text.
#[derive(Clone, Copy)]
struct Entry {
    /// The text's 8 bytes at the current depth, big-endian, zero-padded past its end.
    word: u64,
    index: u32,
    /// Where the text starts in the key; the top bit: the text goes on past this word.
    skip: u32,
}

const MORE: u32 = 1 << 31;

impl Entry {
    fn at(text: &[u8], depth: usize, index: u32, skip: u32) -> Self {
        let from = (depth * 8).min(text.len());
        let part = &text[from..text.len().min(from + 8)];
        let mut head = [0u8; 8];
        head[..part.len()].copy_from_slice(part);
        let more = if text.len() > from + 8 { MORE } else { 0 };
        Entry {
            word: u64::from_be_bytes(head),
            index,
            skip: (skip & !MORE) | more,
        }
    }

    fn more(&self) -> bool {
        self.skip & MORE != 0
    }

    fn skip(&self) -> usize {
        (self.skip & !MORE) as usize
    }
}

/// Sorts `entries`, whose texts agree on their first `depth · 8` bytes and whose words are
/// those at `depth`.
fn refine<'a>(entries: &mut [Entry], depth: usize, key: &(dyn Fn(u64) -> Key<'a> + Sync)) {
    // Texts that end within this word come before those that go on (zero padding sorts
    // first); among equal words that both end, the shorter text first, then the index.
    let order = |a: &Entry, b: &Entry| {
        a.word
            .cmp(&b.word)
            .then(a.more().cmp(&b.more()))
            .then_with(|| {
                if a.more() {
                    return std::cmp::Ordering::Equal;
                }
                let length = |e: &Entry| key(u64::from(e.index)).len() - e.skip();
                length(a).cmp(&length(b)).then(a.index.cmp(&b.index))
            })
    };
    if entries.len() > 1 << 14 {
        entries.par_sort_unstable_by(order);
    } else {
        entries.sort_unstable_by(order);
    }
    // Runs of equal words whose texts go on: by their next 8 bytes.
    let runs: Vec<&mut [Entry]> = entries
        .chunk_by_mut(|a, b| a.word == b.word && a.more() && b.more())
        .filter(|run| run.len() > 1 && run[0].more())
        .collect();
    runs.into_par_iter().for_each(|run| {
        for entry in run.iter_mut() {
            let k = key(u64::from(entry.index));
            *entry = Entry::at(
                &k[entry.skip()..],
                depth + 1,
                entry.index,
                entry.skip() as u32,
            );
        }
        refine(run, depth + 1, key);
    });
}

/// The positions `start..end` of `order` whose text starts with `prefix`.
pub(crate) fn prefix_range<'a>(
    order: &[impl Copy + Into<u64>],
    key: &dyn Fn(u64) -> Key<'a>,
    prefix: &[u8],
) -> (usize, usize) {
    let text = |position: usize| text_in(key(order[position].into()));
    let start = partition(order.len(), |p| *text(p) < *prefix);
    let end = start + partition(order.len() - start, |p| text(start + p).starts_with(prefix));
    (start, end)
}

/// The first position in `0..len` for which `below` is false (`below` holds for a prefix).
fn partition(len: usize, below: impl Fn(usize) -> bool) -> usize {
    let (mut low, mut high) = (0, len);
    while low < high {
        let mid = low + (high - low) / 2;
        if below(mid) {
            low = mid + 1;
        } else {
            high = mid;
        }
    }
    low
}

/// The order of the entries after the mapped base (or of all, without one): built when a
/// search first needs it, extended by merging when the dictionary has grown.
#[derive(Debug, Default)]
pub(crate) struct TextOrder {
    /// The entries `first..covered` are in `order`.
    pub(crate) first: u64,
    pub(crate) covered: u64,
    pub(crate) order: Vec<u64>,
}

impl TextOrder {
    /// Takes in the entries `covered..len` (all from `first` on if `first` changed).
    pub(crate) fn extend<'a>(
        &mut self,
        first: u64,
        len: u64,
        key: &(dyn Fn(u64) -> Key<'a> + Sync),
    ) {
        if first != self.first {
            *self = Self {
                first,
                covered: first,
                order: Vec::new(),
            };
        }
        if self.covered >= len {
            return;
        }
        let added: Vec<u64> = sorted(self.covered..len, key);
        let text = |index: u64| text_in(key(index));
        let old = std::mem::take(&mut self.order);
        let mut merged = Vec::with_capacity(old.len() + added.len());
        let (mut i, mut j) = (0, 0);
        while i < old.len() && j < added.len() {
            // Equal texts: the older (lower) index first, as a full sort would put them.
            if (text(old[i]), old[i]) <= (text(added[j]), added[j]) {
                merged.push(old[i]);
                i += 1;
            } else {
                merged.push(added[j]);
                j += 1;
            }
        }
        merged.extend_from_slice(&old[i..]);
        merged.extend_from_slice(&added[j..]);
        self.order = merged;
        self.covered = len;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The radix refinement against a plain sort by (text, index): texts sharing long
    /// prefixes, ending on and around 8-byte boundaries, containing zero bytes, and equal
    /// texts under different tags; enough of them for the parallel branches.
    #[test]
    fn the_word_radix_order_is_the_text_order() {
        let mut seed = 0x2026_1003_u64;
        let mut next = |n: u64| {
            seed = seed
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            (seed >> 33) % n
        };
        let stems: [&[u8]; 4] = [
            b"http://yago-knowledge.org/resource/",
            b"http://e/",
            b"",
            b"ab\0",
        ];
        let tags: [&[u8]; 4] = [b"I", b"S", b"Len\0", b"Lde\0"];
        let mut keys: Vec<Vec<u8>> = Vec::new();
        for _ in 0..40_000 {
            let mut key = tags[next(4) as usize].to_vec();
            key.extend_from_slice(stems[next(4) as usize]);
            for _ in 0..next(20) {
                key.push(b"ab\0z"[next(4) as usize]);
            }
            keys.push(key);
        }
        keys.push(b"Bblank".to_vec());
        let key = |i: u64| Cow::Borrowed(keys[i as usize].as_slice());
        let got: Vec<u64> = sorted(0..keys.len() as u64, &key);
        let mut expected: Vec<u64> = (0..keys.len() as u64)
            .filter(|&i| text_of(&keys[i as usize]).is_some())
            .collect();
        expected.sort_by(|&a, &b| {
            text_of(&keys[a as usize])
                .cmp(&text_of(&keys[b as usize]))
                .then(a.cmp(&b))
        });
        assert_eq!(got, expected);
    }

    #[test]
    fn texts_sort_and_prefixes_are_ranges() {
        let keys: Vec<Vec<u8>> = [
            &b"Sbanana"[..],
            b"Lde\0wind",
            b"Ihttp://e/wind",
            b"Bblank",
            b"Sapple",
            b"Den\0rtl\0windsurf",
            b"Thttp://www.w3.org/2001/XMLSchema#integer\x0042",
            b"Swin",
            b"Swind",
        ]
        .iter()
        .map(|k| k.to_vec())
        .collect();
        let plain = |i: u64| keys[i as usize].as_slice();
        // Half the keys as if decoded.
        let key = |i: u64| match i % 2 {
            0 => Cow::Borrowed(plain(i)),
            _ => Cow::Owned(plain(i).to_vec()),
        };
        let all: Vec<u64> = sorted(0..keys.len() as u64, &key);
        let texts: Vec<&[u8]> = all.iter().map(|&i| text_of(plain(i)).unwrap()).collect();
        assert_eq!(
            texts,
            [
                &b"42"[..],
                b"apple",
                b"banana",
                b"http://e/wind",
                b"win",
                b"wind",
                b"wind",
                b"windsurf"
            ]
        );
        let (start, end) = prefix_range(&all, &key, b"wind");
        let found: Vec<&[u8]> = all[start..end].iter().map(|&i| plain(i)).collect();
        assert_eq!(found, [&b"Lde\0wind"[..], b"Swind", b"Den\0rtl\0windsurf"]);
        assert_eq!(prefix_range(&all, &key, b"zzz"), (all.len(), all.len()));
        // Built in two steps, merged: the same order.
        let mut order = TextOrder::default();
        order.extend(0, 4, &key);
        order.extend(0, keys.len() as u64, &key);
        assert_eq!(order.order, all);
    }
}
