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

#[cfg(test)]
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};

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
/// arena; here a prefix costs one read of each entry's key per 8 bytes, and a prefix a
/// whole run shares is skipped at once ([`common`]). A run of at most [`FEW`] entries is
/// finished by comparing the rest of its texts. The refinement loops along a run's texts
/// rather than nesting a level per word (which overflowed the stack at texts of tens of
/// kilobytes); runs split off are sorted in parallel, nested at most log2 of the entries
/// deep ([`refine`]). Transient memory: 16 bytes per entry (640 MB at DBpedia's 40 M terms),
/// freed when the order is built, and nothing per run; uncharged (a W1 site: the
/// checkpoint's and the lazy order's build).
pub(crate) fn sorted<'a, T>(
    range: std::ops::Range<u64>,
    key: &(dyn Fn(u64) -> Key<'a> + Sync),
) -> Vec<T>
where
    T: Copy + Ord + Send + Into<u64> + TryFrom<u64>,
{
    sorted_reading(range, key, &Words::default())
}

/// The words of keys the refinement reads, counted under test only: the guard of
/// [`refine`]'s bound. Product builds have nothing here and count nothing ([`count`]).
#[cfg(test)]
type Words = AtomicU64;
#[cfg(not(test))]
type Words = ();

/// Adds `$n` words to the `Words` `$read`: under test; compiled out of product builds.
macro_rules! count {
    ($read:expr, $n:expr) => {
        #[cfg(test)]
        $read.fetch_add($n as u64, Relaxed);
        #[cfg(not(test))]
        let _: &Words = $read;
    };
}

/// [`sorted`], the words of keys it reads added to `read`.
fn sorted_reading<'a, T>(
    range: std::ops::Range<u64>,
    key: &(dyn Fn(u64) -> Key<'a> + Sync),
    read: &Words,
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
    count!(read, entries.len());
    refine(&mut entries, 0, key, read);
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

/// The entries of a run that is finished by comparing its texts at any depth.
const FEW: usize = 16;
/// The entries from which a run's sort and its key reads go parallel.
const PARALLEL: usize = 1 << 14;

/// Sorts `run`, whose texts agree on their first `depth · 8` bytes and whose words are
/// those at `depth`, by the rest of their texts, then their index; adds the words of keys
/// it reads to `read`.
///
/// It loops along the run: the run sorted by its words, its runs of equal words whose
/// texts go on are each sorted by their next words. The largest, if it holds more than
/// half of the run, goes on in the loop; the others in parallel, each at most half of the
/// run it came from, so nested at most log2 of the entries deep however long the texts
/// (no depth limit and no stack per word). A run whose entries all share their word skips
/// what they share at once ([`common`]).
///
/// Work: an entry's key is read for a word only while it is still in a run with another
/// entry, so below its distinguishing prefix (the bytes up to where its text differs from
/// every other, or ends); [`common`] reads at most about three times the words it skips.
/// So O(Σ distinguishing prefixes) words of keys in all, read once each up to a constant,
/// also for runs whose shared prefix shrinks a little at every level (a staircase of
/// prefixes); plus each level's sort, O(m log m) comparisons of 16-byte entries for a run
/// of m (texts that end in the same word compared by their length, a key read each).
fn refine<'a>(
    mut run: &mut [Entry],
    mut depth: usize,
    key: &(dyn Fn(u64) -> Key<'a> + Sync),
    read: &Words,
) {
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
    // Equal words whose texts go on: a run sorted by its next words.
    let same = |a: &Entry, b: &Entry| a.word == b.word && a.more() && b.more();
    while run.len() > 1 {
        if run.len() > PARALLEL {
            run.par_sort_unstable_by(order);
        } else {
            run.sort_unstable_by(order);
        }
        if same(&run[0], &run[run.len() - 1]) {
            depth = common(run, depth, key, read);
            continue;
        }
        let largest = largest_run(run, same);
        let at = run.as_ptr() as usize;
        run.par_chunk_by_mut(same).for_each(|part| {
            let start = (part.as_ptr() as usize - at) / size_of::<Entry>();
            if part.len() > 1 && largest.is_none_or(|(first, _)| first != start) {
                if part.len() <= FEW {
                    finish(part, depth + 1, key);
                } else {
                    words_at(part, depth + 1, key, read);
                    refine(part, depth + 1, key, read);
                }
            }
        });
        let Some((start, len)) = largest else {
            return;
        };
        run = &mut std::mem::take(&mut run)[start..start + len];
        depth += 1;
        if run.len() <= FEW {
            finish(run, depth, key);
            return;
        }
        words_at(run, depth, key, read);
    }
}

/// The run of `run`'s entries that `same` joins which holds more than half of them, if
/// any: its start and length.
fn largest_run(run: &[Entry], same: impl Fn(&Entry, &Entry) -> bool) -> Option<(usize, usize)> {
    let mut start = 0;
    for end in 1..=run.len() {
        if end == run.len() || !same(&run[end - 1], &run[end]) {
            if 2 * (end - start) > run.len() {
                return Some((start, end - start));
            }
            start = end;
        }
    }
    None
}

/// Reads the word at `depth` of every entry of `run` (each key once); whether they are
/// one word and every text goes on.
fn words_at<'a>(
    run: &mut [Entry],
    depth: usize,
    key: &(dyn Fn(u64) -> Key<'a> + Sync),
    read: &Words,
) -> bool {
    let at = |entry: &mut Entry| {
        let k = key(u64::from(entry.index));
        *entry = Entry::at(&k[entry.skip()..], depth, entry.index, entry.skip() as u32);
    };
    let first = |run: &[Entry]| (run[0].word, run[0].more());
    if run.len() > PARALLEL {
        run.par_iter_mut().for_each(at);
        count!(read, run.len());
        let (word, more) = first(run);
        more && run.par_iter().all(|e| e.word == word && e.more())
    } else {
        run.iter_mut().for_each(at);
        count!(read, run.len());
        let (word, more) = first(run);
        more && run.iter().all(|e| e.word == word && e.more())
    }
}

/// Skips the prefix every text of `run` shares, past its first `(depth + 1) · 8` bytes
/// (its words, at `depth`, are one and its texts go on): returns the depth reached, the
/// entries' words read at it. The next two words are read one at a time (most shared
/// prefixes end in them); past them the texts are compared with the first entry's in
/// windows of 16, 32, 64, … bytes until one differs, then the words past the shared bytes
/// are read. The windows read at most about three times the bytes they skip, plus 16, per
/// entry: never what an entry shares with the first one beyond the run's shared prefix,
/// which a single pass to the first difference would read again at every level where the
/// run's shared prefix shrinks by a word.
fn common<'a>(
    run: &mut [Entry],
    mut depth: usize,
    key: &(dyn Fn(u64) -> Key<'a> + Sync),
    read: &Words,
) -> usize {
    for _ in 0..2 {
        depth += 1;
        if !words_at(run, depth, key, read) {
            return depth;
        }
    }
    let first = key(u64::from(run[0].index));
    let theirs = &first[run[0].skip()..];
    let mut known = (depth + 1) * 8;
    let mut window = 16;
    loop {
        let window_of_theirs = &theirs[known..theirs.len().min(known + window)];
        let shared = |entry: &Entry| {
            let k = key(u64::from(entry.index));
            let text = k.get(entry.skip() + known..).unwrap_or_default();
            let shared = text
                .iter()
                .zip(window_of_theirs)
                .take_while(|(a, b)| a == b)
                .count();
            // The words compared, the differing one included.
            count!(read, shared / 8 + 1);
            shared
        };
        let least = match run.len() > PARALLEL {
            true => run.par_iter().map(&shared).min(),
            false => run.iter().map(&shared).min(),
        }
        .unwrap_or(0);
        known += least;
        if least < window {
            break;
        }
        window *= 2;
    }
    words_at(run, known / 8, key, read);
    known / 8
}

/// Sorts `entries`, whose texts agree on their first `depth · 8` bytes and go on past them,
/// by the rest of their texts, then their index: the order [`refine`] gives (a text that
/// is a prefix of another comes first).
fn finish<'a>(entries: &mut [Entry], depth: usize, key: &(dyn Fn(u64) -> Key<'a> + Sync)) {
    fn rest<'k>(key: &'k [u8], entry: &Entry, depth: usize) -> &'k [u8] {
        key.get(entry.skip() + depth * 8..).unwrap_or_default()
    }
    entries.sort_unstable_by(|a, b| {
        let (ka, kb) = (key(u64::from(a.index)), key(u64::from(b.index)));
        rest(&ka, a, depth)
            .cmp(rest(&kb, b, depth))
            .then(a.index.cmp(&b.index))
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
    /// prefixes (past the windows a run's shared prefix is skipped by), ending on and
    /// around 8-byte boundaries, containing zero bytes, and equal texts under different
    /// tags; enough of them for the parallel branches.
    #[test]
    fn the_word_radix_order_is_the_text_order() {
        let mut seed = 0x2026_1003_u64;
        let mut next = |n: u64| {
            seed = seed
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            (seed >> 33) % n
        };
        let long = b"http://e/".repeat(32);
        let stems: [&[u8]; 5] = [
            b"http://yago-knowledge.org/resource/",
            b"http://e/",
            b"",
            b"ab\0",
            &long,
        ];
        let tags: [&[u8]; 4] = [b"I", b"S", b"Len\0", b"Lde\0"];
        let mut keys: Vec<Vec<u8>> = Vec::new();
        for _ in 0..40_000 {
            let mut key = tags[next(4) as usize].to_vec();
            key.extend_from_slice(stems[next(5) as usize]);
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

    /// A staircase of prefixes: texts leaving the run one by one, eight words apart, beside
    /// a block sharing 512 words. The order is the text order, and the words of keys read
    /// stay within a constant of the distinguishing prefixes, where reading to the first
    /// difference at every level reads the block's shared prefix again at each step.
    #[test]
    fn a_staircase_of_prefixes_reads_each_word_a_bounded_number_of_times() {
        let mut keys: Vec<Vec<u8>> = Vec::new();
        for i in 0..400 {
            keys.push(
                [
                    &b"S"[..],
                    &b"wwwwwwww".repeat(512),
                    format!("{i:08}").as_bytes(),
                ]
                .concat(),
            );
        }
        for k in 0..400 {
            keys.push([&b"S"[..], &b"wwwwwwww".repeat(8 * k + 8), b"xxxxxxxx"].concat());
        }
        let key = |i: u64| Cow::Borrowed(keys[i as usize].as_slice());
        let read = AtomicU64::new(0);
        let got: Vec<u64> = sorted_reading(0..keys.len() as u64, &key, &read);
        let read = read.into_inner();
        let text = |i: u64| text_of(&keys[i as usize]).unwrap();
        let mut expected: Vec<u64> = (0..keys.len() as u64).collect();
        expected.sort_by(|&a, &b| text(a).cmp(text(b)).then(a.cmp(&b)));
        assert_eq!(got, expected);
        // Each text's distinguishing prefix: its words up to where it differs from both
        // neighbours in the order.
        let shared = |a: u64, b: u64| {
            text(a)
                .iter()
                .zip(text(b))
                .take_while(|(x, y)| x == y)
                .count()
        };
        let distinguishing: usize = (0..got.len())
            .map(|p| {
                let before = p.checked_sub(1).map_or(0, |q| shared(got[q], got[p]));
                let after = got.get(p + 1).map_or(0, |&next| shared(got[p], next));
                before.max(after) / 8 + 1
            })
            .sum();
        assert!(
            read <= 4 * distinguishing as u64,
            "{read} words read, {distinguishing} distinguishing"
        );
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
