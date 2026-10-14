//! The working set's storage: sorted runs of pairs in chunks, their merges, and the
//! sorts that turn a round's candidates into runs.

use super::*;

/// A relation's new pairs, in its subject order, with the marks of those a closed family
/// produced ([`Marks`]).
pub(super) type New<R> = (Pairs<R>, Option<Marks>);

/// The new pairs of the lists `open` and `closed` (each sorted and distinct), those of
/// `closed` marked: what a closed family produced.
pub(super) fn new_pairs<R: Representation>(open: Vec<Vec<R>>, mut closed: Vec<Vec<R>>) -> New<R> {
    closed.retain(|part| !part.is_empty());
    if closed.is_empty() {
        return (union(open), None);
    }
    let all = open.iter().all(Vec::is_empty);
    let parts = open
        .into_iter()
        .map(|part| (part, Marking::None))
        .chain(closed.into_iter().map(|part| (part, Marking::All)))
        .collect();
    let (pairs, marks) = sort_marked(parts, false);
    let marks = Marks {
        all,
        so: if all {
            Vec::new()
        } else {
            marks.expect("marked")
        },
        os: None,
    };
    (pairs, Some(marks))
}

/// The union of sorted, distinct pair lists, in chunks (see [`sort_chunked`]).
pub(super) fn union<R: Representation>(mut parts: Vec<Vec<R>>) -> Pairs<R> {
    if parts.len() == 1 {
        return Pairs::<R>::chunked(parts.pop().expect("one part"));
    }
    sort_chunked(parts, false)
}

/// The pairs of `parts` (each read as `(b, a)` if `swap`), sorted and distinct, in chunks
/// of at most about [`chunk_pairs`] pairs, without ever holding them in one vector: a sample of
/// them gives the chunks' bounds, each part is split by the bounds (and freed, if it is
/// owned), and each chunk's pieces are sorted together. So the parts and their pieces
/// are what is held at once, not the parts beside a sorted copy of all of them (at
/// LUBM 1000, a round's `rdf:type` candidates, 2.2 times its new facts: 1 GB).
pub(super) fn sort_chunked<R: Representation, P: AsRef<[R]> + Send>(
    parts: Vec<P>,
    swap: bool,
) -> Pairs<R> {
    let parts = parts
        .into_iter()
        .map(|part| (part, Marking::None))
        .collect();
    sort_marked(parts, swap).0
}

/// How the pairs of a part of [`sort_marked`] are marked ([`Marks`]).
#[derive(Clone, Copy)]
pub(super) enum Marking<'a> {
    None,
    All,
    /// A bit per pair of the part.
    Bits(&'a [u64]),
}

/// [`sort_chunked`] of parts whose pairs may be marked, with the marks per chunk if a part
/// is (a pair both marked and not is marked). Each chunk's marked and unmarked pieces are
/// sorted apart and merged, so the marks cost a pass over the chunk in the cache. The
/// chunks are of even sizes, never a power of two: a binary search over arrays of
/// 2^20 pairs met the same cache sets at every step (R13's layout, 7 October 2026).
pub(super) fn sort_marked<R: Representation, P: AsRef<[R]> + Send>(
    parts: Vec<(P, Marking<'_>)>,
    swap: bool,
) -> (Pairs<R>, Option<Vec<Vec<u64>>>) {
    sort_parts(parts, swap, false)
}

/// [`sort_marked`], where `in_order` says that the parts are a run's chunks (each sorted,
/// each before the next) and are read swapped: then each chunk's pieces, gathered in the
/// parts' order, are in order of their second number, and a stable sort by the first
/// sorts them ([`sort_by_first`]). Parts read as they are are each sorted and distinct
/// (a morsel's candidates, a run's chunks): they are cut at the bounds, not split pair by
/// pair.
fn sort_parts<R: Representation, P: AsRef<[R]> + Send>(
    parts: Vec<(P, Marking<'_>)>,
    swap: bool,
    in_order: bool,
) -> (Pairs<R>, Option<Vec<Vec<u64>>>) {
    debug_assert!(swap || !in_order);
    let read = |&p: &R| if swap { R::swapped(p) } else { p };
    let total: usize = parts.iter().map(|(part, _)| part.as_ref().len()).sum();
    let any_marked = parts
        .iter()
        .any(|(part, marking)| !matches!(marking, Marking::None) && !part.as_ref().is_empty());
    let chunks = total.div_ceil(chunk_pairs()).max(1);
    // The bounds: chunk i takes the pairs from bounds[i - 1] up to bounds[i].
    let bounds: Vec<R> = if chunks > 1 {
        let step = (total / (chunks * 64)).max(1);
        let mut sample: Vec<R> = Vec::with_capacity(total / step + 1);
        let mut at = 0;
        for (part, _) in &parts {
            let part = part.as_ref();
            let first = (step - at % step) % step;
            sample.extend(part.iter().skip(first).step_by(step).map(read));
            at += part.len();
        }
        sample.par_sort_unstable();
        (1..chunks)
            .map(|i| sample[i * sample.len() / chunks])
            .collect()
    } else {
        Vec::new()
    };
    let chunk_of = |pair: &R| bounds.partition_point(|bound| bound <= pair);
    // Each part split by the bounds (and by its marks), at the pieces' exact sizes.
    type Pieces<R> = Vec<(usize, bool, Vec<R>)>;
    let pieces: Vec<Pieces<R>> = parts
        .into_par_iter()
        .map(|(part, marking)| {
            let pairs = part.as_ref();
            // A loop per marking, so that none tests it per pair.
            match marking {
                Marking::None if !swap => cut_part(pairs, &bounds, false),
                Marking::All if !swap => cut_part(pairs, &bounds, true),
                Marking::None => split_part(pairs, chunks, &chunk_of, read, |_| false),
                Marking::All => split_part(pairs, chunks, &chunk_of, read, |_| true),
                Marking::Bits(words) => {
                    split_part(pairs, chunks, &chunk_of, read, |i| marked(words, i))
                }
            }
        })
        .collect();
    let mut by_chunk: Vec<[Vec<Vec<R>>; 2]> =
        (0..chunks).map(|_| [Vec::new(), Vec::new()]).collect();
    for (chunk, is_marked, piece) in pieces.into_iter().flatten() {
        by_chunk[chunk][usize::from(is_marked)].push(piece);
    }
    let gather = |pieces: Vec<Vec<R>>| {
        let mut chunk = Vec::with_capacity(pieces.iter().map(Vec::len).sum());
        for piece in pieces {
            chunk.extend_from_slice(&piece);
        }
        if in_order {
            return sort_by_first(chunk);
        }
        // In parallel too: few chunks may come at once (OWL2Bench RL-1's modules hand
        // over one list of 1.3 M pairs: two chunks, two threads, 13 % slower).
        chunk.par_sort_unstable();
        chunk.dedup();
        chunk
    };
    let sorted: Vec<(Vec<R>, Option<Vec<u64>>)> = by_chunk
        .into_par_iter()
        .map(|[open, closed]| {
            let (open, closed) = (gather(open), gather(closed));
            let (chunk, words) = if closed.is_empty() {
                (open, None)
            } else {
                let (chunk, words) = merge_marked(&open, &closed);
                (chunk, Some(words))
            };
            let words =
                any_marked.then(|| words.unwrap_or_else(|| vec![0; chunk.len().div_ceil(64)]));
            if chunk.len() == chunk.capacity() {
                (chunk, words)
            } else {
                (chunk.as_slice().to_vec(), words)
            }
        })
        .collect();
    let mut out = Pairs::<R>::default();
    let mut marks = any_marked.then(Vec::new);
    for (chunk, words) in sorted {
        if chunk.is_empty() {
            continue;
        }
        out.push(chunk);
        if let (Some(marks), Some(words)) = (&mut marks, words) {
            marks.push(words);
        }
    }
    (out, marks)
}

/// The pieces of sorted `pairs` per chunk of [`sort_marked`], all marked or none: the
/// pairs between each two bounds, found by a search per bound, not a test per pair.
fn cut_part<R: Representation>(
    pairs: &[R],
    bounds: &[R],
    marked: bool,
) -> Vec<(usize, bool, Vec<R>)> {
    let (Some(&head), Some(&tail)) = (pairs.first(), pairs.last()) else {
        return Vec::new();
    };
    let chunk_of = |pair: R| bounds.partition_point(|&bound| bound <= pair);
    let mut pieces = Vec::new();
    let mut from = 0;
    for chunk in chunk_of(head)..=chunk_of(tail) {
        let to = match bounds.get(chunk) {
            Some(&bound) => from + pairs[from..].partition_point(|&p| p < bound),
            None => pairs.len(),
        };
        if to > from {
            pieces.push((chunk, marked, pairs[from..to].to_vec()));
        }
        from = to;
    }
    pieces
}

/// `chunk`, whose pairs are in order of their second numbers, sorted by their first ones,
/// stably: in order. A pass of counts per digit of the first numbers above the least: one
/// pass where the counts are no more than twice the pairs (a large delta's chunk of
/// objects, whose numbers are dense), otherwise passes of 11 bits; a comparison sort of a
/// chunk reads it about 20 times.
pub(super) fn sort_by_first<R: Representation>(chunk: Vec<R>) -> Vec<R> {
    if chunk.len() < 256 {
        let mut chunk = chunk;
        chunk.sort_unstable();
        return chunk;
    }
    let (low, high) = chunk.iter().fold((u64::MAX, 0), |(low, high), &p| {
        (low.min(R::first(p)), high.max(R::first(p)))
    });
    let (bits, width) = digits(chunk.len(), high - low);
    let (mut from, mut to) = (chunk, Vec::new());
    let mut shift = 0;
    while shift < bits {
        let digits = width.min(bits - shift);
        let mask = (1u64 << digits) - 1;
        let digit = |p: R| (((R::first(p) - low) >> shift) & mask) as usize;
        let mut starts = vec![0usize; (1 << digits) + 1];
        for &p in &from {
            starts[digit(p) + 1] += 1;
        }
        for i in 1..starts.len() {
            starts[i] += starts[i - 1];
        }
        to.clear();
        to.resize(from.len(), R::default());
        for &p in &from {
            let at = &mut starts[digit(p)];
            to[*at] = p;
            *at += 1;
        }
        std::mem::swap(&mut from, &mut to);
        shift += digits;
    }
    from
}

/// The bits of first numbers spread over `spread` above the least, and the bits of a digit
/// [`sort_by_first`] counts per pass for `pairs` pairs.
fn digits(pairs: usize, spread: u64) -> (u32, u32) {
    let bits = 64 - spread.leading_zeros();
    match 1u64
        .checked_shl(bits)
        .is_some_and(|counts| counts <= 2 * pairs as u64)
    {
        true => (bits, bits),
        false => (bits, 11),
    }
}

/// The pieces of `pairs` (each read by `read`) per chunk of [`sort_marked`] and per mark:
/// `(chunk, marked, pairs)`, at their exact sizes.
pub(super) fn split_part<R: Representation>(
    pairs: &[R],
    chunks: usize,
    chunk_of: &impl Fn(&R) -> usize,
    read: impl Fn(&R) -> R,
    is_marked: impl Fn(usize) -> bool,
) -> Vec<(usize, bool, Vec<R>)> {
    let slot = |i: usize, pair: &R| 2 * chunk_of(pair) + usize::from(is_marked(i));
    let mut counts = vec![0; 2 * chunks];
    for (i, pair) in pairs.iter().enumerate() {
        counts[slot(i, &read(pair))] += 1;
    }
    let mut pieces: Vec<Vec<R>> = counts.iter().map(|&n| Vec::with_capacity(n)).collect();
    for (i, pair) in pairs.iter().enumerate() {
        let pair = read(pair);
        pieces[slot(i, &pair)].push(pair);
    }
    pieces
        .into_iter()
        .enumerate()
        .filter(|(_, piece)| !piece.is_empty())
        .map(|(at, piece)| (at / 2, at % 2 == 1, piece))
        .collect()
}

/// The union of `open` and `marked` (each sorted and distinct), with a bit per pair set
/// where it is in `marked` (a pair in both is marked).
pub(super) fn merge_marked<R: Representation>(open: &[R], marked: &[R]) -> (Vec<R>, Vec<u64>) {
    let mut out = Vec::with_capacity(open.len() + marked.len());
    let mut words = vec![0u64; out.capacity().div_ceil(64)];
    let (mut i, mut j) = (0, 0);
    while i < open.len() || j < marked.len() {
        let take_marked = j < marked.len() && (i == open.len() || marked[j] <= open[i]);
        if take_marked {
            if i < open.len() && open[i] == marked[j] {
                i += 1;
            }
            let at = out.len();
            words[at / 64] |= 1 << (at % 64);
            out.push(marked[j]);
            j += 1;
        } else {
            out.push(open[i]);
            i += 1;
        }
    }
    words.truncate(out.len().div_ceil(64));
    (out, words)
}

/// Pairs<R> per chunk of a run that is merged later: 16 MiB. Tiny in the unit tests, so
/// their closures go through runs of many chunks; [`set_chunk_pairs`] changes it.
pub(super) static CHUNK_PAIRS: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(if cfg!(test) { 16 } else { 1 << 20 });

/// The pairs per chunk ([`CHUNK_PAIRS`]).
pub(super) fn chunk_pairs() -> usize {
    CHUNK_PAIRS.load(std::sync::atomic::Ordering::Relaxed)
}

/// Sets the pairs per chunk of the working set's merged runs, for tests whose inputs are
/// too small to fill a chunk of the default 16 MiB.
#[doc(hidden)]
pub fn set_chunk_pairs(pairs: usize) {
    CHUNK_PAIRS.store(pairs.max(1), std::sync::atomic::Ordering::Relaxed);
}

/// Sorted, distinct pairs, in chunks: each chunk's pairs come before the next's. A run
/// that is merged later (a delta, which becomes an added run and is merged with others)
/// is held in chunks of [`chunk_pairs`] pairs, and a merge writes its result a chunk at a time
/// and frees its sources a chunk at a time as it passes them. So merging two added runs
/// never holds the old runs beside the new one: at LUBM 1000 that was
/// 1 GB per order of `rdf:type`, the materialisation's memory peak. The input, which is
/// never merged, stays one vector, unmoved.
#[derive(Default)]
pub(super) struct Pairs<R: Representation> {
    pub(super) chunks: Vec<Vec<R>>,
    /// Each chunk's last pair.
    pub(super) lasts: Vec<R>,
    pub(super) len: usize,
}

impl<R: Representation> Pairs<R> {
    /// `pairs` (sorted, distinct) as they are, one chunk.
    pub(super) fn whole(pairs: Vec<R>) -> Self {
        let mut out = Self::default();
        out.push(pairs);
        out
    }

    /// `pairs` (sorted, distinct) in chunks of [`chunk_pairs`] without spare capacity: copied
    /// unless they are one such chunk already.
    pub(super) fn chunked(pairs: Vec<R>) -> Self {
        let size = chunk_pairs();
        if pairs.len() <= size && pairs.len() == pairs.capacity() {
            return Self::whole(pairs);
        }
        let mut out = Self::default();
        for chunk in pairs.chunks(size) {
            out.push(chunk.to_vec());
        }
        out
    }

    pub(super) fn push(&mut self, chunk: Vec<R>) {
        if let Some(&last) = chunk.last() {
            self.len += chunk.len();
            self.lasts.push(last);
            self.chunks.push(chunk);
        }
    }

    pub(super) fn len(&self) -> usize {
        self.len
    }

    /// The bytes of the pairs, spare capacity included.
    pub(super) fn bytes(&self) -> usize {
        self.chunks.iter().map(Vec::capacity).sum::<usize>() * std::mem::size_of::<R>()
    }

    /// The chunk `pair` would be in: the first whose last pair isn't before it.
    pub(super) fn chunk_of(&self, pair: R) -> Option<&[R]> {
        let at = self.lasts.partition_point(|&last| last < pair);
        self.chunks.get(at).map(Vec::as_slice)
    }

    pub(super) fn contains(&self, pair: R) -> bool {
        self.chunk_of(pair)
            .is_some_and(|chunk| chunk.binary_search(&pair).is_ok())
    }

    /// The chunk and the place in it of `pair`, if it is there.
    pub(super) fn position(&self, pair: R) -> Option<(usize, usize)> {
        let at = self.lasts.partition_point(|&last| last < pair);
        let chunk = self.chunks.get(at)?;
        chunk.binary_search(&pair).ok().map(|i| (at, i))
    }

    /// Whether `pair` lies between the run's first and last pairs (never for an empty run).
    #[inline]
    pub(super) fn spans(&self, pair: R) -> bool {
        match (
            self.chunks.first().and_then(|chunk| chunk.first()),
            self.lasts.last(),
        ) {
            (Some(&low), Some(&high)) => low <= pair && pair <= high,
            _ => false,
        }
    }

    /// [`Self::contains`] read forward from `finger`, where the last probe landed: for
    /// probes in ascending order (a morsel's candidates).
    pub(super) fn contains_from(&self, finger: &mut PairsFinger, pair: R) -> bool {
        #[cfg(test)]
        PROBE_READS.with(|reads| reads.set(reads.get() + 1));
        let chunk = finger.chunk.seek(&self.lasts, &pair);
        if chunk != finger.current {
            finger.current = chunk;
            finger.within = Finger::default();
        }
        self.chunks
            .get(chunk)
            .is_some_and(|pairs| finger.within.contains(pairs, &pair))
    }

    /// The pairs whose first component is `key`: a slice of each chunk they are in.
    pub(super) fn range(&self, key: u64) -> impl Iterator<Item = &[R]> {
        self.range_at(key).map(|(_, _, pairs)| pairs)
    }

    /// [`Self::range`], each slice with its chunk and where it starts in it.
    pub(super) fn range_at(&self, key: u64) -> impl Iterator<Item = (usize, usize, &[R])> {
        self.range_from(key, &mut RangeFinger::default())
    }

    /// [`Self::range_at`] from `finger`, where the last key's pairs started: a key at or
    /// above it gallops from there, in the lines the last lookup read, and a lower one
    /// searches from the root. A join step's keys mostly ascend (its drivers are sorted,
    /// and many share a key), and the search from the root was the join kernel's slowest
    /// load (half of `range_at`'s samples at CSPA httpd's subset, 13 October 2026). The
    /// key's pairs end where a gallop from their start finds: a second search over the
    /// rest of the chunk was a lookup's slowest half (26 of 46 % of the kernel's samples
    /// there), for ranges of a few pairs.
    pub(super) fn range_from<'a>(
        &'a self,
        key: u64,
        finger: &mut RangeFinger,
    ) -> impl Iterator<Item = (usize, usize, &'a [R])> + use<'a, R> {
        let from_root = |chunks: &[Vec<R>], lasts: &[R], c: usize| {
            let at = c + lasts[c..].partition_point(|&last| R::first(last) < key);
            let start = chunks
                .get(at)
                .map_or(0, |chunk| chunk.partition_point(|&p| R::first(p) < key));
            (at, start)
        };
        let (at, start, roots) = match *finger {
            RangeFinger {
                key: last,
                chunk,
                at,
                roots,
            } if chunk < self.chunks.len() && last <= key => {
                if R::first(self.lasts[chunk]) >= key {
                    let pairs = &self.chunks[chunk];
                    let start =
                        nrese_exec::search::partition_point_from(pairs, at, |&p| R::first(p) < key);
                    (chunk, start, roots)
                } else {
                    let (at, start) = from_root(&self.chunks, &self.lasts, chunk + 1);
                    (at, start, roots + 1)
                }
            }
            RangeFinger { roots, .. } => {
                let (at, start) = from_root(&self.chunks, &self.lasts, 0);
                (at, start, roots + 1)
            }
        };
        *finger = RangeFinger {
            key,
            chunk: at,
            at: start,
            roots,
        };
        self.chunks[at.min(self.chunks.len())..]
            .iter()
            .enumerate()
            .map_while(move |(i, chunk)| {
                let start = if i == 0 { start } else { 0 };
                (chunk.get(start).is_some_and(|&p| R::first(p) == key)).then(|| {
                    let end = nrese_exec::search::partition_point_from(chunk, start, |&p| {
                        R::first(p) <= key
                    });
                    (at + i, start, &chunk[start..end])
                })
            })
    }

    pub(super) fn iter(&self) -> impl Iterator<Item = &R> {
        self.chunks.iter().flatten()
    }
}

#[cfg(test)]
thread_local! {
    /// The runs [`Run::<R>::each`] walked, on this thread (the guard's count).
    static RUN_READS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
    /// The runs [`Pairs::<R>::contains_from`] searched, on this thread (the guard's count).
    static PROBE_READS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// Where a lookup of [`Pairs::<R>::range_from`] found its key's pairs: the next lookup of a
/// key at or above it starts there.
#[derive(Clone, Copy)]
pub(super) struct RangeFinger {
    key: u64,
    chunk: usize,
    at: usize,
    /// The lookups that searched from the root (the guard's count).
    roots: u32,
}

impl Default for RangeFinger {
    fn default() -> Self {
        Self {
            key: u64::MAX,
            chunk: usize::MAX,
            at: 0,
            roots: 0,
        }
    }
}

/// A place in [`Pairs<R>`] a merge reads from.
pub(super) struct Cursor<R: Representation> {
    pub(super) pairs: Arc<Pairs<R>>,
    pub(super) chunk: usize,
    pub(super) at: usize,
}

impl<R: Representation> Cursor<R> {
    pub(super) fn new(pairs: Arc<Pairs<R>>) -> Self {
        Self {
            pairs,
            chunk: 0,
            at: 0,
        }
    }
}

/// What a merge reads from: sorted pairs, a slice at a time.
pub(super) trait Feed<R: Representation> {
    /// The next pairs not read yet; empty at the end.
    fn rest(&mut self) -> &[R];
    /// Marks the first `n` pairs of [`Feed<R>::rest`] read.
    fn read(&mut self, n: usize);
}

impl<R: Representation> Feed<R> for Cursor<R> {
    /// The pairs of the current chunk not read yet. A chunk read to its end is freed here
    /// if the cursor holds the only reference to the pairs.
    fn rest(&mut self) -> &[R] {
        while self
            .pairs
            .chunks
            .get(self.chunk)
            .is_some_and(|chunk| self.at == chunk.len())
        {
            if let Some(pairs) = Arc::get_mut(&mut self.pairs) {
                pairs.chunks[self.chunk] = Vec::new();
            }
            self.chunk += 1;
            self.at = 0;
        }
        self.pairs
            .chunks
            .get(self.chunk)
            .map_or(&[], |chunk| &chunk[self.at..])
    }

    fn read(&mut self, n: usize) {
        self.at += n;
    }
}

/// Which pairs of a delta one closed rule family produced ([`Relation::<R>::closed_by`]): a bit
/// per pair, per chunk of each order the delta keeps, or all of them. The family's
/// instances skip those pairs ([`Seg::DeltaNotBy`]); every other read sees the delta as
/// one run, so the partition costs lookups, folds and merges nothing.
#[derive(Default)]
pub(super) struct Marks {
    /// Every pair is marked (the family produced the whole delta).
    pub(super) all: bool,
    pub(super) so: Vec<Vec<u64>>,
    pub(super) os: Option<Vec<Vec<u64>>>,
}

impl Marks {
    pub(super) fn bytes(&self) -> usize {
        let words = |chunks: &Vec<Vec<u64>>| chunks.iter().map(Vec::capacity).sum::<usize>();
        (words(&self.so) + self.os.as_ref().map_or(0, words)) * 8
    }
}

/// Whether bit `at` of `words` is set.
pub(super) fn marked(words: &[u64], at: usize) -> bool {
    words[at / 64] >> (at % 64) & 1 != 0
}

/// Moves the pairs of `a` and `b` (disjoint), in order, into `out` until it is full or
/// both are read.
pub(super) fn fill<R: Representation>(
    a: &mut impl Feed<R>,
    b: &mut impl Feed<R>,
    out: &mut Vec<R>,
) {
    loop {
        let (x, y) = (a.rest(), b.rest());
        let room = out.capacity() - out.len();
        let (i, j) = if y.is_empty() {
            let n = room.min(x.len());
            out.extend_from_slice(&x[..n]);
            (n, 0)
        } else if x.is_empty() {
            let n = room.min(y.len());
            out.extend_from_slice(&y[..n]);
            (0, n)
        } else {
            let (mut i, mut j) = (0, 0);
            while i + j < room && i < x.len() && j < y.len() {
                if x[i] < y[j] {
                    out.push(x[i]);
                    i += 1;
                } else {
                    out.push(y[j]);
                    j += 1;
                }
            }
            (i, j)
        };
        if i + j == 0 {
            return;
        }
        a.read(i);
        b.read(j);
    }
}

/// The union of two disjoint feeds holding `total` pairs, in chunks of [`chunk_pairs`]; a
/// source a cursor holds the only reference to is freed a chunk at a time as the merge
/// passes it.
pub(super) fn merge<R: Representation>(
    mut a: impl Feed<R>,
    mut b: impl Feed<R>,
    total: usize,
) -> Pairs<R> {
    let mut out = Pairs::<R>::default();
    let size = chunk_pairs();
    while out.len() < total {
        let mut chunk = Vec::with_capacity(size.min(total - out.len()));
        fill(&mut a, &mut b, &mut chunk);
        if chunk.is_empty() {
            break;
        }
        out.push(chunk);
    }
    out
}

/// [`merge`] on every core: the two runs cut into parts of about [`chunk_pairs`] pairs at
/// sampled pairs (a binary search per run and cut), each part merged on one thread into a
/// chunk of the result, a wave of parts at a time (one per thread); after each wave the
/// source chunks the merge has passed are freed where it holds the only reference, so it
/// holds the sources and a wave beside the result, never both runs whole beside it. A
/// relation's merge was one thread's: CSPA httpd's three derived relations merged 3.24 G
/// pairs on at most three threads, 17.6 of its 37.2 s on 32 (13 October 2026). The result
/// is the same pairs at any thread count; only where its chunks end differs.
pub(super) fn merge_parallel<R: Representation>(
    mut a: Arc<Pairs<R>>,
    mut b: Arc<Pairs<R>>,
) -> Pairs<R> {
    let total = a.len() + b.len();
    let size = chunk_pairs();
    let threads = rayon::current_num_threads();
    if total <= 2 * size || threads == 1 {
        return merge(Cursor::<R>::new(a), Cursor::<R>::new(b), total);
    }
    let parts = total.div_ceil(size);
    let (starts_a, starts_b) = (starts(&a), starts(&b));
    // Cuts at sampled pairs of both runs: about 16 a part.
    let step = (total / (parts * 16)).max(1);
    let sample_of = |pairs: &Pairs<R>, starts: &[usize]| -> Vec<R> {
        (step / 2..pairs.len())
            .step_by(step)
            .map(|at| pair_at(pairs, starts, at))
            .collect()
    };
    let mut sample = sample_of(&a, &starts_a);
    sample.extend(sample_of(&b, &starts_b));
    sample.sort_unstable();
    let mut bounds: Vec<R> = (1..parts)
        .map(|i| sample[i * sample.len() / parts])
        .collect();
    bounds.dedup();
    let cuts = |pairs: &Pairs<R>, starts: &[usize]| -> Vec<usize> {
        std::iter::once(0)
            .chain(
                bounds
                    .iter()
                    .map(|&bound| position_of(pairs, starts, bound)),
            )
            .chain(std::iter::once(pairs.len()))
            .collect()
    };
    let (cuts_a, cuts_b) = (cuts(&a, &starts_a), cuts(&b, &starts_b));
    let parts = bounds.len() + 1;
    let mut out = Pairs::<R>::default();
    let mut first = 0;
    while first < parts {
        let end = (first + threads).min(parts);
        let merged: Vec<Vec<R>> = (first..end)
            .into_par_iter()
            .map(|part| {
                let (from_a, to_a) = (cuts_a[part], cuts_a[part + 1]);
                let (from_b, to_b) = (cuts_b[part], cuts_b[part + 1]);
                let mut x = SliceFeed::<R>::new(slices(&a, &starts_a, from_a, to_a));
                let mut y = SliceFeed::<R>::new(slices(&b, &starts_b, from_b, to_b));
                let mut chunk = Vec::with_capacity(to_a - from_a + to_b - from_b);
                fill(&mut x, &mut y, &mut chunk);
                chunk
            })
            .collect();
        for chunk in merged {
            out.push(chunk);
        }
        free_before(&mut a, &starts_a, cuts_a[end]);
        free_before(&mut b, &starts_b, cuts_b[end]);
        first = end;
    }
    out
}

/// Where each chunk of `pairs` starts among its pairs; its length last.
fn starts<R: Representation>(pairs: &Pairs<R>) -> Vec<usize> {
    let mut starts = Vec::with_capacity(pairs.chunks.len() + 1);
    let mut at = 0;
    for chunk in &pairs.chunks {
        starts.push(at);
        at += chunk.len();
    }
    starts.push(at);
    starts
}

/// The pair at position `at`.
fn pair_at<R: Representation>(pairs: &Pairs<R>, starts: &[usize], at: usize) -> R {
    let chunk = starts.partition_point(|&start| start <= at) - 1;
    pairs.chunks[chunk][at - starts[chunk]]
}

/// The first position whose pair isn't below `bound`.
fn position_of<R: Representation>(pairs: &Pairs<R>, starts: &[usize], bound: R) -> usize {
    let chunk = pairs.lasts.partition_point(|&last| last < bound);
    match pairs.chunks.get(chunk) {
        Some(pairs) => starts[chunk] + pairs.partition_point(|&p| p < bound),
        None => pairs.len(),
    }
}

/// The slices of `pairs` at positions `from..to`.
fn slices<'a, R: Representation>(
    pairs: &'a Pairs<R>,
    starts: &[usize],
    from: usize,
    to: usize,
) -> Vec<&'a [R]> {
    let mut out = Vec::new();
    if from >= to {
        return out;
    }
    let mut chunk = starts.partition_point(|&start| start <= from) - 1;
    let mut at = from;
    while at < to {
        let start = starts[chunk];
        let end = (to - start).min(pairs.chunks[chunk].len());
        out.push(&pairs.chunks[chunk][at - start..end]);
        at = start + end;
        chunk += 1;
    }
    out
}

/// Frees the chunks of `pairs` wholly before position `at`, if this holds the only
/// reference to them.
fn free_before<R: Representation>(pairs: &mut Arc<Pairs<R>>, starts: &[usize], at: usize) {
    if let Some(pairs) = Arc::get_mut(pairs) {
        for (chunk, pairs) in pairs.chunks.iter_mut().enumerate() {
            if starts[chunk + 1] > at {
                break;
            }
            *pairs = Vec::new();
        }
    }
}

/// Slices<R> of a run, read in order as a [`Feed<R>`].
struct SliceFeed<'a, R: Representation> {
    slices: Vec<&'a [R]>,
    current: usize,
    at: usize,
}

impl<'a, R: Representation> SliceFeed<'a, R> {
    fn new(slices: Vec<&'a [R]>) -> Self {
        Self {
            slices,
            current: 0,
            at: 0,
        }
    }
}

impl<R: Representation> Feed<R> for SliceFeed<'_, R> {
    fn rest(&mut self) -> &[R] {
        while self
            .slices
            .get(self.current)
            .is_some_and(|slice| self.at == slice.len())
        {
            self.current += 1;
            self.at = 0;
        }
        self.slices
            .get(self.current)
            .map_or(&[], |slice| &slice[self.at..])
    }

    fn read(&mut self, n: usize) {
        self.at += n;
    }
}

/// A finger into [`Pairs<R>`]: over the chunks' last pairs, and into the chunk it is in.
#[derive(Clone, Copy, Default)]
pub(super) struct PairsFinger {
    pub(super) chunk: Finger,
    pub(super) current: usize,
    pub(super) within: Finger,
}

/// A sorted run of pairs, by subject and, where a rule can look the relation up by object
/// alone, by object. Cloning shares the pairs.
#[derive(Default, Clone)]
pub(super) struct Run<R: Representation> {
    pub(super) so: Arc<Pairs<R>>,
    /// `(object, subject)` pairs, if kept.
    pub(super) os: Option<Arc<Pairs<R>>>,
}

/// The `(object, subject)` order of `so`.
pub(super) fn objects_of<R: Representation>(so: &Pairs<R>) -> Pairs<R> {
    let parts = so
        .chunks
        .iter()
        .map(|chunk| (chunk.as_slice(), Marking::None))
        .collect();
    sort_parts(parts, true, true).0
}

impl<R: Representation> Run<R> {
    /// A run of `so` (in chunks: it will be merged), with its `(object, subject)` order if
    /// `objects`.
    pub(super) fn new(so: Pairs<R>, objects: bool) -> Self {
        let os = objects.then(|| Arc::new(objects_of(&so)));
        Self {
            so: Arc::new(so),
            os,
        }
    }

    /// A run of `os` (`(object, subject)` pairs, sorted, deduplicated) as it is: the
    /// input, which is never merged.
    pub(super) fn from_os(os: Vec<R>) -> Self {
        let mut so: Vec<R> = os.iter().map(|&p| R::swapped(p)).collect();
        so.par_sort_unstable();
        Self {
            so: Arc::new(Pairs::<R>::whole(so)),
            os: Some(Arc::new(Pairs::<R>::whole(os))),
        }
    }

    pub(super) fn len(&self) -> usize {
        self.so.len()
    }

    /// The bytes of both orders, spare capacity included.
    pub(super) fn bytes(&self) -> usize {
        self.so.bytes() + self.os.as_ref().map_or(0, |os| os.bytes())
    }

    /// Keeps the `(object, subject)` order if `objects` (built if missing; the input's,
    /// given with spare capacity, made exact), drops it if not.
    pub(super) fn objects(&mut self, objects: bool) {
        if objects && self.os.is_none() && self.len() > 0 {
            self.os = Some(Arc::new(objects_of(&self.so)));
            return;
        }
        match (&mut self.os, objects) {
            (os @ Some(_), false) => *os = None,
            (Some(os), true) => {
                if let Some(pairs) = Arc::get_mut(os)
                    && pairs
                        .chunks
                        .iter()
                        .any(|chunk| chunk.len() < chunk.capacity())
                {
                    let chunks = std::mem::take(&mut pairs.chunks);
                    *pairs = Pairs::<R>::default();
                    for chunk in chunks {
                        pairs.push(chunk.as_slice().to_vec());
                    }
                }
            }
            _ => {}
        }
    }

    pub(super) fn contains(&self, s: u64, o: u64) -> bool {
        self.so.contains(R::pair(s, o))
    }

    /// Calls `f` with every `(s, o)` matching the bound positions.
    pub(super) fn scan(
        &self,
        s: Option<u64>,
        o: Option<u64>,
        missed: &std::sync::atomic::AtomicU64,
        f: &mut dyn FnMut(u64, u64),
    ) {
        match (s, o) {
            (Some(s), Some(o)) => {
                if self.contains(s, o) {
                    f(s, o);
                }
            }
            (Some(s), None) => {
                for &p in self.so.range(s).flatten() {
                    f(s, R::second(p));
                }
            }
            (None, Some(o)) => match &self.os {
                Some(os) => {
                    for &p in os.range(o).flatten() {
                        f(R::second(p), o);
                    }
                }
                // Without the object order: every pair read. Never by the rules that
                // decided it isn't kept; counted where it happens all the same.
                None => {
                    if self.len() > 0 {
                        missed.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    }
                    for &p in self.so.iter() {
                        if R::second(p) == o {
                            f(R::first(p), o);
                        }
                    }
                }
            },
            (None, None) => {
                for &p in self.so.iter() {
                    f(R::first(p), R::second(p));
                }
            }
        }
    }

    /// The pairs matching the bound positions, as slices of one order (a slice per chunk
    /// they are in), with whether that order is `(object, subject)`; `false` if they
    /// aren't one slice per chunk (an object without its order).
    pub(super) fn matching<'a>(
        &'a self,
        s: Option<u64>,
        o: Option<u64>,
        out: &mut Slices<'a, R>,
    ) -> bool {
        match (s, o) {
            (Some(s), Some(o)) => {
                if let Some(chunk) = self.so.chunk_of(R::pair(s, o))
                    && let Ok(at) = chunk.binary_search(&R::pair(s, o))
                {
                    out.push(Slice::<R>::of(&chunk[at..=at], false));
                }
            }
            (Some(s), None) => out.extend(self.so.range(s).map(|p| Slice::<R>::of(p, false))),
            (None, Some(o)) => match &self.os {
                Some(os) => out.extend(os.range(o).map(|p| Slice::<R>::of(p, true))),
                None => return self.len() == 0,
            },
            (None, None) => out.extend(self.so.chunks.iter().map(|p| Slice::<R>::of(p, false))),
        }
        true
    }

    /// Calls `f` with every `(s, o)` matching the bound positions, or `false` where only a
    /// scan of every pair could find them (an object without its
    /// order): [`Self::scan`] without a callback per run or a list of slices, for the
    /// join kernel ([`super::join`]). `marks`, if given, are the run's (the delta's) and
    /// its marked pairs are left out.
    #[inline]
    pub(super) fn each(
        &self,
        s: Option<u64>,
        o: Option<u64>,
        marks: Option<&Marks>,
        finger: &mut RangeFinger,
        f: &mut impl FnMut(u64, u64),
    ) -> bool {
        #[cfg(test)]
        RUN_READS.with(|reads| reads.set(reads.get() + 1));
        if marks.is_some_and(|marks| marks.all) {
            return true;
        }
        let unmarked = |words: Option<&Vec<Vec<u64>>>, chunk: usize, at: usize| {
            words.is_none_or(|words| !marked(&words[chunk], at))
        };
        match (s, o) {
            (Some(s), Some(o)) => {
                if let Some((chunk, at)) = self.so.position(R::pair(s, o))
                    && unmarked(marks.map(|m| &m.so), chunk, at)
                {
                    f(s, o);
                }
            }
            (Some(s), None) => {
                for (chunk, start, pairs) in self.so.range_from(s, finger) {
                    for (i, &q) in pairs.iter().enumerate() {
                        let o = R::second(q);
                        if unmarked(marks.map(|m| &m.so), chunk, start + i) {
                            f(s, o);
                        }
                    }
                }
            }
            (None, Some(o)) => {
                let Some(os) = &self.os else {
                    return self.len() == 0;
                };
                let words = match marks {
                    Some(marks) => match &marks.os {
                        Some(words) => Some(words),
                        None => return self.len() == 0,
                    },
                    None => None,
                };
                for (chunk, start, pairs) in os.range_from(o, finger) {
                    for (i, &q) in pairs.iter().enumerate() {
                        let s = R::second(q);
                        if unmarked(words, chunk, start + i) {
                            f(s, o);
                        }
                    }
                }
            }
            (None, None) => {
                for (chunk, pairs) in self.so.chunks.iter().enumerate() {
                    for (i, &q) in pairs.iter().enumerate() {
                        let (s, o) = (R::first(q), R::second(q));
                        if unmarked(marks.map(|m| &m.so), chunk, i) {
                            f(s, o);
                        }
                    }
                }
            }
        }
        true
    }

    /// [`Run::<R>::matching`] without the pairs `marks` marks, as slices that carry their
    /// chunk's marks ([`Slice::<R>::skip`]); `false` where [`Run::<R>::matching`] is.
    pub(super) fn matching_unmarked<'a>(
        &'a self,
        s: Option<u64>,
        o: Option<u64>,
        marks: &'a Marks,
        out: &mut Slices<'a, R>,
    ) -> bool {
        if marks.all {
            return true;
        }
        let slice = |pairs, swapped, words: &'a [u64], start| Slice {
            pairs,
            swapped,
            skip: Some((words, start)),
        };
        match (s, o) {
            (Some(s), Some(o)) => {
                if let Some((c, i)) = self.so.position(R::pair(s, o))
                    && !marked(&marks.so[c], i)
                {
                    out.push(Slice::<R>::of(&self.so.chunks[c][i..=i], false));
                }
            }
            (Some(s), None) => out.extend(
                self.so
                    .range_at(s)
                    .map(|(c, start, p)| slice(p, false, &marks.so[c][..], start)),
            ),
            (None, Some(o)) => match (&self.os, &marks.os) {
                (Some(os), Some(os_marks)) => out.extend(
                    os.range_at(o)
                        .map(|(c, start, p)| slice(p, true, &os_marks[c][..], start)),
                ),
                _ => return self.len() == 0,
            },
            (None, None) => out.extend(
                self.so
                    .chunks
                    .iter()
                    .zip(&marks.so)
                    .map(|(p, words)| slice(p, false, &words[..], 0)),
            ),
        }
        true
    }

    /// [`Run::<R>::scan`] without the pairs `marks` marks.
    pub(super) fn scan_unmarked(
        &self,
        s: Option<u64>,
        o: Option<u64>,
        marks: &Marks,
        missed: &std::sync::atomic::AtomicU64,
        f: &mut dyn FnMut(u64, u64),
    ) {
        if marks.all {
            return;
        }
        let mut slices = Slices::<R>::new();
        if self.matching_unmarked(s, o, marks, &mut slices) {
            for slice in &slices {
                slice.for_each(|p| match slice.swapped {
                    true => f(R::second(p), R::first(p)),
                    false => f(R::first(p), R::second(p)),
                });
            }
            return;
        }
        // By object without the object order: every pair read.
        let o = o.expect("only lookups by object alone have no slices");
        if self.len() > 0 {
            missed.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
        for (chunk, words) in self.so.chunks.iter().zip(&marks.so) {
            for (i, &p) in chunk.iter().enumerate() {
                let (s, other) = (R::first(p), R::second(p));
                if other == o && !marked(words, i) {
                    f(s, o);
                }
            }
        }
    }

    pub(super) fn estimate(&self, s: Option<u64>, o: Option<u64>) -> usize {
        match (s, o) {
            (Some(s), Some(o)) => usize::from(self.contains(s, o)),
            (Some(s), None) => self.so.range(s).map(<[R]>::len).sum(),
            (None, Some(o)) => match &self.os {
                Some(os) => os.range(o).map(<[R]>::len).sum(),
                None => self.so.len(),
            },
            (None, None) => self.so.len(),
        }
    }

    /// The union with a disjoint run, one order at a time, so the sources and the result
    /// never coexist in both; a source this holds the only reference to is freed as it is
    /// merged. Adds the pairs it writes (both orders) to `written`.
    pub(super) fn merge(self, other: Run<R>, written: &mut usize) -> Run<R> {
        if self.len() == 0 {
            return other;
        }
        if other.len() == 0 {
            return self;
        }
        let two = |a: Arc<Pairs<R>>, b: Arc<Pairs<R>>| Arc::new(merge_parallel(a, b));
        let so = two(self.so, other.so);
        let os = match (self.os, other.os) {
            (Some(a), Some(b)) => Some(two(a, b)),
            _ => None,
        };
        *written += so.len() + os.as_ref().map_or(0, |os| os.len());
        Run { so, os }
    }
}

/// A slice of a run matching a pattern.
#[derive(Clone, Copy)]
pub(super) struct Slice<'a, R: Representation> {
    pub(super) pairs: &'a [R],
    /// Whether its order is `(object, subject)`.
    pub(super) swapped: bool,
    /// The marks of its chunk and where the slice starts in it: marked pairs are skipped
    /// ([`Marks`]).
    pub(super) skip: Option<(&'a [u64], usize)>,
}

impl<'a, R: Representation> Slice<'a, R> {
    pub(super) fn of(pairs: &'a [R], swapped: bool) -> Self {
        Self {
            pairs,
            swapped,
            skip: None,
        }
    }

    /// Calls `f` with each pair of `range` (positions in the slice) not skipped, as stored.
    pub(super) fn for_each_in(&self, range: std::ops::Range<usize>, mut f: impl FnMut(R)) {
        match self.skip {
            None => self.pairs[range].iter().for_each(|&pair| f(pair)),
            Some((words, start)) => {
                for i in range {
                    if !marked(words, start + i) {
                        f(self.pairs[i]);
                    }
                }
            }
        }
    }

    pub(super) fn for_each(&self, f: impl FnMut(R)) {
        self.for_each_in(0..self.pairs.len(), f);
    }
}

/// Slices<R> of runs matching a pattern, in the order their runs are read.
pub(super) type Slices<'a, R> = smallvec::SmallVec<[Slice<'a, R>; 4]>;

/// The delta of `pairs` (sorted, disjoint from the relation), some of them marked by
/// `marks` (its subject order's, from [`sort_marked`]), with its `(object, subject)` order
/// and that order's marks if `objects`.
pub(super) fn marked_run<R: Representation>(
    pairs: Pairs<R>,
    marks: Option<Marks>,
    objects: bool,
) -> (Run<R>, Option<Marks>) {
    let Some(mut marks) = marks else {
        return (Run::<R>::new(pairs, objects), None);
    };
    if marks.all || !objects {
        return (Run::<R>::new(pairs, objects), Some(marks));
    }
    let parts = pairs
        .chunks
        .iter()
        .zip(&marks.so)
        .map(|(chunk, words)| (chunk.as_slice(), Marking::Bits(words)))
        .collect();
    let (os, os_marks) = sort_parts(parts, true, true);
    marks.os = os_marks;
    let run = Run {
        so: Arc::new(pairs),
        os: Some(Arc::new(os)),
    };
    (run, Some(marks))
}

#[cfg(test)]
mod chunk_tests {
    use super::*;

    /// The pairs whose first component is `key`.
    fn range<R: Representation>(pairs: &[R], key: u64) -> &[R] {
        let start = pairs.partition_point(|&p| R::first(p) < key);
        let len = pairs[start..].partition_point(|&p| R::first(p) == key);
        &pairs[start..start + len]
    }

    /// Pseudo-random pairs over a small range, so that chunks repeat keys at their ends.
    fn pairs<R: Representation>(seed: u64, n: usize) -> Vec<R> {
        let mut x = seed;
        (0..n)
            .map(|_| {
                x = x
                    .wrapping_mul(6364136223846793005)
                    .wrapping_add(1442695040888963407);
                R::pair((x >> 33) % 23, (x >> 13) % 41)
            })
            .collect()
    }

    fn sorted<R: Representation>(mut pairs: Vec<R>) -> Vec<R> {
        pairs.sort_unstable();
        pairs.dedup();
        pairs
    }

    /// Chunked runs sort, merge and answer like one sorted vector, at chunk boundaries
    /// too (in unit tests a chunk holds 16 pairs).
    fn chunked_runs_answer_as_one_sorted_vector<R: Representation>() {
        for seed in 0..40 {
            let parts: Vec<Vec<R>> = (0..1 + seed as usize % 7)
                .map(|i| sorted(pairs::<R>(seed * 31 + i as u64, 5 + 37 * i)))
                .collect();
            let all = sorted(parts.concat());
            let chunked = sort_chunked(parts.clone(), false);
            assert_eq!(chunked.iter().copied().collect::<Vec<_>>(), all);
            assert!(chunked.chunks.iter().all(|c| c.len() == c.capacity()));
            let objects = sort_chunked(parts, true);
            let expected = sorted(all.iter().map(|&p| R::swapped(p)).collect());
            assert_eq!(objects.iter().copied().collect::<Vec<_>>(), expected);
            for key in 0..24 {
                let found: Vec<R> = chunked.range(key).flatten().copied().collect();
                assert_eq!(found, range(&all, key), "range {key}");
            }
            for pair in pairs::<R>(seed + 1000, 50) {
                assert_eq!(chunked.contains(pair), all.binary_search(&pair).is_ok());
            }
            // A finger answers the same over sorted probes (across chunks), and over
            // probes in any order.
            let mut finger = PairsFinger::default();
            for pair in sorted(pairs::<R>(seed + 2000, 60)) {
                assert_eq!(
                    chunked.contains_from(&mut finger, pair),
                    all.binary_search(&pair).is_ok()
                );
            }
            for pair in pairs::<R>(seed + 3000, 40) {
                assert_eq!(
                    chunked.contains_from(&mut finger, pair),
                    all.binary_search(&pair).is_ok()
                );
            }
            // A merge of disjoint runs, one shared (kept) and one owned (freed).
            let (left, right): (Vec<R>, Vec<R>) = all
                .iter()
                .partition(|&&p| (R::first(p) + R::second(p)).is_multiple_of(3));
            let shared = Arc::new(Pairs::<R>::chunked(left.clone()));
            let owned = Arc::new(sort_chunked(vec![right.clone()], false));
            let merged = merge(
                Cursor::<R>::new(shared.clone()),
                Cursor::<R>::new(owned),
                all.len(),
            );
            assert_eq!(merged.iter().copied().collect::<Vec<_>>(), all);
            assert_eq!(merged.len(), all.len());
            assert_eq!(shared.iter().copied().collect::<Vec<_>>(), left);
            // A sort that marks one side's pairs (also those on both sides), and the other
            // order of its result, marked from the first.
            for (open, closed) in [(left.clone(), right.clone()), (all.clone(), right.clone())] {
                let (sorted, marks) =
                    sort_marked(vec![(open, Marking::None), (closed, Marking::All)], false);
                let marks = marks.expect("marked");
                assert_eq!(sorted.iter().copied().collect::<Vec<_>>(), all);
                for (chunk, words) in sorted.chunks.iter().zip(&marks) {
                    assert_eq!(chunk.len(), chunk.capacity());
                    for (i, pair) in chunk.iter().enumerate() {
                        assert_eq!(marked(words, i), right.binary_search(pair).is_ok());
                    }
                }
                let parts = sorted
                    .chunks
                    .iter()
                    .zip(&marks)
                    .map(|(chunk, words)| (chunk.as_slice(), Marking::Bits(words)))
                    .collect();
                let (objects, object_marks) = sort_marked(parts, true);
                for (chunk, words) in objects.chunks.iter().zip(&object_marks.expect("marked")) {
                    for (i, &p) in chunk.iter().enumerate() {
                        assert_eq!(
                            marked(words, i),
                            right.binary_search(&R::swapped(p)).is_ok()
                        );
                    }
                }
            }
        }
    }

    /// Guard: ascending lookups through one finger search from the root only when they
    /// leave a chunk; the others gallop from the last key's pairs. Lookups through one
    /// finger answer as searches from the root, for keys ascending, repeated and
    /// descending, across chunk boundaries (16 pairs a chunk in unit tests).
    fn lookups_from_a_finger_equal_lookups_from_the_root<R: Representation>() {
        for seed in 0..20u64 {
            let all = sorted(pairs::<R>(seed * 17 + 5, 100 + 60 * seed as usize));
            let run = Pairs::<R>::chunked(all.clone());
            let mut keys: Vec<u64> = sorted(pairs::<R>(seed + 77, 40))
                .iter()
                .map(|&p| R::first(p))
                .collect();
            let mut finger = RangeFinger::default();
            for &key in &keys {
                run.range_from(key, &mut finger).for_each(drop);
            }
            assert!(
                finger.roots as usize <= run.chunks.len(),
                "seed {seed}: {} searches from the root over {} chunks",
                finger.roots,
                run.chunks.len()
            );
            keys.extend([3, 3, 0, 22, 23, 24, 1]);
            let mut finger = RangeFinger::default();
            for key in keys {
                let found: Vec<R> = run
                    .range_from(key, &mut finger)
                    .flat_map(|(_, _, pairs)| pairs.iter().copied())
                    .collect();
                assert_eq!(found, range(&all, key), "seed {seed} key {key}");
            }
        }
    }

    /// A run's object order, sorted stably by object from its subject order, equals a
    /// comparison sort's, over objects that are dense, sparse and all one (unit tests'
    /// chunks hold 16 pairs; the stable sort starts at 256).
    fn a_runs_object_order_equals_a_sorted_copy<R: Representation>() {
        for (seed, objects) in [(1u64, 900u64), (2, 1 << 20), (3, 1), (4, 1 << 31)] {
            let mut x = seed;
            let mut all: Vec<R> = (0..3000)
                .map(|_| {
                    x = x
                        .wrapping_mul(6364136223846793005)
                        .wrapping_add(1442695040888963407);
                    R::pair((x >> 40) % 700, 1 + (x >> 7) % objects)
                })
                .collect();
            all.sort_unstable();
            all.dedup();
            let expected = sorted(all.iter().map(|&p| R::swapped(p)).collect());
            let whole: Vec<R> = sort_by_first(all.iter().map(|&p| R::swapped(p)).collect());
            assert_eq!(whole, expected, "objects below {objects}");
            let run = Pairs::<R>::chunked(all.clone());
            let objects = objects_of(&run);
            assert_eq!(objects.iter().copied().collect::<Vec<_>>(), expected);
        }
    }

    /// Guard: a large delta's chunk of objects (dense numbers) is sorted by one counting
    /// pass, not by comparisons (about 20 reads of the chunk) or passes of 11 bits; sparse
    /// numbers take passes of 11 bits.
    #[test]
    fn a_dense_chunk_of_objects_takes_one_pass() {
        let passes = |pairs: usize, spread: u64| {
            let (bits, width) = digits(pairs, spread);
            bits.div_ceil(width.max(1))
        };
        // A full chunk of 2^21 pairs over 3 M objects, and one of 600 k over 1 M.
        assert_eq!(passes(1 << 21, 3_000_000), 1);
        assert_eq!(passes(600_000, 1_000_000), 1);
        assert_eq!(passes(1_000, 1 << 31), 3);
        assert_eq!(passes(1_000, 0), 0);
    }

    /// A merge in parallel parts holds the pairs of the sequential one at any thread count
    /// (in unit tests a chunk holds 16 pairs, so the runs have many parts), frees an owned
    /// source and leaves a shared one whole.
    fn a_parallel_merge_equals_the_sequential_one<R: Representation>() {
        for seed in 0..12u64 {
            let all = sorted(pairs::<R>(seed * 7 + 1, 200 + 90 * seed as usize));
            let (left, right): (Vec<R>, Vec<R>) = all
                .iter()
                .partition(|&&p| (R::first(p) * 7 + R::second(p) + seed).is_multiple_of(3));
            for threads in [1, 3, 8] {
                let pool = rayon::ThreadPoolBuilder::new()
                    .num_threads(threads)
                    .build()
                    .unwrap();
                let shared = Arc::new(Pairs::<R>::chunked(left.clone()));
                let owned = Arc::new(sort_chunked(vec![right.clone()], false));
                let merged = pool.install(|| merge_parallel(shared.clone(), owned));
                assert_eq!(
                    merged.iter().copied().collect::<Vec<_>>(),
                    all,
                    "{threads} threads"
                );
                assert_eq!(merged.len(), all.len());
                assert!(
                    merged
                        .chunks
                        .iter()
                        .all(|c| !c.is_empty() && c.len() == c.capacity())
                );
                assert_eq!(shared.iter().copied().collect::<Vec<_>>(), left);
                for pair in pairs::<R>(seed + 500, 40) {
                    assert_eq!(merged.contains(pair), all.binary_search(&pair).is_ok());
                }
            }
        }
    }

    impl<R: Representation> Relation<R> {
        fn advance_with(&mut self, (pairs, marks): New<R>) -> usize {
            self.advance(pairs, marks)
        }
    }

    /// Guard: a lookup calls no run's walk for a run without pairs (a derived relation's
    /// input, mostly): a call with its prologue per run per lookup, five runs with the
    /// tiered layout, was a tenth of the join kernel's enumeration (CSPA httpd's subset,
    /// 13 October 2026).
    fn lookups_read_only_the_runs_that_hold_pairs<R: Representation>() {
        let mut relation = Relation::<R>::default();
        let batch = |from: u64| -> Vec<R> { (0..40).map(|i| R::pair(i % 7, from + i)).collect() };
        for from in [0, 100, 200] {
            let mut pairs = batch(from);
            pairs.sort_unstable();
            relation.advance_with(new_pairs(vec![pairs], Vec::new()));
        }
        // No input, the first two batches added, the third the delta.
        assert_eq!(relation.input.len(), 0);
        let runs = 1 + relation.added.len();
        RUN_READS.with(|reads| reads.set(0));
        let mut fingers = [RangeFinger::default(); RUNS];
        let mut found = 0;
        assert!(
            relation.each(Some(3), None, Seg::All, &mut fingers, &mut |_, _| found +=
                1)
        );
        assert_eq!(found, 18);
        assert_eq!(RUN_READS.with(std::cell::Cell::get), runs as u64);
    }

    /// Guard: a probe searches only the runs whose first-to-last range holds its pair, so
    /// neither a derived relation's empty input nor a run past it: the tiered layout's
    /// extra runs raised CSPA httpd's probes from 5.5 to 6.9 thread seconds (Khaleesi,
    /// 13 October 2026).
    fn probes_search_only_the_runs_that_span_them<R: Representation>() {
        let mut relation = Relation::<R>::default();
        for from in [0, 100, 200] {
            let mut pairs: Vec<R> = (0..40).map(|i| R::pair(i % 7, from + i)).collect();
            pairs.sort_unstable();
            relation.advance_with(new_pairs(vec![pairs], Vec::new()));
        }
        assert_eq!(relation.input.len(), 0);
        let probe = |p: R| {
            PROBE_READS.with(|reads| reads.set(0));
            let mut fingers = [PairsFinger::default(); RUNS];
            let found = relation.contains_from(p, &mut fingers);
            (found, PROBE_READS.with(std::cell::Cell::get))
        };
        // In the added run, which comes before the delta: one search.
        assert_eq!(probe(R::pair(3, 103)), (true, 1));
        // Past every run: none.
        assert_eq!(probe(R::pair(9, 0)), (false, 0));
    }

    /// Guard: over rounds shaped like CSPA httpd's (a few large deltas among many small
    /// ones), the merges write each added pair about three times in its order, where the
    /// recent run folded at a quarter of the base wrote it about six times (the full
    /// graph: 3.24 G merged pairs for 324.5 M facts, 13 October 2026).
    fn added_pairs_are_written_about_three_times<R: Representation>() {
        // Thousandths of the full graph's per-round candidates (its 32 rounds).
        let deltas = [
            1592u64, 4320, 9110, 7409, 18350, 18195, 86477, 34590, 26194, 48650, 76927, 15134,
            9010, 4158, 20862, 7263, 2188, 1402, 5233, 1143, 20, 10, 49, 17, 9, 3, 21, 5, 1, 1, 1,
        ];
        let mut relation = Relation::<R>::default();
        relation.objects(false);
        let (mut written, mut added, mut next) = (0usize, 0usize, 0u64);
        for &n in &deltas {
            let pairs: Vec<R> = (next..next + n).map(|i| R::pair(i % 4093, i)).collect();
            next += n;
            let mut pairs = pairs;
            pairs.sort_unstable();
            written += relation.advance_with(new_pairs(vec![pairs], Vec::new()));
            added += n as usize;
        }
        assert!(
            written * 10 <= added * 35,
            "{written} pairs written for {added} added: {:.2} a pair",
            written as f64 / added as f64
        );
    }

    /// R13's layout: a closed family's part of the delta is marked in the one delta run
    /// ([`Marks`]): it reads as the delta, the family's own reads skip it, and it costs the
    /// merges nothing over the same rounds with every pair open. As a run of its own it
    /// cost each fold into an empty base a copy, and every lookup a fourth run (LUBM 100:
    /// the merges wrote 19.4 M pairs, 12.5 M with every pair open; 18.5 M point lookups for
    /// 12.9 M).
    fn a_closed_part_reads_as_the_delta_and_costs_nothing<R: Representation>() {
        let mut x = 7u64;
        let mut next = move || {
            x = x
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            x >> 33
        };
        let (mut split, mut open) = (Relation::<R>::default(), Relation::<R>::default());
        split.closed_by = Some(Family::Schema(1));
        let (mut written, mut plain) = (0, 0);
        let mut known = std::collections::BTreeSet::new();
        let sorted = |mut pairs: Vec<R>| {
            pairs.sort_unstable();
            pairs
        };
        for round in 0..14u64 {
            let fresh: Vec<R> = (0..40 + round * 30)
                .map(|_| R::pair(next() % 61, next() % 53))
                .filter(|&pair| known.insert(pair))
                .collect();
            let fresh = sorted(fresh);
            // A third of the pairs closed; every pair in some rounds, none in others.
            let (closed, rest): (Vec<R>, Vec<R>) = fresh.iter().partition(|&&p| match round % 5 {
                3 => true,
                4 => false,
                _ => (R::first(p) + R::second(p)).is_multiple_of(3),
            });
            let closed_set: std::collections::BTreeSet<R> = closed.iter().copied().collect();
            written += split.advance_with(new_pairs(vec![rest], vec![closed]));
            plain += open.advance_with(new_pairs(vec![fresh], Vec::new()));
            if round % 2 == 1 {
                // An object order dropped and built again: the marks follow it.
                for relation in [&mut split, &mut open] {
                    relation.objects(false);
                    relation.objects(true);
                }
            }
            for (s, o) in [
                (None, None),
                (Some(3), None),
                (None, Some(5)),
                (Some(3), Some(5)),
                (Some(3), Some(6)),
            ] {
                let read = |relation: &Relation<R>, seg: Seg| {
                    let mut out = Vec::new();
                    relation.scan(s, o, seg, &mut |s, o| out.push(R::pair(s, o)));
                    sorted(out)
                };
                // As morsels read it: the slices, their skipped pairs left out.
                let sliced = |relation: &Relation<R>, seg: Seg| {
                    let mut out = Vec::new();
                    for slice in relation.matching(s, o, seg).expect("slices") {
                        slice.for_each(&mut |p: R| {
                            let (a, b) = (R::first(p), R::second(p));
                            let (s, o) = if slice.swapped { (b, a) } else { (a, b) };
                            out.push(R::pair(s, o));
                        });
                    }
                    sorted(out)
                };
                for seg in [
                    Seg::Delta,
                    Seg::Old,
                    Seg::All,
                    Seg::DeltaNotBy(Family::Schema(2)),
                ] {
                    assert_eq!(read(&split, seg), read(&open, seg), "round {round} {seg:?}");
                    assert_eq!(
                        sliced(&split, seg),
                        read(&open, seg),
                        "round {round} {seg:?}"
                    );
                }
                let mut own = read(&open, Seg::Delta);
                own.retain(|pair| !closed_set.contains(pair));
                assert_eq!(
                    read(&split, Seg::DeltaNotBy(Family::Schema(1))),
                    own,
                    "round {round}"
                );
                assert_eq!(
                    sliced(&split, Seg::DeltaNotBy(Family::Schema(1))),
                    own,
                    "round {round}"
                );
            }
            for s in 0..61 {
                for o in 0..53 {
                    assert_eq!(split.contains(s, o), known.contains(&R::pair(s, o)));
                }
            }
        }
        assert_eq!(
            written, plain,
            "pairs the merges wrote, with a closed part and without"
        );
    }
    #[test]
    fn both_static_layouts_share_all_run_guards() {
        chunked_runs_answer_as_one_sorted_vector::<u64>();
        chunked_runs_answer_as_one_sorted_vector::<[u64; 2]>();
        lookups_from_a_finger_equal_lookups_from_the_root::<u64>();
        lookups_from_a_finger_equal_lookups_from_the_root::<[u64; 2]>();
        a_runs_object_order_equals_a_sorted_copy::<u64>();
        a_runs_object_order_equals_a_sorted_copy::<[u64; 2]>();
        a_parallel_merge_equals_the_sequential_one::<u64>();
        a_parallel_merge_equals_the_sequential_one::<[u64; 2]>();
        lookups_read_only_the_runs_that_hold_pairs::<u64>();
        lookups_read_only_the_runs_that_hold_pairs::<[u64; 2]>();
        probes_search_only_the_runs_that_span_them::<u64>();
        probes_search_only_the_runs_that_span_them::<[u64; 2]>();
        added_pairs_are_written_about_three_times::<u64>();
        added_pairs_are_written_about_three_times::<[u64; 2]>();
        a_closed_part_reads_as_the_delta_and_costs_nothing::<u64>();
        a_closed_part_reads_as_the_delta_and_costs_nothing::<[u64; 2]>();
    }
}
