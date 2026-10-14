//! Permutations merged from sorted runs: the inferred stack built from a rule engine's
//! working set as it holds it (per predicate, its pairs sorted by subject and, where it
//! keeps them so, by object), without a list of every statement and without sorting all
//! of them.
//!
//! Every permutation of the default-graph layout is a merge of such runs. PSOG and POSG
//! are each predicate's runs merged by (subject, object) or (object, subject), one
//! predicate after another; SPOG and OSPG are every predicate's range of the packed PSOG
//! and POSG merged by subject or object first. So each statement is read from the runs
//! twice and from the packed keys twice, and no sort runs but one by object for a
//! relation kept by subject alone.
//!
//! A merge runs in parts of about [`PART`] keys, cut at sampled keys, so each part takes a
//! range of every run (a binary search per run) and is merged on one thread. The parts are
//! packed as they come, a wave of them at a time ([`PackedBuilder`]): the keys held
//! unpacked are a wave's (the engine's configured target), never a permutation's.

use std::collections::BinaryHeap;
use std::ops::Range;

use std::sync::Arc;

use rayon::prelude::*;

use super::keys::{BLOCK, IndexEncoding, PackedBuilder, PackedKeys};
use crate::quad::{EncodedQuad, Key, Permutation};
use crate::term::{TermId, TermKind};

/// A run of `(a, b)` pairs, sorted and distinct, in chunks: each chunk's pairs come after
/// the previous chunk's.
#[derive(Debug, Clone)]
pub enum PairChunks {
    /// Pairs of ids.
    Ids(Vec<Vec<[u64; 2]>>),
    /// Pairs of numbers, each pair a word, `a`'s number in the high half; `ids[n]` is the
    /// id numbered `n`, ascending in `n` (a rule engine's working set numbers its ids
    /// densely, so that a pair is one word): sorted by number, so by id.
    Numbers {
        chunks: Vec<Vec<u64>>,
        ids: Arc<[u64]>,
    },
}

impl Default for PairChunks {
    fn default() -> Self {
        Self::Ids(Vec::new())
    }
}

impl PairChunks {
    /// Its pairs.
    pub fn len(&self) -> usize {
        match self {
            Self::Ids(chunks) => chunks.iter().map(Vec::len).sum(),
            Self::Numbers { chunks, .. } => chunks.iter().map(Vec::len).sum(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Its pairs `keep` accepts (as ids), in the same form.
    pub(crate) fn retained(&self, mut keep: impl FnMut([u64; 2]) -> bool) -> Self {
        match self {
            Self::Ids(chunks) => Self::Ids(
                chunks
                    .iter()
                    .map(|chunk| {
                        chunk
                            .iter()
                            .copied()
                            .filter(|&pair| keep(pair))
                            .collect::<Vec<_>>()
                    })
                    .filter(|chunk| !chunk.is_empty())
                    .collect(),
            ),
            Self::Numbers { chunks, ids } => Self::Numbers {
                chunks: chunks
                    .iter()
                    .map(|chunk| {
                        chunk
                            .iter()
                            .copied()
                            .filter(|&n| keep(numbered(ids, n)))
                            .collect::<Vec<_>>()
                    })
                    .filter(|chunk| !chunk.is_empty())
                    .collect(),
                ids: Arc::clone(ids),
            },
        }
    }

    /// Sorted pairs whose first original ID is at least `low`, without visiting the
    /// prefix. The physical layout is selected once for this iterator.
    pub fn with_first_from(&self, low: u64) -> Box<dyn Iterator<Item = [u64; 2]> + '_> {
        fn suffix<'a, P: Copy + 'a>(
            chunks: &'a [Vec<P>],
            low: u64,
            first: impl Fn(P) -> u64 + Copy + 'a,
        ) -> impl Iterator<Item = P> + 'a {
            let start =
                chunks.partition_point(|chunk| chunk.last().is_some_and(|&p| first(p) < low));
            chunks[start..].iter().flat_map(move |chunk| {
                let at = chunk.partition_point(|&p| first(p) < low);
                chunk[at..].iter().copied()
            })
        }
        match self {
            Self::Ids(chunks) => Box::new(suffix(chunks, low, |p| p[0])),
            Self::Numbers { chunks, ids } => {
                let rank = ids.partition_point(|&id| id < low) as u64;
                Box::new(suffix(chunks, rank, |p| p >> 32).map(|p| numbered(ids, p)))
            }
        }
    }

    /// Its pairs as ids, in order.
    pub fn pairs(&self) -> Box<dyn Iterator<Item = [u64; 2]> + '_> {
        match self {
            Self::Ids(chunks) => Box::new(chunks.iter().flatten().copied()),
            Self::Numbers { chunks, ids } => {
                Box::new(chunks.iter().flatten().map(|&n| numbered(ids, n)))
            }
        }
    }
}

/// The pair of ids the numbers packed in `n` stand for.
#[inline]
fn numbered(ids: &[u64], n: u64) -> [u64; 2] {
    [ids[(n >> 32) as usize], ids[(n & 0xFFFF_FFFF) as usize]]
}

/// The statements of one predicate, as a rule engine's working set holds them: runs of
/// `(subject, object)` pairs, and the same pairs as `(object, subject)` runs where it
/// keeps them. Runs of one predicate may share pairs.
#[derive(Debug, Default, Clone)]
pub struct PredicateRuns {
    pub predicate: TermId,
    pub by_subject: Vec<PairChunks>,
    /// The same pairs by object; `None` if the producer doesn't hold them so.
    pub by_object: Option<Vec<PairChunks>>,
}

impl PredicateRuns {
    /// The pairs of its runs by subject (shared pairs counted once per run).
    pub fn len(&self) -> usize {
        self.by_subject.iter().map(PairChunks::len).sum()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// The pairs of its runs by subject a stack can hold: none without an IRI predicate,
    /// else those with an IRI or blank node subject (a range of each run).
    pub fn storable_len(&self) -> usize {
        if self.predicate.kind() != TermKind::Iri {
            return 0;
        }
        let p = self.predicate.raw();
        self.by_subject
            .iter()
            .map(|run| {
                PairRun::new(run, p, false, Permutation::Psog)
                    .within(SUBJECTS.start, SUBJECTS.end)
                    .len()
            })
            .sum()
    }

    /// The relations of `triples` (`[subject, predicate, object]`, any order, duplicates
    /// allowed), each predicate once and in order, a run each.
    pub fn of_triples(triples: Vec<[u64; 3]>) -> Vec<Self> {
        let mut keys: Vec<[u64; 3]> = triples.into_iter().map(|[s, p, o]| [p, s, o]).collect();
        let kept = nrese_exec::sort::sort_dedup_keys(&mut keys);
        keys.truncate(kept);
        keys.chunk_by(|a, b| a[0] == b[0])
            .map(|group| Self {
                predicate: TermId::from_raw(group[0][0]),
                by_subject: vec![PairChunks::Ids(vec![
                    group.iter().map(|&[_, s, o]| [s, o]).collect(),
                ])],
                by_object: None,
            })
            .collect()
    }
}

/// Keys of a sorted sequence, read by position: permutation keys, or a run's pairs of
/// numbers (one word each).
pub(crate) trait SortedKeys<T = Key>: Sync {
    fn len(&self) -> usize;
    /// The key at `i`.
    fn key(&self, i: usize) -> T;
    /// Appends the keys at `range` to `out`, in order.
    fn read(&self, range: Range<usize>, out: &mut Vec<T>);
}

/// What a merge orders: permutation keys, or pairs of numbers.
pub(crate) trait Merged: Copy + Ord + Default + Send + Sync {}

impl<T: Copy + Ord + Default + Send + Sync> Merged for T {}

/// Keys per part of a merge, about: a part is merged on one thread. Small in unit tests,
/// so their merges run in many parts and waves.
const PART: usize = if cfg!(test) { 40 } else { 1 << 18 };

// The wave targets passed to this thread's merges (configuration-isolation guard).
#[cfg(test)]
thread_local! {
    pub(crate) static MERGE_WAVES: std::cell::RefCell<Vec<usize>> = const {
        std::cell::RefCell::new(Vec::new())
    };
}

/// Keys read from a run at a time while merging.
const READ: usize = 512;

/// Sampled keys per part, for the cuts between parts.
const SAMPLES: usize = 16;

/// Adds the union of `runs` (each sorted and distinct) to `out`, in order: the merge in
/// parts, a wave at a time. Keys in more than one run are added once. O(n log k) for n
/// keys of k runs, on every core.
pub(crate) fn merge_into(runs: &[&dyn SortedKeys], out: &mut PackedBuilder, wave: usize) {
    merge_with(runs, wave, &mut |keys: &[Key]| out.extend(keys));
}

/// The union of `runs` (each sorted and distinct), in order, handed to `emit` a wave at
/// a time ([`merge_into`]).
pub(crate) fn merge_with<T: Merged>(
    runs: &[&dyn SortedKeys<T>],
    wave: usize,
    emit: &mut dyn FnMut(&[T]),
) {
    merge_in_waves(runs, wave, emit);
}

/// [`merge_with`] holding at most `wave` keys unpacked at once (a part larger than it
/// alone).
fn merge_in_waves<T: Merged>(runs: &[&dyn SortedKeys<T>], wave: usize, emit: &mut dyn FnMut(&[T])) {
    #[cfg(test)]
    MERGE_WAVES.with(|waves| waves.borrow_mut().push(wave));
    let runs: Vec<&dyn SortedKeys<T>> = runs.iter().copied().filter(|run| run.len() > 0).collect();
    let total: usize = runs.iter().map(|run| run.len()).sum();
    if total == 0 {
        return;
    }
    let bounds = bounds(&runs, total.div_ceil(PART));
    // `cuts[r][i]`: where part `i` starts in run `r`; the run's length last. A key equal
    // to a bound goes to the part after it, so a part's keys all precede the next part's.
    let cuts: Vec<Vec<usize>> = runs
        .par_iter()
        .map(|run| {
            let mut cut = Vec::with_capacity(bounds.len() + 2);
            cut.push(0);
            let mut from = 0;
            for bound in &bounds {
                from = partition_point(*run, from, bound);
                cut.push(from);
            }
            cut.push(run.len());
            cut
        })
        .collect();
    let parts = bounds.len() + 1;
    let size = |part: usize| {
        cuts.iter()
            .map(|cut| cut[part + 1] - cut[part])
            .sum::<usize>()
    };
    let mut first = 0;
    while first < parts {
        let (mut end, mut held) = (first, 0usize);
        while end < parts && (end == first || wave == 0 || held.saturating_add(size(end)) <= wave) {
            held += size(end);
            end += 1;
        }
        // Each part into a list of its own, handed over in order: one buffer for the wave
        // was zeroed and its parts' keys moved together on the calling thread alone, while
        // the other cores waited (CSPA httpd's inferred stack: 9.6 s, 25-34 ns of thread
        // time a key, where packing and decoding take about 4; 14 October 2026).
        let merged: Vec<Vec<T>> = (first..end)
            .into_par_iter()
            .map(|part| merge_part(&runs, &cuts, part, size(part)))
            .collect();
        for keys in &merged {
            if !keys.is_empty() {
                emit(keys);
            }
        }
        first = end;
    }
}

/// The union of `runs`, packed ([`merge_into`]).
#[cfg(test)]
pub(crate) fn merge_packed(
    runs: &[&dyn SortedKeys],
    encoding: IndexEncoding,
    wave: usize,
) -> PackedKeys {
    let mut out = PackedBuilder::new(encoding);
    merge_into(runs, &mut out, wave);
    out.finish()
}

/// Keys that cut the keys of `runs` into about `parts` parts of equal size: about
/// [`SAMPLES`] of each part's keys sampled (at least one of every run), sorted, and the
/// sample cut evenly.
fn bounds<T: Merged>(runs: &[&dyn SortedKeys<T>], parts: usize) -> Vec<T> {
    if parts <= 1 {
        return Vec::new();
    }
    let total: usize = runs.iter().map(|run| run.len()).sum();
    let step = (total / (parts * SAMPLES)).max(1);
    let mut sample: Vec<T> = runs
        .par_iter()
        .flat_map_iter(|run| {
            let first = (step / 2).min(run.len() / 2);
            (first..run.len()).step_by(step).map(|i| run.key(i))
        })
        .collect();
    sample.par_sort_unstable();
    let mut bounds: Vec<T> = (1..parts)
        .map(|i| sample[i * sample.len() / parts])
        .collect();
    bounds.dedup();
    bounds
}

/// The first position at or after `from` whose key isn't below `bound`. O(log n).
fn partition_point<T: Merged>(run: &dyn SortedKeys<T>, from: usize, bound: &T) -> usize {
    let (mut low, mut high) = (from, run.len());
    while low < high {
        let mid = low + (high - low) / 2;
        if run.key(mid) < *bound {
            low = mid + 1;
        } else {
            high = mid;
        }
    }
    low
}

/// A run's keys within a part, read [`READ`] at a time.
struct Cursor<'a, T> {
    run: &'a dyn SortedKeys<T>,
    next: usize,
    end: usize,
    keys: Vec<T>,
    at: usize,
}

impl<T: Merged> Cursor<'_, T> {
    /// The current key, reading the next ones where they are used up; `None` at the end.
    #[inline]
    fn peek(&mut self) -> Option<T> {
        if self.at == self.keys.len() {
            if self.next == self.end {
                return None;
            }
            let to = (self.next + READ).min(self.end);
            self.keys.clear();
            self.run.read(self.next..to, &mut self.keys);
            self.next = to;
            self.at = 0;
        }
        Some(self.keys[self.at])
    }
}

/// Runs a part merges by a scan for the least key, at most; more go through a heap.
const SCANNED: usize = 4;

/// The keys of part `part` of `runs` (`size` of them, fewer where runs share keys), each
/// once, in order. Two runs merge by one comparison a key, up to [`SCANNED`]
/// by a scan of their current keys, and more through a heap: a heap's pop and push moved
/// 40-byte entries for each of CSPA httpd's subset's inferred statements in each
/// permutation, its merges then about as long as the closure (13 October 2026).
fn merge_part<T: Merged>(
    runs: &[&dyn SortedKeys<T>],
    cuts: &[Vec<usize>],
    part: usize,
    size: usize,
) -> Vec<T> {
    let mut out: Vec<T> = Vec::with_capacity(size);
    let mut cursors: Vec<Cursor<'_, T>> = runs
        .iter()
        .zip(cuts)
        .filter(|(_, cut)| cut[part] < cut[part + 1])
        .map(|(&run, cut)| Cursor {
            run,
            next: cut[part],
            end: cut[part + 1],
            keys: Vec::with_capacity(READ),
            at: 0,
        })
        .collect();
    let mut put = |key: T| {
        if out.last() != Some(&key) {
            out.push(key);
        }
    };
    match cursors.as_mut_slice() {
        [] => {}
        [cursor] => {
            while let Some(key) = cursor.peek() {
                put(key);
                cursor.at += 1;
            }
        }
        [a, b] => loop {
            match (a.peek(), b.peek()) {
                (Some(x), Some(y)) => {
                    if x <= y {
                        put(x);
                        a.at += 1;
                    } else {
                        put(y);
                        b.at += 1;
                    }
                }
                (Some(x), None) => {
                    put(x);
                    a.at += 1;
                }
                (None, Some(y)) => {
                    put(y);
                    b.at += 1;
                }
                (None, None) => break,
            }
        },
        cursors if cursors.len() <= SCANNED => loop {
            let mut least: Option<(T, usize)> = None;
            for (i, cursor) in cursors.iter_mut().enumerate() {
                if let Some(key) = cursor.peek()
                    && least.is_none_or(|(low, _)| key < low)
                {
                    least = Some((key, i));
                }
            }
            let Some((key, i)) = least else {
                break;
            };
            put(key);
            cursors[i].at += 1;
        },
        cursors => {
            let mut heap: BinaryHeap<std::cmp::Reverse<(T, usize)>> = cursors
                .iter_mut()
                .enumerate()
                .filter_map(|(i, cursor)| cursor.peek().map(|key| std::cmp::Reverse((key, i))))
                .collect();
            while let Some(std::cmp::Reverse((key, i))) = heap.pop() {
                put(key);
                let cursor = &mut cursors[i];
                cursor.at += 1;
                if let Some(next) = cursor.peek() {
                    heap.push(std::cmp::Reverse((next, i)));
                }
            }
        }
    }
    out
}

/// The default graph's id, the graph of every key a pair run makes.
const DEFAULT_GRAPH: u64 = TermId::DEFAULT_GRAPH.raw();

/// The chunks of a [`PairChunks`], borrowed.
#[derive(Clone, Copy)]
enum Chunks<'a> {
    Ids(&'a [Vec<[u64; 2]>]),
    Numbers(&'a [Vec<u64>], &'a Arc<[u64]>),
}

impl<'a> Chunks<'a> {
    fn of(run: &'a PairChunks) -> Self {
        match run {
            PairChunks::Ids(chunks) => Self::Ids(chunks),
            PairChunks::Numbers { chunks, ids } => Self::Numbers(chunks, ids),
        }
    }

    fn lens(&self) -> Vec<usize> {
        match self {
            Self::Ids(chunks) => chunks.iter().map(Vec::len).collect(),
            Self::Numbers(chunks, _) => chunks.iter().map(Vec::len).collect(),
        }
    }

    /// Pair `j` of chunk `c`, as ids.
    #[inline]
    fn get(&self, c: usize, j: usize) -> [u64; 2] {
        match self {
            Self::Ids(chunks) => chunks[c][j],
            Self::Numbers(chunks, ids) => numbered(ids, chunks[c][j]),
        }
    }

    /// Calls `f` with pairs `from..to` of chunk `c`, as ids.
    #[inline]
    fn each(&self, c: usize, from: usize, to: usize, f: &mut impl FnMut([u64; 2])) {
        match self {
            Self::Ids(chunks) => chunks[c][from..to].iter().for_each(|&pair| f(pair)),
            Self::Numbers(chunks, ids) => chunks[c][from..to]
                .iter()
                .for_each(|&n| f(numbered(ids, n))),
        }
    }
}

/// A run of one predicate's pairs read as keys of `order`, in the default graph:
/// `(subject, object)` pairs, or `(object, subject)` ones (`by_object`); a window of its
/// pairs ([`Self::within`]).
pub(crate) struct PairRun<'a> {
    chunks: Chunks<'a>,
    /// Where each chunk starts among the run's pairs; the run's length last.
    starts: Vec<usize>,
    window: Range<usize>,
    predicate: u64,
    by_object: bool,
    order: Permutation,
}

impl<'a> PairRun<'a> {
    pub(crate) fn new(
        run: &'a PairChunks,
        predicate: u64,
        by_object: bool,
        order: Permutation,
    ) -> Self {
        let chunks = Chunks::of(run);
        let mut starts = Vec::new();
        let mut at = 0;
        for len in chunks.lens() {
            starts.push(at);
            at += len;
        }
        starts.push(at);
        Self {
            chunks,
            starts,
            window: 0..at,
            predicate,
            by_object,
            order,
        }
    }

    /// All its pairs, the window's or not.
    pub(crate) fn pairs(&self) -> usize {
        *self.starts.last().expect("the length")
    }

    /// The chunk position `i` is in.
    fn chunk_at(&self, i: usize) -> usize {
        self.starts.partition_point(|&start| start <= i) - 1
    }

    fn pair(&self, i: usize) -> [u64; 2] {
        let chunk = self.chunk_at(i);
        self.chunks.get(chunk, i - self.starts[chunk])
    }

    /// Only the pairs whose first component lies in `low..high`.
    pub(crate) fn within(mut self, low: u64, high: u64) -> Self {
        let first = |bound: u64| {
            let (mut a, mut b) = (0, self.pairs());
            while a < b {
                let mid = a + (b - a) / 2;
                if self.pair(mid)[0] < bound {
                    a = mid + 1;
                } else {
                    b = mid;
                }
            }
            a
        };
        self.window = first(low)..first(high);
        self
    }

    /// Calls `f` with the pairs at `start..end` (positions among the run's), in order.
    fn each(&self, start: usize, end: usize, mut f: impl FnMut([u64; 2])) {
        if start >= end {
            return;
        }
        let mut chunk = self.chunk_at(start);
        let mut i = start;
        while i < end {
            let at = self.starts[chunk];
            let to = (end - at).min(self.starts[chunk + 1] - at);
            self.chunks.each(chunk, i - at, to, &mut f);
            i = at + to;
            chunk += 1;
        }
    }

    #[inline]
    fn key_of(&self, [a, b]: [u64; 2]) -> Key {
        let (s, o) = if self.by_object { (b, a) } else { (a, b) };
        self.order.to_key(&EncodedQuad::from_components([
            s,
            self.predicate,
            o,
            DEFAULT_GRAPH,
        ]))
    }
}

impl SortedKeys for PairRun<'_> {
    fn len(&self) -> usize {
        self.window.len()
    }

    fn key(&self, i: usize) -> Key {
        self.key_of(self.pair(self.window.start + i))
    }

    fn read(&self, range: Range<usize>, out: &mut Vec<Key>) {
        let (start, end) = (
            self.window.start + range.start,
            self.window.start + range.end,
        );
        self.each(start, end, |pair| out.push(self.key_of(pair)));
    }
}

/// A numbered [`PairRun`]'s window read as its pairs of numbers (one word each, sorted as
/// the pairs are): one predicate's runs merged by a word's comparison and their keys made
/// once, after the merge.
struct Words<'r, 'a>(&'r PairRun<'a>);

impl Words<'_, '_> {
    /// The run's chunks of numbers, if it is numbered.
    fn chunks(&self) -> &[Vec<u64>] {
        match self.0.chunks {
            Chunks::Numbers(chunks, _) => chunks,
            Chunks::Ids(_) => unreachable!("words of a numbered run"),
        }
    }
}

impl SortedKeys<u64> for Words<'_, '_> {
    fn len(&self) -> usize {
        self.0.window.len()
    }

    fn key(&self, i: usize) -> u64 {
        let i = self.0.window.start + i;
        let chunk = self.0.chunk_at(i);
        self.chunks()[chunk][i - self.0.starts[chunk]]
    }

    fn read(&self, range: Range<usize>, out: &mut Vec<u64>) {
        let (start, end) = (
            self.0.window.start + range.start,
            self.0.window.start + range.end,
        );
        if start >= end {
            return;
        }
        let (chunks, starts) = (self.chunks(), &self.0.starts);
        let mut chunk = self.0.chunk_at(start);
        let mut i = start;
        while i < end {
            let at = starts[chunk];
            let to = (end - at).min(starts[chunk + 1] - at);
            out.extend_from_slice(&chunks[chunk][i - at..to]);
            i = at + to;
            chunk += 1;
        }
    }
}

/// Adds the union of `runs` (one predicate's, in one order) to `out`: by their words where
/// every run is numbered by the same table, then each word's key made once, on every core;
/// otherwise by their keys.
fn merge_runs_into(runs: &[PairRun<'_>], out: &mut PackedBuilder, wave: usize) {
    let table = match runs.first().map(|run| run.chunks) {
        Some(Chunks::Numbers(_, ids)) => Some(ids),
        _ => None,
    };
    let shared = table.filter(|table| {
        runs.iter()
            .all(|run| matches!(run.chunks, Chunks::Numbers(_, ids) if Arc::ptr_eq(ids, table)))
    });
    let (Some(ids), Some(first)) = (shared, runs.first()) else {
        merge_into(&dyn_runs(runs), out, wave);
        return;
    };
    let words: Vec<Words<'_, '_>> = runs.iter().map(Words).collect();
    let words: Vec<&dyn SortedKeys<u64>> = words
        .iter()
        .map(|run| run as &dyn SortedKeys<u64>)
        .collect();
    let mut keys: Vec<Key> = Vec::new();
    merge_with(&words, wave, &mut |pairs: &[u64]| {
        pairs
            .par_iter()
            .map(|&n| first.key_of(numbered(ids, n)))
            .collect_into_vec(&mut keys);
        #[cfg(test)]
        KEYS_MADE.with(|made| made.set(made.get() + keys.len() as u64));
        out.extend(&keys);
    });
}

#[cfg(test)]
thread_local! {
    /// The keys [`merge_runs_into`] made from merged words, on this thread (the guard's
    /// count).
    static KEYS_MADE: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// A range of packed keys of one permutation, read as keys of another in which they are
/// sorted too (one predicate's PSOG range as SPOG keys).
pub(crate) struct PackedRange<'a> {
    keys: &'a PackedKeys,
    range: Range<usize>,
    from: Permutation,
    to: Permutation,
}

impl PackedRange<'_> {
    #[inline]
    fn convert(&self, key: &Key) -> Key {
        self.to.to_key(&self.from.key_to_quad(key))
    }
}

impl SortedKeys for PackedRange<'_> {
    fn len(&self) -> usize {
        self.range.len()
    }

    fn key(&self, i: usize) -> Key {
        self.convert(&self.keys.get(self.range.start + i))
    }

    fn read(&self, range: Range<usize>, out: &mut Vec<Key>) {
        let (mut at, end) = (self.range.start + range.start, self.range.start + range.end);
        while at < end {
            let block_end = ((at / BLOCK + 1) * BLOCK).min(end);
            let first = out.len();
            self.keys.decode_range(at, block_end, out);
            for key in &mut out[first..] {
                *key = self.convert(key);
            }
            at = block_end;
        }
    }
}

/// Subjects a stored statement may have: IRIs and blank nodes, whose ids are one range.
const SUBJECTS: Range<u64> = (TermKind::Iri as u64) << 60..(TermKind::BlankNode as u64 + 1) << 60;

/// The permutations of the default-graph layout holding the statements of `relations`
/// (a predicate may come more than once, in any order): those with an IRI predicate and
/// an IRI or blank node subject, the others left out as no stored statement can have
/// them. A relation's runs are freed once its PSOG and POSG keys are packed.
#[cfg(test)]
pub(crate) fn default_graph_permutations(
    relations: Vec<PredicateRuns>,
    encoding: IndexEncoding,
    wave: usize,
) -> Vec<(Permutation, PackedKeys)> {
    default_graph_permutations_timed(relations, encoding, wave).0
}

/// [`default_graph_permutations`], with the time PSOG and POSG took (from the runs) and
/// SPOG and OSPG (merged from them).
pub(crate) fn default_graph_permutations_timed(
    mut relations: Vec<PredicateRuns>,
    encoding: IndexEncoding,
    wave: usize,
) -> (Vec<(Permutation, PackedKeys)>, [std::time::Duration; 2]) {
    let started = std::time::Instant::now();
    relations.retain(|relation| relation.predicate.kind() == TermKind::Iri);
    relations.sort_by_key(|relation| relation.predicate);
    let mut psog = PackedBuilder::new(encoding);
    let mut posg = PackedBuilder::new(encoding);
    // Each predicate's range of PSOG and of POSG.
    let (mut psog_ranges, mut posg_ranges): (Vec<Range<usize>>, Vec<Range<usize>>) =
        (Vec::new(), Vec::new());
    let mut relations = relations.into_iter().peekable();
    while let Some(mut relation) = relations.next() {
        // The runs of a predicate that came more than once, together.
        while let Some(next) = relations.next_if(|next| next.predicate == relation.predicate) {
            relation.by_subject.extend(next.by_subject);
            relation.by_object = relation.by_object.zip(next.by_object).map(|(mut a, b)| {
                a.extend(b);
                a
            });
        }
        let p = relation.predicate.raw();
        let (psog_start, posg_start) = (psog.len(), posg.len());
        let by_subject: Vec<PairRun<'_>> = relation
            .by_subject
            .iter()
            .map(|run| {
                PairRun::new(run, p, false, Permutation::Psog).within(SUBJECTS.start, SUBJECTS.end)
            })
            .collect();
        merge_runs_into(&by_subject, &mut psog, wave);
        let whole = by_subject.iter().all(|run| run.len() == run.pairs());
        match relation.by_object.as_ref().filter(|_| whole) {
            Some(runs) => {
                let by_object: Vec<PairRun<'_>> = runs
                    .iter()
                    .map(|run| PairRun::new(run, p, true, Permutation::Posg))
                    .collect();
                merge_runs_into(&by_object, &mut posg, wave);
            }
            None => {
                let objects = by_object_of(&by_subject);
                let run = PairRun::new(&objects, p, true, Permutation::Posg);
                merge_runs_into(std::slice::from_ref(&run), &mut posg, wave);
            }
        }
        drop(by_subject);
        drop(relation);
        psog_ranges.push(psog_start..psog.len());
        posg_ranges.push(posg_start..posg.len());
    }
    let (psog, posg) = (psog.finish(), posg.finish());
    let pair_orders = started.elapsed();
    let started = std::time::Instant::now();
    // Side by side: each merge hands its waves over and packs them on the calling thread
    // between its parallel parts, which the other's parts fill. Each holds half a wave, so
    // together they hold no more than one merge alone (the inferred-memory guard's peak).
    let half = if wave == 0 { 0 } else { (wave / 2).max(1) };
    let (spog, ospg) = rayon::join(
        || {
            merged_ranges(
                &psog,
                &psog_ranges,
                Permutation::Psog,
                Permutation::Spog,
                encoding,
                half,
            )
        },
        || {
            merged_ranges(
                &posg,
                &posg_ranges,
                Permutation::Posg,
                Permutation::Ospg,
                encoding,
                half,
            )
        },
    );
    let built = vec![
        (Permutation::Spog, spog),
        (Permutation::Posg, posg),
        (Permutation::Ospg, ospg),
        (Permutation::Psog, psog),
    ];
    (built, [pair_orders, started.elapsed()])
}

/// The union of `ranges` of `keys` (of permutation `from`), each sorted in `to` too (a
/// predicate's range of PSOG is sorted as SPOG keys, of POSG as OSPG keys), packed in `to`,
/// at most `wave` keys unpacked at once.
fn merged_ranges(
    keys: &PackedKeys,
    ranges: &[Range<usize>],
    from: Permutation,
    to: Permutation,
    encoding: IndexEncoding,
    wave: usize,
) -> PackedKeys {
    let runs: Vec<PackedRange<'_>> = ranges
        .iter()
        .map(|range| PackedRange {
            keys,
            range: range.clone(),
            from,
            to,
        })
        .collect();
    let mut out = PackedBuilder::new(encoding);
    merge_in_waves(&dyn_runs(&runs), wave, &mut |keys: &[Key]| out.extend(keys));
    out.finish()
}

fn dyn_runs<T: SortedKeys>(runs: &[T]) -> Vec<&dyn SortedKeys> {
    runs.iter().map(|run| run as &dyn SortedKeys).collect()
}

/// The pairs of the windows of `runs` as `(object, subject)` pairs, sorted and distinct:
/// in numbers where every run numbers its pairs by the same table (a word a pair, and a
/// word's sort), else as pairs of ids.
fn by_object_of(runs: &[PairRun<'_>]) -> PairChunks {
    let table = match runs.first().map(|run| run.chunks) {
        Some(Chunks::Numbers(_, ids)) => Some(ids),
        _ => None,
    };
    let shared = table.filter(|table| {
        runs.iter()
            .all(|run| matches!(run.chunks, Chunks::Numbers(_, ids) if Arc::ptr_eq(ids, table)))
    });
    let total = runs.iter().map(SortedKeys::len).sum();
    match shared {
        Some(table) => {
            let mut numbers: Vec<u64> = Vec::with_capacity(total);
            for run in runs {
                let Chunks::Numbers(chunks, _) = run.chunks else {
                    unreachable!("every run numbered")
                };
                let (start, end) = (run.window.start, run.window.end);
                let mut chunk = if start < end {
                    run.chunk_at(start)
                } else {
                    chunks.len()
                };
                let mut i = start;
                while i < end {
                    let at = run.starts[chunk];
                    let to = (end - at).min(chunks[chunk].len());
                    numbers.extend(chunks[chunk][i - at..to].iter().map(|n| n.rotate_left(32)));
                    i = at + to;
                    chunk += 1;
                }
            }
            numbers.par_sort_unstable();
            numbers.dedup();
            PairChunks::Numbers {
                chunks: vec![numbers],
                ids: Arc::clone(table),
            }
        }
        None => {
            let mut pairs: Vec<[u64; 2]> = Vec::with_capacity(total);
            for run in runs {
                run.each(run.window.start, run.window.end, |[s, o]| {
                    pairs.push([o, s])
                });
            }
            let kept = nrese_exec::sort::sort_dedup_keys(&mut pairs);
            pairs.truncate(kept);
            PairChunks::Ids(vec![pairs])
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::Layout;
    use crate::index::run::Run;

    struct Rng(u64);

    impl Rng {
        fn below(&mut self, n: u64) -> u64 {
            self.0 = self
                .0
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            (self.0 >> 33) % n
        }
    }

    fn id(kind: TermKind, payload: u64) -> u64 {
        ((kind as u64) << 60) | payload
    }

    /// A sorted run of `pairs`, cut into chunks at random (some empty).
    fn chunked(rng: &mut Rng, mut pairs: Vec<[u64; 2]>) -> PairChunks {
        pairs.sort_unstable();
        pairs.dedup();
        let mut chunks = Vec::new();
        let mut rest = pairs.as_slice();
        while !rest.is_empty() {
            let n = (rng.below(40) as usize).min(rest.len());
            chunks.push(rest[..n].to_vec());
            rest = &rest[n..];
        }
        PairChunks::Ids(chunks)
    }

    /// `relations` with every run numbered by one table of their ids, in order (as a rule
    /// engine's working set numbers them), number 0 for no id; the relations of `others`
    /// numbered by a table of their own, the rest left as ids.
    fn numbered(relations: &[PredicateRuns], others: impl Fn(usize) -> bool) -> Vec<PredicateRuns> {
        let table = |relations: &[&PredicateRuns]| -> Arc<[u64]> {
            let mut ids: Vec<u64> = relations
                .iter()
                .flat_map(|r| r.by_subject.iter().chain(r.by_object.iter().flatten()))
                .flat_map(|run| run.pairs().flatten().collect::<Vec<_>>())
                .collect();
            ids.sort_unstable();
            ids.dedup();
            std::iter::once(0).chain(ids).collect::<Vec<_>>().into()
        };
        let shared = table(&relations.iter().collect::<Vec<_>>());
        let number = |run: &PairChunks, ids: &Arc<[u64]>| {
            let n = |id: u64| ids[1..].binary_search(&id).expect("in the table") as u64 + 1;
            let PairChunks::Ids(chunks) = run else {
                unreachable!("ids")
            };
            PairChunks::Numbers {
                chunks: chunks
                    .iter()
                    .map(|chunk| chunk.iter().map(|&[a, b]| (n(a) << 32) | n(b)).collect())
                    .collect(),
                ids: Arc::clone(ids),
            }
        };
        relations
            .iter()
            .enumerate()
            .map(|(i, relation)| {
                let ids = match others(i) {
                    true => table(&[relation]),
                    false => Arc::clone(&shared),
                };
                PredicateRuns {
                    predicate: relation.predicate,
                    by_subject: relation
                        .by_subject
                        .iter()
                        .map(|run| number(run, &ids))
                        .collect(),
                    by_object: relation
                        .by_object
                        .as_ref()
                        .map(|runs| runs.iter().map(|run| number(run, &ids)).collect()),
                }
            })
            .collect()
    }

    /// Random relations: subjects mostly IRIs, some blank nodes and literals; objects of
    /// any kind; predicates IRIs and one literal; a predicate may come twice; runs of a
    /// predicate share pairs; object orders given for some.
    fn relations(
        rng: &mut Rng,
        predicates: u64,
        facts: u64,
    ) -> (Vec<PredicateRuns>, Vec<EncodedQuad>) {
        let mut out = Vec::new();
        let mut expected = Vec::new();
        for p in 0..predicates {
            let predicate = match p {
                0 => id(TermKind::String, 7),
                _ => id(TermKind::Iri, 1000 + p % (predicates - 1).max(1)),
            };
            let runs = 1 + rng.below(3);
            let mut by_subject = Vec::new();
            let mut all: Vec<[u64; 2]> = Vec::new();
            for _ in 0..runs {
                let n = rng.below(facts + 1);
                let pairs: Vec<[u64; 2]> = (0..n)
                    .map(|_| {
                        let subject = match rng.below(20) {
                            0 => id(TermKind::BlankNode, rng.below(50)),
                            1 => id(TermKind::String, rng.below(50)),
                            _ => id(TermKind::Iri, rng.below(300)),
                        };
                        let object = match rng.below(3) {
                            0 => id(TermKind::Integer, rng.below(1 << 40)),
                            _ => id(TermKind::Iri, rng.below(300)),
                        };
                        [subject, object]
                    })
                    .collect();
                all.extend(&pairs);
                by_subject.push(chunked(rng, pairs));
            }
            let by_object = (rng.below(2) == 0).then(|| {
                by_subject
                    .iter()
                    .map(|run| {
                        let swapped = run.pairs().map(|[s, o]| [o, s]).collect();
                        chunked(rng, swapped)
                    })
                    .collect()
            });
            let storable = TermId::from_raw(predicate).kind() == TermKind::Iri;
            for [s, o] in all {
                if storable && SUBJECTS.contains(&s) {
                    expected.push(EncodedQuad::from_components([
                        s,
                        predicate,
                        o,
                        DEFAULT_GRAPH,
                    ]));
                }
            }
            out.push(PredicateRuns {
                predicate: TermId::from_raw(predicate),
                by_subject,
                by_object,
            });
        }
        (out, expected)
    }

    /// Merged from runs, every permutation holds what one built from the statements does
    /// (`Run::from_quads`, sorted): across parts and waves, with runs that share pairs,
    /// predicates that come twice, object orders given or built, and statements no stack
    /// can hold left out.
    #[test]
    fn permutations_merged_from_runs_equal_those_sorted_from_statements() {
        let mut rng = Rng(17);
        for (predicates, facts) in [(1, 0), (2, 5), (3, 400), (12, 150), (40, 60)] {
            for _ in 0..6 {
                let (relations, expected) = relations(&mut rng, predicates, facts);
                let oracle = Run::from_quads(Layout::DefaultGraph, IndexEncoding::Fast, expected);
                // As ids, numbered by one table, and numbered by tables of their own where
                // a predicate comes twice (an object order then built as ids).
                let forms = [
                    relations.clone(),
                    numbered(&relations, |_| false),
                    numbered(&relations, |i| i % 2 == 1),
                ];
                for (form, relations) in forms.into_iter().enumerate() {
                    let built = default_graph_permutations(relations, IndexEncoding::Fast, 300);
                    for (permutation, keys) in &built {
                        let want: Vec<Key> = oracle.permutation(*permutation).keys.iter().collect();
                        let got: Vec<Key> = keys.iter().collect();
                        assert_eq!(
                            got, want,
                            "{permutation:?} ({predicates} predicates, form {form})"
                        );
                    }
                    assert_eq!(built.len(), 4);
                }
            }
        }
    }

    /// Guard: one predicate's runs numbered by one table merge by their words (a word's
    /// comparison, one-word moves), and each statement's PSOG and POSG keys are made once,
    /// after the merge: a merge of their keys made one for every pair of every run, and
    /// moved 40-byte heap entries (CSPA httpd's subset: the inferred stack's permutations
    /// took about as long as the closure, 13 October 2026).
    #[test]
    fn numbered_runs_merge_by_words_and_make_each_key_once() {
        let mut rng = Rng(29);
        for _ in 0..4 {
            let (relations, _) = relations(&mut rng, 8, 300);
            KEYS_MADE.with(|made| made.set(0));
            let built = default_graph_permutations(
                numbered(&relations, |_| false),
                IndexEncoding::Fast,
                300,
            );
            let made = KEYS_MADE.with(std::cell::Cell::get);
            let len = |order: Permutation| {
                built
                    .iter()
                    .find(|(permutation, _)| *permutation == order)
                    .map_or(0, |(_, keys)| keys.len())
            };
            assert_eq!(
                made as usize,
                len(Permutation::Psog) + len(Permutation::Posg)
            );
        }
    }

    /// A merge in parts equals the sorted union of its runs, keys in several runs once;
    /// also of many runs too short to be sampled at the parts' step.
    #[test]
    fn a_merge_in_parts_is_the_sorted_union() {
        let mut rng = Rng(5);
        for (runs, keys) in [
            (1, 900),
            (2, 900),
            (3, 900),
            (4, 900),
            (5, 900),
            (7, 900),
            (200, 2),
        ] {
            let chunks: Vec<PairChunks> = (0..runs)
                .map(|_| {
                    let n = rng.below(keys);
                    let pairs = (0..n).map(|_| [rng.below(60), rng.below(60)]).collect();
                    chunked(&mut rng, pairs)
                })
                .collect();
            let streams: Vec<PairRun<'_>> = chunks
                .iter()
                .map(|run| PairRun::new(run, 9, false, Permutation::Spog))
                .collect();
            let merged: Vec<Key> = merge_packed(&dyn_runs(&streams), IndexEncoding::Fast, 300)
                .iter()
                .collect();
            let mut want: Vec<Key> = chunks
                .iter()
                .flat_map(PairChunks::pairs)
                .map(|[s, o]| [s, 9, o, DEFAULT_GRAPH])
                .collect();
            want.sort_unstable();
            want.dedup();
            assert_eq!(merged, want, "{runs} runs");
        }
    }

    #[test]
    fn range_boundaries_are_compared_without_packing_the_exclusive_rank() {
        let ids: Arc<[u64]> = Arc::from([0, 0, 10, 20, 30, 40, 50, 60]);
        let numbers = PairChunks::Numbers {
            chunks: vec![vec![(1u64 << 32) | 7, (7u64 << 32) | 1]],
            ids,
        };
        let wide = PairChunks::Ids(vec![vec![[0, 60], [60, 0]]]);
        for run in [&numbers, &wide] {
            assert_eq!(
                run.with_first_from(0).collect::<Vec<_>>(),
                [[0, 60], [60, 0]]
            );
            assert_eq!(run.with_first_from(60).collect::<Vec<_>>(), [[60, 0]]);
            // Numbered lower bound is N+1 (8), not a member or a packed pair.
            assert!(run.with_first_from(61).next().is_none());
            assert!(run.with_first_from(u64::MAX).next().is_none());
        }
        assert!(PairChunks::default().with_first_from(0).next().is_none());
    }

    /// Per-part lists append actual keys; no wave-sized default-initialized buffer.
    #[test]
    fn merged_parts_do_not_initialize_a_buffer_of_default_keys() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static DEFAULTS: AtomicUsize = AtomicUsize::new(0);
        #[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
        struct Counted(u64);
        impl Default for Counted {
            fn default() -> Self {
                DEFAULTS.fetch_add(1, Ordering::Relaxed);
                Self(0)
            }
        }
        struct Run(Vec<Counted>);
        impl SortedKeys<Counted> for Run {
            fn len(&self) -> usize {
                self.0.len()
            }
            fn key(&self, i: usize) -> Counted {
                self.0[i]
            }
            fn read(&self, range: Range<usize>, out: &mut Vec<Counted>) {
                out.extend_from_slice(&self.0[range]);
            }
        }
        let a = Run((0..1000).step_by(2).map(Counted).collect());
        let b = Run((0..1000).step_by(3).map(Counted).collect());
        let mut expected: Vec<_> = a.0.iter().chain(&b.0).copied().collect();
        expected.sort_unstable();
        expected.dedup();
        for wave in [13, 300, 0] {
            DEFAULTS.store(0, Ordering::Relaxed);
            let mut actual = Vec::new();
            merge_with(&[&a, &b], wave, &mut |part| actual.extend_from_slice(part));
            assert!(actual == expected);
            assert_eq!(DEFAULTS.load(Ordering::Relaxed), 0);
        }
    }
}
