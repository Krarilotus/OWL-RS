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

use rayon::prelude::*;

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

/// The indices `range` with a text, sorted by it (ties by index). Parallel, and in place:
/// the indices alone (4 or 8 bytes each) are sorted, each comparison reading the texts,
/// rather than (text, index) pairs of 24 bytes (2 GB more at DBpedia's 40 M terms).
pub(crate) fn sorted<'a, T>(
    range: std::ops::Range<u64>,
    key: &(dyn Fn(u64) -> &'a [u8] + Sync),
) -> Vec<T>
where
    T: Copy + Ord + Send + Into<u64> + TryFrom<u64>,
{
    let mut entries: Vec<T> = range
        .into_par_iter()
        .filter(|&index| text_of(key(index)).is_some())
        .map(|index| {
            T::try_from(index)
                .ok()
                .expect("an index of the order's width")
        })
        .collect();
    let text = |index: T| text_of(key(index.into())).unwrap_or_default();
    entries.par_sort_unstable_by(|&a, &b| text(a).cmp(text(b)).then(a.cmp(&b)));
    entries
}

/// The positions `start..end` of `order` whose text starts with `prefix`.
pub(crate) fn prefix_range<'a>(
    order: &[impl Copy + Into<u64>],
    key: &dyn Fn(u64) -> &'a [u8],
    prefix: &[u8],
) -> (usize, usize) {
    let text = |position: usize| text_of(key(order[position].into())).unwrap_or_default();
    let start = partition(order.len(), |p| text(p) < prefix);
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
        key: &(dyn Fn(u64) -> &'a [u8] + Sync),
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
        let text = |index: u64| text_of(key(index)).unwrap_or_default();
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
        let key = |i: u64| keys[i as usize].as_slice();
        let all: Vec<u64> = sorted(0..keys.len() as u64, &key);
        let texts: Vec<&[u8]> = all.iter().map(|&i| text_of(key(i)).unwrap()).collect();
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
        let found: Vec<&[u8]> = all[start..end].iter().map(|&i| key(i)).collect();
        assert_eq!(found, [&b"Lde\0wind"[..], b"Swind", b"Den\0rtl\0windsurf"]);
        assert_eq!(prefix_range(&all, &key, b"zzz"), (all.len(), all.len()));
        // Built in two steps, merged: the same order.
        let mut order = TextOrder::default();
        order.extend(0, 4, &key);
        order.extend(0, keys.len() as u64, &key);
        assert_eq!(order.order, all);
    }
}
