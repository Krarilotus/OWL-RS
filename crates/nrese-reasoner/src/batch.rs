//! The batch executor (reasoner-v2 design §4.1): full materialisation by semi-naive
//! evaluation over a vertically partitioned working set.
//!
//! - **Working set.** One `Relation` per predicate: its pairs sorted by subject and by
//!   object, plus the last round's delta in both orders. Runs that are merged are held
//!   in chunks, so a merge never holds its sources beside its result (P1-F10), and a
//!   round's candidates stay in their morsels' lists until they are merged (P1-F9).
//! - **Schema grounding** (design §3.2, [`super::eval::ground`]). Atoms over the TBox
//!   vocabulary are evaluated against the current facts and substituted into the rest of
//!   the rule. So `cax-sco` becomes one `(?x type C1) -> (?x type C2)` rule per subclass
//!   edge, and almost every atom gets a constant predicate. When a round derives schema
//!   facts, the rules are grounded again through those facts only, and the new instances
//!   are evaluated once over all facts. That keeps specialisation exact when instance
//!   rules feed the schema (punning, class `sameAs`).
//! - **Semi-naive evaluation.** A rule with n atoms runs n variants per round: atom i over
//!   the delta, atoms before it over the old facts and atoms after it over all facts.
//!   Each variant is driven by its delta atom's matches, split into morsels that run in
//!   parallel; the other atoms are index lookups, ordered by how bound they are.
//! - **Transitive module** (design §4.4): transitivity rules are replaced by a closure by
//!   SCC condensation, which is O(k²) on a k-clique where the rule's joins are O(k³).
//! - **Deduplication** is a sort per round, then a merge into each relation. There's no
//!   shared hash set, and the result doesn't depend on the thread count.
//!
//! The naive evaluator (`naive`, tests and the `oracle` feature only) is the oracle: both compute the same closure.

use std::sync::Arc;

use hashbrown::{HashMap, HashSet};
use rayon::prelude::*;

use super::delta::Interrupted;
pub use super::eval::Schema;
use super::eval::{
    AllFacts, GroundProgram, Job, MORSEL, NEVER, OperationStop, Poller, Probe, Probes, Seg, Source,
    Stopped, Visit, Visits, guards_hold, run_jobs_by_morsel,
};
use super::ir::Triple;
use super::ir::{Guard, Head, Rule, SharedClasses, Term};
use super::lists::{ListVocabulary, instantiate};
use super::representatives::{EqualityClasses, Guarded, over_classes};
use super::violations::{Violations, unlimited};
use nrese_exec::Budget;
use nrese_exec::heap;
use nrese_exec::memory::MemoryStopCause;
use nrese_exec::search::Finger;
use nrese_exec::{BudgetExceeded, ChargedVec};

type Pair = (u64, u64);

/// Facts split by predicate: each predicate once, in order, with its `(subject, object)`
/// pairs sorted and distinct, each list at its exact size. A round's candidates are held
/// this way, a list per morsel: 16 bytes a fact instead of 24, and never concatenated.
type Segments = Vec<(u64, Vec<Pair>)>;

/// A relation's new pairs, in its subject order, with the marks of those a closed family
/// produced ([`Marks`]).
type New = (Pairs, Option<Marks>);

/// The new pairs of the lists `open` and `closed` (each sorted and distinct), those of
/// `closed` marked: what a closed family produced.
fn new_pairs(open: Vec<Vec<Pair>>, mut closed: Vec<Vec<Pair>>) -> New {
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

/// Candidate facts with the closed rule family that produced them, if one did (its schema
/// link; see [`Relation::closed_by`]).
type Candidates = (Option<u64>, Segments);

/// `facts`, sorted by (predicate, subject, object) and distinct, as [`Segments`].
fn split(facts: Vec<Triple>) -> Segments {
    facts
        .chunk_by(|a, b| a[1] == b[1])
        .map(|chunk| (chunk[0][1], chunk.iter().map(|&[s, _, o]| (s, o)).collect()))
        .collect()
}

/// Any `facts` as [`Segments`].
fn segments(mut facts: Vec<Triple>) -> Segments {
    facts.par_sort_unstable_by_key(|&[s, p, o]| (p, s, o));
    facts.dedup();
    split(facts)
}

/// The union of sorted, distinct pair lists, in chunks (see [`sort_chunked`]).
fn union(mut parts: Vec<Vec<Pair>>) -> Pairs {
    if parts.len() == 1 {
        return Pairs::chunked(parts.pop().expect("one part"));
    }
    sort_chunked(parts, false)
}

/// The pairs of `parts` (each read as `(b, a)` if `swap`), sorted and distinct, in chunks
/// of at most about [`chunk_pairs`] pairs, without ever holding them in one vector: a sample of
/// them gives the chunks' bounds, each part is split by the bounds (and freed, if it is
/// owned), and each chunk's pieces are sorted together. So the parts and their pieces
/// are what is held at once, not the parts beside a sorted copy of all of them (at
/// LUBM 1000, a round's `rdf:type` candidates, 2.2 times its new facts: 1 GB).
fn sort_chunked<P: AsRef<[Pair]> + Send>(parts: Vec<P>, swap: bool) -> Pairs {
    let parts = parts
        .into_iter()
        .map(|part| (part, Marking::None))
        .collect();
    sort_marked(parts, swap).0
}

/// How the pairs of a part of [`sort_marked`] are marked ([`Marks`]).
#[derive(Clone, Copy)]
enum Marking<'a> {
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
fn sort_marked<P: AsRef<[Pair]> + Send>(
    parts: Vec<(P, Marking<'_>)>,
    swap: bool,
) -> (Pairs, Option<Vec<Vec<u64>>>) {
    let read = |&(a, b): &Pair| if swap { (b, a) } else { (a, b) };
    let total: usize = parts.iter().map(|(part, _)| part.as_ref().len()).sum();
    let any_marked = parts
        .iter()
        .any(|(part, marking)| !matches!(marking, Marking::None) && !part.as_ref().is_empty());
    let chunks = total.div_ceil(chunk_pairs()).max(1);
    // The bounds: chunk i takes the pairs from bounds[i - 1] up to bounds[i].
    let bounds: Vec<Pair> = if chunks > 1 {
        let step = (total / (chunks * 64)).max(1);
        let mut sample: Vec<Pair> = Vec::with_capacity(total / step + 1);
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
    let chunk_of = |pair: &Pair| bounds.partition_point(|bound| bound <= pair);
    // Each part split by the bounds (and by its marks), at the pieces' exact sizes.
    type Pieces = Vec<(usize, bool, Vec<Pair>)>;
    let pieces: Vec<Pieces> = parts
        .into_par_iter()
        .map(|(part, marking)| {
            let pairs = part.as_ref();
            // A loop per marking, so that none tests it per pair.
            match marking {
                Marking::None => split_part(pairs, chunks, &chunk_of, read, |_| false),
                Marking::All => split_part(pairs, chunks, &chunk_of, read, |_| true),
                Marking::Bits(words) => {
                    split_part(pairs, chunks, &chunk_of, read, |i| marked(words, i))
                }
            }
        })
        .collect();
    let mut by_chunk: Vec<[Vec<Vec<Pair>>; 2]> =
        (0..chunks).map(|_| [Vec::new(), Vec::new()]).collect();
    for (chunk, is_marked, piece) in pieces.into_iter().flatten() {
        by_chunk[chunk][usize::from(is_marked)].push(piece);
    }
    let gather = |pieces: Vec<Vec<Pair>>| {
        let mut chunk = Vec::with_capacity(pieces.iter().map(Vec::len).sum());
        for piece in pieces {
            chunk.extend_from_slice(&piece);
        }
        // In parallel too: few chunks may come at once (OWL2Bench RL-1's modules hand
        // over one list of 1.3 M pairs: two chunks, two threads, 13 % slower).
        chunk.par_sort_unstable();
        chunk.dedup();
        chunk
    };
    let sorted: Vec<(Vec<Pair>, Option<Vec<u64>>)> = by_chunk
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
    let mut out = Pairs::default();
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

/// The pieces of `pairs` (each read by `read`) per chunk of [`sort_marked`] and per mark:
/// `(chunk, marked, pairs)`, at their exact sizes.
fn split_part(
    pairs: &[Pair],
    chunks: usize,
    chunk_of: &impl Fn(&Pair) -> usize,
    read: impl Fn(&Pair) -> Pair,
    is_marked: impl Fn(usize) -> bool,
) -> Vec<(usize, bool, Vec<Pair>)> {
    let slot = |i: usize, pair: &Pair| 2 * chunk_of(pair) + usize::from(is_marked(i));
    let mut counts = vec![0; 2 * chunks];
    for (i, pair) in pairs.iter().enumerate() {
        counts[slot(i, &read(pair))] += 1;
    }
    let mut pieces: Vec<Vec<Pair>> = counts.iter().map(|&n| Vec::with_capacity(n)).collect();
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
fn merge_marked(open: &[Pair], marked: &[Pair]) -> (Vec<Pair>, Vec<u64>) {
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

/// The result of [`materialise`].
#[derive(Debug, Default)]
pub struct Materialisation {
    /// Facts derived beyond the input, sorted, without duplicates.
    pub derived: Vec<Triple>,
    /// Consistency violations on the closure, sorted, charged to the run's budget.
    pub violations: Violations,
    pub diagnostics: Vec<super::lists::ListDiagnostic>,
    pub rounds: usize,
    /// Rules after grounding, at the end.
    pub ground_rules: usize,
    /// Predicates closed by the transitive module.
    pub transitive: usize,
    /// Time per phase: grounding, rule joins, modules, merging, consistency.
    pub phases: Phases,
    /// What the rounds held and did, in counts.
    pub counters: Counters,
    /// With equality by representatives ([`materialise_representatives_until`]): the
    /// `owl:sameAs` classes, and in how many rounds (the start included) they merged.
    pub classes: EqualityClasses,
    pub merges: usize,
    /// With equality by representatives: a class holds `rdf:nil`, so every list through it
    /// read as empty, where the replacement rules keep a list cell's members. The closure
    /// isn't the replicated one's; the caller computes it by replication.
    pub nil_merged: bool,
}

/// A materialisation's input: facts, or facts grouped as the store streams them (each
/// predicate once, its `(object, subject)` pairs sorted and distinct).
pub enum Input {
    Facts(Vec<Triple>),
    Grouped(Vec<(u64, Vec<(u64, u64)>)>),
}

/// What [`materialise_representatives_until`] lists as [`Materialisation::derived`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Listing {
    /// Every fact of the closure over representatives, the rewritten input included.
    Representatives,
    /// The closure over representatives as an inferred stack keeps it beside the input:
    /// the facts not in the input, and those of the input that mention a class.
    Stored,
    /// The closure expanded to every identity, without the input's facts: what the
    /// replacement rules derive.
    Expanded,
}

/// Deterministic counts of a materialisation's rounds, for the guards of
/// `docs/design/performance.md` §0: they don't depend on the machine or the thread count.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Counters {
    /// Per round: the working set's bytes after it.
    pub store_bytes: Vec<StoreBytes>,
    /// Bytes of driver matches the rule jobs copied out of the working set before they
    /// ran (P1-F1: none, they read the runs in place).
    pub driver_bytes_copied: usize,
    /// Per round: how often the rule jobs' candidates were checked against the working
    /// set (P1-F8: once per distinct candidate of a morsel).
    pub probes: Vec<u64>,
    /// Checks that came out of (predicate, subject, object) order within their morsel
    /// (P1-F8: none).
    pub unordered_probes: u64,
    /// Where the checks started a relation's runs from the beginning: once per relation
    /// and morsel, the other checks reading each run forward from the last one's place
    /// (G1; checked one by one, every check searched every run from the root).
    pub probe_starts: u64,
    /// Lookups by object alone in relations kept without their object order (P1-F11:
    /// none).
    pub lookups_without_order: u64,
    /// Pairs of a recent run read as old that were checked against the delta (#10 of
    /// the investigation of 6 October 2026).
    pub old_checks: u64,
    /// Per round: the pairs the transitive module read to close its predicates (#11).
    pub closure_pairs: Vec<u64>,
    /// Scans for `sameAs` partners by the equality module (equality by copying, #12).
    pub member_scans: u64,
    /// The pairs the relations' merges wrote into their base and recent runs (both
    /// orders): the merge phase's work (R13's layout).
    pub merged_pairs: u64,
    /// Per round: the complete bindings the rule jobs enumerated, each a candidate fact
    /// per head atom (the joins' work).
    pub bindings: Vec<u64>,
    /// The bindings of all rounds by the name of the rule the jobs' instances come from.
    pub bindings_by_rule: std::collections::BTreeMap<String, u64>,
}

/// The bytes the working set's runs hold (both orders, spare capacity included; a run
/// shared by two roles counted once).
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct StoreBytes {
    /// The store's input, apart.
    pub input: usize,
    /// What was added since: the base and recent runs.
    pub added: usize,
    /// The delta where it doesn't share another run.
    pub delta: usize,
}

impl StoreBytes {
    pub fn total(&self) -> usize {
        self.input + self.added + self.delta
    }
}

/// Time spent per phase of [`materialise`].
#[derive(Debug, Default, Clone, Copy)]
pub struct Phases {
    pub load: std::time::Duration,
    pub grounding: std::time::Duration,
    pub joins: std::time::Duration,
    pub modules: std::time::Duration,
    pub merge: std::time::Duration,
    pub consistency: std::time::Duration,
    /// Head facts the rule joins' bindings produced, before any deduplication: with
    /// `new_facts`, the bindings per derived fact (tautological and re-derived ones
    /// included; the fast suite's rule-work cases).
    pub bindings: u64,
    /// Membership probes of the rule joins' facts against the working set.
    pub probes: u64,
    /// The new facts the rounds added.
    pub new_facts: u64,
    /// Whole materialisations run: one (equality by representatives merges classes
    /// within the rounds since R4; it ran one per pass that derived a new `owl:sameAs`).
    pub passes: u64,
}

impl Phases {
    /// Adds `other`'s times and work to these.
    pub fn add(&mut self, other: &Self) {
        self.load += other.load;
        self.grounding += other.grounding;
        self.joins += other.joins;
        self.modules += other.modules;
        self.merge += other.merge;
        self.consistency += other.consistency;
        self.bindings += other.bindings;
        self.probes += other.probes;
        self.new_facts += other.new_facts;
        self.passes += other.passes;
    }
}

/// Pairs per chunk of a run that is merged later: 16 MiB. Tiny in the unit tests, so
/// their closures go through runs of many chunks; [`set_chunk_pairs`] changes it.
static CHUNK_PAIRS: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(if cfg!(test) { 16 } else { 1 << 20 });

/// The pairs per chunk ([`CHUNK_PAIRS`]).
fn chunk_pairs() -> usize {
    CHUNK_PAIRS.load(std::sync::atomic::Ordering::Relaxed)
}

/// Sets the pairs per chunk of the working set's merged runs, for tests whose inputs are
/// too small to fill a chunk of the default 16 MiB.
#[doc(hidden)]
pub fn set_chunk_pairs(pairs: usize) {
    CHUNK_PAIRS.store(pairs.max(1), std::sync::atomic::Ordering::Relaxed);
}

/// Sorted, distinct pairs, in chunks: each chunk's pairs come before the next's. A run
/// that is merged later (a delta, which becomes the recent run and then part of the base)
/// is held in chunks of [`chunk_pairs`] pairs, and a merge writes its result a chunk at a time
/// and frees its sources a chunk at a time as it passes them. So folding the recent run
/// into the base never holds the old base beside the new one: at LUBM 1000 that was
/// 1 GB per order of `rdf:type`, the materialisation's memory peak. The input, which is
/// never merged, stays one vector, unmoved.
#[derive(Default)]
struct Pairs {
    chunks: Vec<Vec<Pair>>,
    /// Each chunk's last pair.
    lasts: Vec<Pair>,
    len: usize,
}

impl Pairs {
    /// `pairs` (sorted, distinct) as they are, one chunk.
    fn whole(pairs: Vec<Pair>) -> Self {
        let mut out = Self::default();
        out.push(pairs);
        out
    }

    /// `pairs` (sorted, distinct) in chunks of [`chunk_pairs`] without spare capacity: copied
    /// unless they are one such chunk already.
    fn chunked(pairs: Vec<Pair>) -> Self {
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

    fn push(&mut self, chunk: Vec<Pair>) {
        if let Some(&last) = chunk.last() {
            self.len += chunk.len();
            self.lasts.push(last);
            self.chunks.push(chunk);
        }
    }

    fn len(&self) -> usize {
        self.len
    }

    /// The bytes of the pairs, spare capacity included.
    fn bytes(&self) -> usize {
        self.chunks.iter().map(Vec::capacity).sum::<usize>() * std::mem::size_of::<Pair>()
    }

    /// The chunk `pair` would be in: the first whose last pair isn't before it.
    fn chunk_of(&self, pair: Pair) -> Option<&[Pair]> {
        let at = self.lasts.partition_point(|&last| last < pair);
        self.chunks.get(at).map(Vec::as_slice)
    }

    fn contains(&self, pair: Pair) -> bool {
        self.chunk_of(pair)
            .is_some_and(|chunk| chunk.binary_search(&pair).is_ok())
    }

    /// The chunk and the place in it of `pair`, if it is there.
    fn position(&self, pair: Pair) -> Option<(usize, usize)> {
        let at = self.lasts.partition_point(|&last| last < pair);
        let chunk = self.chunks.get(at)?;
        chunk.binary_search(&pair).ok().map(|i| (at, i))
    }

    /// [`Self::contains`] read forward from `finger`, where the last probe landed: for
    /// probes in ascending order (a morsel's candidates).
    fn contains_from(&self, finger: &mut PairsFinger, pair: Pair) -> bool {
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
    fn range(&self, key: u64) -> impl Iterator<Item = &[Pair]> {
        self.range_at(key).map(|(_, _, pairs)| pairs)
    }

    /// [`Self::range`], each slice with its chunk and where it starts in it.
    fn range_at(&self, key: u64) -> impl Iterator<Item = (usize, usize, &[Pair])> {
        let first = self.lasts.partition_point(|last| last.0 < key);
        self.chunks[first..]
            .iter()
            .enumerate()
            .map_while(move |(i, chunk)| {
                (chunk[0].0 <= key).then(|| {
                    let start = chunk.partition_point(|p| p.0 < key);
                    let len = chunk[start..].partition_point(|p| p.0 == key);
                    (first + i, start, &chunk[start..start + len])
                })
            })
    }

    fn iter(&self) -> impl Iterator<Item = &Pair> {
        self.chunks.iter().flatten()
    }
}

/// A place in [`Pairs`] a merge reads from.
struct Cursor {
    pairs: Arc<Pairs>,
    chunk: usize,
    at: usize,
}

impl Cursor {
    fn new(pairs: Arc<Pairs>) -> Self {
        Self {
            pairs,
            chunk: 0,
            at: 0,
        }
    }
}

/// What a merge reads from: sorted pairs, a slice at a time.
trait Feed {
    /// The next pairs not read yet; empty at the end.
    fn rest(&mut self) -> &[Pair];
    /// Marks the first `n` pairs of [`Feed::rest`] read.
    fn read(&mut self, n: usize);
}

impl Feed for Cursor {
    /// The pairs of the current chunk not read yet. A chunk read to its end is freed here
    /// if the cursor holds the only reference to the pairs.
    fn rest(&mut self) -> &[Pair] {
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

/// Which pairs of a delta one closed rule family produced ([`Relation::closed_by`]): a bit
/// per pair, per chunk of each order the delta keeps, or all of them. The family's
/// instances skip those pairs ([`Seg::DeltaNotBy`]); every other read sees the delta as
/// one run, so the partition costs lookups, folds and merges nothing.
#[derive(Default)]
struct Marks {
    /// Every pair is marked (the family produced the whole delta).
    all: bool,
    so: Vec<Vec<u64>>,
    os: Option<Vec<Vec<u64>>>,
}

impl Marks {
    fn bytes(&self) -> usize {
        let words = |chunks: &Vec<Vec<u64>>| chunks.iter().map(Vec::capacity).sum::<usize>();
        (words(&self.so) + self.os.as_ref().map_or(0, words)) * 8
    }
}

/// Whether bit `at` of `words` is set.
fn marked(words: &[u64], at: usize) -> bool {
    words[at / 64] >> (at % 64) & 1 != 0
}

/// Moves the pairs of `a` and `b` (disjoint), in order, into `out` until it is full or
/// both are read.
fn fill(a: &mut impl Feed, b: &mut impl Feed, out: &mut Vec<Pair>) {
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
fn merge(mut a: impl Feed, mut b: impl Feed, total: usize) -> Pairs {
    let mut out = Pairs::default();
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

/// A finger into [`Pairs`]: over the chunks' last pairs, and into the chunk it is in.
#[derive(Clone, Copy, Default)]
struct PairsFinger {
    chunk: Finger,
    current: usize,
    within: Finger,
}

/// A sorted run of pairs, by subject and, where a rule can look the relation up by object
/// alone, by object. Cloning shares the pairs, so the recent run can be the delta itself
/// instead of a copy.
#[derive(Default, Clone)]
struct Run {
    so: Arc<Pairs>,
    /// `(object, subject)` pairs, if kept.
    os: Option<Arc<Pairs>>,
}

/// The `(object, subject)` order of `so`.
fn objects_of(so: &Pairs) -> Pairs {
    sort_chunked(so.chunks.iter().map(Vec::as_slice).collect(), true)
}

impl Run {
    /// A run of `so` (in chunks: it will be merged), with its `(object, subject)` order if
    /// `objects`.
    fn new(so: Pairs, objects: bool) -> Self {
        let os = objects.then(|| Arc::new(objects_of(&so)));
        Self {
            so: Arc::new(so),
            os,
        }
    }

    /// A run of `os` (`(object, subject)` pairs, sorted, deduplicated) as it is: the
    /// input, which is never merged.
    fn from_os(os: Vec<Pair>) -> Self {
        let mut so: Vec<Pair> = os.iter().map(|&(o, s)| (s, o)).collect();
        so.par_sort_unstable();
        Self {
            so: Arc::new(Pairs::whole(so)),
            os: Some(Arc::new(Pairs::whole(os))),
        }
    }

    fn len(&self) -> usize {
        self.so.len()
    }

    /// The bytes of both orders, spare capacity included.
    fn bytes(&self) -> usize {
        self.so.bytes() + self.os.as_ref().map_or(0, |os| os.bytes())
    }

    /// Whether `other` holds the same pairs (shared, not a copy).
    fn shares(&self, other: &Run) -> bool {
        Arc::ptr_eq(&self.so, &other.so)
    }

    /// Keeps the `(object, subject)` order if `objects` (built if missing; the input's,
    /// given with spare capacity, made exact), drops it if not.
    fn objects(&mut self, objects: bool) {
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
                    *pairs = Pairs::default();
                    for chunk in chunks {
                        pairs.push(chunk.as_slice().to_vec());
                    }
                }
            }
            _ => {}
        }
    }

    fn contains(&self, s: u64, o: u64) -> bool {
        self.so.contains((s, o))
    }

    /// Hands every position matching the bound positions to `f`: a pair `keep` accepts as
    /// [`At::Pair`], every other position it visits (the ones `keep` or the object filter
    /// passes over) as [`At::Skip`]; ends once `f` returns false (the reader's poller
    /// stopped: [`Visit`]). `f` is generic, so the reader's step inlines into the loop over
    /// the positions (REAS-2 timing: a call per position through a `dyn` was most of the
    /// read layer's cost on replication).
    fn scan<F: FnMut(At) -> bool>(
        &self,
        s: Option<u64>,
        o: Option<u64>,
        keep: &dyn Fn(u64, u64) -> bool,
        missed: &std::sync::atomic::AtomicU64,
        f: &mut F,
    ) {
        let at = |s, o| match keep(s, o) {
            true => At::Pair(s, o),
            false => At::Skip,
        };
        match (s, o) {
            (Some(s), Some(o)) => {
                f(match self.contains(s, o) {
                    true => at(s, o),
                    false => At::Skip,
                });
            }
            (Some(s), None) => {
                for pairs in self.so.range(s) {
                    if !in_runs(pairs, f, |(_, o)| at(s, o)) {
                        return;
                    }
                }
            }
            (None, Some(o)) => match &self.os {
                Some(os) => {
                    for pairs in os.range(o) {
                        if !in_runs(pairs, f, |(_, s)| at(s, o)) {
                            return;
                        }
                    }
                }
                // Without the object order: every pair read. Never by the rules that
                // decided it isn't kept; counted where it happens all the same.
                None => {
                    if self.len() > 0 {
                        missed.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    }
                    for pairs in &self.so.chunks {
                        let read = |(s, other): Pair| match other == o {
                            true => at(s, o),
                            false => At::Skip,
                        };
                        if !in_runs(pairs, f, read) {
                            return;
                        }
                    }
                }
            },
            (None, None) => {
                for pairs in &self.so.chunks {
                    if !in_runs(pairs, f, |(s, o)| at(s, o)) {
                        return;
                    }
                }
            }
        }
    }

    /// The pairs matching the bound positions, as slices of one order (a slice per chunk
    /// they are in), with whether that order is `(object, subject)` and `old`, made as they
    /// are asked for (REAS-2 fix check r1, rule 1: no list built and counted afterwards);
    /// `None` if they aren't one slice per chunk (an object without its order).
    fn matching(&self, s: Option<u64>, o: Option<u64>, old: bool) -> Option<RunSlices<'_>> {
        Some(match (s, o) {
            (Some(s), Some(o)) => {
                let one = self.so.chunk_of((s, o)).and_then(|chunk| {
                    let at = chunk.binary_search(&(s, o)).ok()?;
                    Some(Slice::of(&chunk[at..=at], false, old))
                });
                RunSlices::one(one)
            }
            (Some(s), None) => RunSlices::of_key(&self.so, s, false, old, None),
            (None, Some(o)) => match &self.os {
                Some(os) => RunSlices::of_key(os, o, true, old, None),
                None if self.len() == 0 => RunSlices::one(None),
                None => return None,
            },
            (None, None) => RunSlices::whole(&self.so, old, None),
        })
    }

    /// [`Run::matching`] without the pairs `marks` marks, as slices that carry their
    /// chunk's marks ([`Slice::skip`]), made as they are asked for; `None` where
    /// [`Run::matching`] is.
    fn matching_unmarked<'a>(
        &'a self,
        s: Option<u64>,
        o: Option<u64>,
        marks: &'a Marks,
    ) -> Option<RunSlices<'a>> {
        if marks.all {
            return Some(RunSlices::one(None));
        }
        Some(match (s, o) {
            (Some(s), Some(o)) => {
                let one = self
                    .so
                    .position((s, o))
                    .filter(|&(c, i)| !marked(&marks.so[c], i))
                    .map(|(c, i)| Slice::of(&self.so.chunks[c][i..=i], false, false));
                RunSlices::one(one)
            }
            (Some(s), None) => RunSlices::of_key(&self.so, s, false, false, Some(&marks.so)),
            (None, Some(o)) => match (&self.os, &marks.os) {
                (Some(os), Some(os_marks)) => RunSlices::of_key(os, o, true, false, Some(os_marks)),
                _ if self.len() == 0 => RunSlices::one(None),
                _ => return None,
            },
            (None, None) => RunSlices::whole(&self.so, false, Some(&marks.so)),
        })
    }

    /// [`Run::scan`] without the pairs `marks` marks: each slice's positions handed on
    /// ahead ([`At::Ahead`], the marked ones included), its pairs, then [`At::Settle`].
    fn scan_unmarked<F: FnMut(At) -> bool>(
        &self,
        s: Option<u64>,
        o: Option<u64>,
        marks: &Marks,
        missed: &std::sync::atomic::AtomicU64,
        f: &mut F,
    ) {
        if marks.all {
            return;
        }
        // Each slice made as it is reached; its positions counted ahead below (a key's
        // chunks without a pair of it end the slices: at most one empty slice is made).
        if let Some(slices) = self.matching_unmarked(s, o, marks) {
            for slice in slices {
                // A slice is handed on in pieces of at most POLL_UNITS positions, each
                // counted ahead: a chunk (up to CHUNK_PAIRS, 1 << 20) counted at once was
                // one poll for all of it.
                let len = slice.pairs.len();
                let mut start = 0;
                while start < len {
                    let end = len.min(start + RUN);
                    if !f(At::Ahead(end - start)) {
                        return;
                    }
                    let mut going = true;
                    slice.for_each_in(start..end, |(a, b)| {
                        // Stopped by a read of `f`'s: the rest of the piece is passed over.
                        if going {
                            going = f(match slice.swapped {
                                true => At::Pair(b, a),
                                false => At::Pair(a, b),
                            });
                        }
                    });
                    if !going || !f(At::Settle) {
                        return;
                    }
                    start = end;
                }
            }
            return;
        }
        // By object without the object order: every pair read.
        let o = o.expect("only lookups by object alone have no slices");
        if self.len() > 0 {
            missed.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
        for (chunk, words) in self.so.chunks.iter().zip(&marks.so) {
            for (i, &(s, other)) in chunk.iter().enumerate() {
                let at = match other == o && !marked(words, i) {
                    true => At::Pair(s, o),
                    false => At::Skip,
                };
                if !f(at) {
                    return;
                }
            }
        }
    }

    /// An upper bound on the pairs matching the bound positions: a chunk range read per chunk
    /// the key's pairs span, each counted on `probe` (review r1 F2: a key can span many
    /// chunks).
    fn estimate(&self, s: Option<u64>, o: Option<u64>, probe: Probe<'_, '_>) -> usize {
        match (s, o) {
            (Some(s), Some(o)) => usize::from(self.contains(s, o)),
            (Some(s), None) => probe.counted(self.so.range(s)).map(<[Pair]>::len).sum(),
            (None, Some(o)) => match &self.os {
                Some(os) => probe.counted(os.range(o)).map(<[Pair]>::len).sum(),
                None => self.so.len(),
            },
            (None, None) => self.so.len(),
        }
    }

    /// The union with a disjoint run, one order at a time, so the sources and the result
    /// never coexist in both; a source this holds the only reference to is freed as it is
    /// merged. Adds the pairs it writes (both orders) to `written`.
    fn merge(self, other: Run, written: &mut usize) -> Run {
        if self.len() == 0 {
            return other;
        }
        if other.len() == 0 {
            return self;
        }
        let two = |a: Arc<Pairs>, b: Arc<Pairs>| {
            let total = a.len() + b.len();
            Arc::new(merge(Cursor::new(a), Cursor::new(b), total))
        };
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
struct Slice<'a> {
    pairs: &'a [Pair],
    /// Whether its order is `(object, subject)`.
    swapped: bool,
    /// Whether its pairs are read as old (the recent run's, whose delta pairs are skipped).
    old: bool,
    /// The marks of its chunk and where the slice starts in it: marked pairs are skipped
    /// ([`Marks`]).
    skip: Option<(&'a [u64], usize)>,
}

impl<'a> Slice<'a> {
    fn of(pairs: &'a [Pair], swapped: bool, old: bool) -> Self {
        Self {
            pairs,
            swapped,
            old,
            skip: None,
        }
    }

    /// Calls `f` with each pair of `range` (positions in the slice) not skipped, as stored.
    fn for_each_in(&self, range: std::ops::Range<usize>, mut f: impl FnMut(Pair)) {
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

    /// [`Self::for_each_in`] over the whole slice (the chunk tests' reads; the store reads a
    /// slice a run at a time).
    #[cfg(test)]
    fn for_each(&self, f: impl FnMut(Pair)) {
        self.for_each_in(0..self.pairs.len(), f);
    }
}

/// Keeps the facts of `facts` that `store` doesn't hold, in order; [`Stopped`] at the first
/// probe its poller stopped, `facts` then emptied (fix check r1, rule 2: a stopped probe
/// read as "not held" walked every fact after it).
fn keep_unheld<S: Source + ?Sized>(
    store: &S,
    facts: &mut Vec<Triple>,
    poll: &Poller<'_>,
) -> Result<(), Stopped> {
    let mut kept = 0;
    for i in 0..facts.len() {
        match store.contains(facts[i], poll) {
            Ok(true) => {}
            Ok(false) => {
                facts[kept] = facts[i];
                kept += 1;
            }
            Err(Stopped) => {
                facts.clear();
                return Err(Stopped);
            }
        }
    }
    facts.truncate(kept);
    Ok(())
}

/// One run's slices matching a pattern ([`Run::matching`]), each made as it is asked for:
/// a chunk's slice of a key costs a search in that chunk, and nothing is collected (REAS-2
/// fix check r1, R2-F2: one key can span any number of chunks).
#[derive(Clone)]
struct RunSlices<'a> {
    /// A pair's own slice (both positions bound), handed on first.
    one: Option<Slice<'a>>,
    /// The chunks read from `at` on, `None` once read or where there are none.
    chunks: Option<&'a Pairs>,
    at: usize,
    /// The key the slices hold (`None`: whole chunks).
    key: Option<u64>,
    swapped: bool,
    old: bool,
    /// Each chunk's marks, where the slices skip marked pairs.
    marks: Option<&'a [Vec<u64>]>,
}

impl<'a> RunSlices<'a> {
    /// At most one slice.
    fn one(one: Option<Slice<'a>>) -> Self {
        Self {
            one,
            chunks: None,
            at: 0,
            key: None,
            swapped: false,
            old: false,
            marks: None,
        }
    }

    /// The pairs whose first component is `key`: a slice of each chunk they are in, from
    /// the first whose last pair isn't before them.
    fn of_key(
        pairs: &'a Pairs,
        key: u64,
        swapped: bool,
        old: bool,
        marks: Option<&'a [Vec<u64>]>,
    ) -> Self {
        Self {
            chunks: Some(pairs),
            at: pairs.lasts.partition_point(|last| last.0 < key),
            key: Some(key),
            swapped,
            old,
            marks,
            ..Self::one(None)
        }
    }

    /// Every chunk whole.
    fn whole(pairs: &'a Pairs, old: bool, marks: Option<&'a [Vec<u64>]>) -> Self {
        Self {
            chunks: Some(pairs),
            old,
            marks,
            ..Self::one(None)
        }
    }
}

impl<'a> Iterator for RunSlices<'a> {
    type Item = Slice<'a>;

    #[inline]
    fn next(&mut self) -> Option<Slice<'a>> {
        if let Some(one) = self.one.take() {
            return Some(one);
        }
        let chunks = self.chunks?;
        let Some(chunk) = chunks.chunks.get(self.at) else {
            self.chunks = None;
            return None;
        };
        let c = self.at;
        self.at += 1;
        let (start, pairs) = match self.key {
            None => (0, &chunk[..]),
            Some(key) if chunk[0].0 > key => {
                self.chunks = None;
                return None;
            }
            Some(key) => {
                let start = chunk.partition_point(|p| p.0 < key);
                let len = chunk[start..].partition_point(|p| p.0 == key);
                (start, &chunk[start..start + len])
            }
        };
        Some(Slice {
            pairs,
            swapped: self.swapped,
            old: self.old,
            skip: self.marks.map(|marks| (&marks[c][..], start)),
        })
    }
}

/// Slices of a relation's runs matching a pattern, in the order its runs are read
/// ([`Relation::matching`]): each run's made as it is asked for.
type Slices<'a> =
    std::iter::Flatten<std::iter::Flatten<std::array::IntoIter<Option<RunSlices<'a>>, 3>>>;

/// The pairs of one predicate, in sorted runs: the store's input, apart, and what was
/// added since, in a large base and a small recent run that takes each round's delta.
/// The recent run is folded into the base once it reaches a quarter of its size, so a
/// round costs its delta plus the recent run, not the whole relation, and the total
/// merge work stays O(n log n). The input, kept apart, makes the added pairs the base
/// and the recent run: what a materialisation derived, without a list of its own.
/// A position a relation's scan visits, as it hands it on ([`Relation::scan`]): a pair it
/// delivers, a position it passes over, a run of positions about to be visited (counted
/// ahead), or the end of such a run (what of it went unvisited, marked pairs, is dropped).
/// The store maps these onto its reader's [`Visit`].
#[derive(Clone, Copy)]
enum At {
    Pair(u64, u64),
    Skip,
    Ahead(usize),
    Settle,
}

/// The longest run of positions (or copies, or probes) a read counts ahead at once: REAS-2's
/// bound on the work after a stop is `POLL_UNITS` plus one run, so a run never exceeds
/// `POLL_UNITS` ([`in_runs`], [`Run::scan_unmarked`], `Equality::run`'s and `list`'s
/// products).
const RUN: usize = nrese_exec::POLL_UNITS as usize;

/// Hands `pairs` to `f` a run of at most [`RUN`] positions at a time, each run counted
/// ahead ([`At::Ahead`], then each position as `read` makes it, then [`At::Settle`]):
/// one count per run, not per position; false once `f` returns false.
fn in_runs<F: FnMut(At) -> bool>(
    pairs: &[Pair],
    f: &mut F,
    mut read: impl FnMut(Pair) -> At,
) -> bool {
    for run in pairs.chunks(RUN) {
        if !f(At::Ahead(run.len())) {
            return false;
        }
        for &pair in run {
            if !f(read(pair)) {
                return false;
            }
        }
        if !f(At::Settle) {
            return false;
        }
    }
    true
}

/// What a relation's positions are, to the store's reader `visit`: `p`'s facts.
fn on(visit: &mut Visit<'_, '_>, p: u64, at: At) -> bool {
    match at {
        At::Pair(s, o) => visit.fact([s, p, o]),
        At::Skip => visit.skip(),
        At::Ahead(n) => visit.ahead(n),
        At::Settle => {
            visit.settle();
            !visit.stopped()
        }
    }
}

#[derive(Default)]
pub(crate) struct Relation {
    /// The pairs the store started with, once the first round has read them as its
    /// delta.
    input: Run,
    base: Run,
    recent: Run,
    /// The last round's pairs (also in `recent`).
    delta: Run,
    /// Those of the delta one closed rule family produced (the family whose schema link
    /// is `closed_by`: `rdfs:subClassOf` for `cax-sco`), if it produced some: the family's
    /// instances read the delta without them ([`Seg::DeltaNotBy`]), as it derived them
    /// with all their consequences under the family.
    marks: Option<Marks>,
    closed_by: Option<u64>,
    /// Whether the delta is still the input (the store's first round is to come).
    fresh: bool,
    /// Whether no rule looks the relation up by object alone, so its runs keep no
    /// `(object, subject)` order (unused relations, P1-F11).
    by_subject_only: bool,
    /// Lookups by object alone in its runs without that order: the analysis that drops
    /// the order ([`Reads`]) says there are none, and the closure tests check it.
    missed: std::sync::atomic::AtomicU64,
    /// Pairs of the recent run read as old that were looked up in the delta.
    old_checks: std::sync::atomic::AtomicU64,
}

/// The delta of `pairs` (sorted, disjoint from the relation), some of them marked by
/// `marks` (its subject order's, from [`sort_marked`]), with its `(object, subject)` order
/// and that order's marks if `objects`.
fn marked_run(pairs: Pairs, marks: Option<Marks>, objects: bool) -> (Run, Option<Marks>) {
    let Some(mut marks) = marks else {
        return (Run::new(pairs, objects), None);
    };
    if marks.all || !objects {
        return (Run::new(pairs, objects), Some(marks));
    }
    let parts = pairs
        .chunks
        .iter()
        .zip(&marks.so)
        .map(|(chunk, words)| (chunk.as_slice(), Marking::Bits(words)))
        .collect();
    let (os, os_marks) = sort_marked(parts, true);
    marks.os = os_marks;
    let run = Run {
        so: Arc::new(pairs),
        os: Some(Arc::new(os)),
    };
    (run, Some(marks))
}

impl Relation {
    pub(crate) fn delta_len(&self) -> usize {
        self.delta.len()
    }

    /// Whether the recent run holds nothing but the delta (it is the delta, shared: right
    /// after a fold, or the first round's input), so its old part is empty.
    fn recent_is_delta(&self) -> bool {
        self.recent.shares(&self.delta)
    }

    /// The marks `seg` skips: the closed part of the delta, read by its own family.
    fn skips(&self, seg: Seg) -> Option<&Marks> {
        match seg {
            Seg::DeltaNotBy(family) if self.closed_by == Some(family) => self.marks.as_ref(),
            _ => None,
        }
    }

    fn bytes(&self) -> StoreBytes {
        let (input, base, recent) = (&self.input, &self.base, &self.recent);
        let shared = |run: &Run| [input, base, recent].iter().any(|other| run.shares(other));
        StoreBytes {
            input: input.bytes(),
            added: base.bytes()
                + if recent.shares(base) {
                    0
                } else {
                    recent.bytes()
                },
            delta: if shared(&self.delta) {
                0
            } else {
                self.delta.bytes()
            } + self.marks.as_ref().map_or(0, Marks::bytes),
        }
    }

    /// Whether the delta has a pair with object `o`, counting what it reads on `poll`
    /// (false once that has stopped: an answer that means nothing).
    pub(crate) fn delta_has_object(&self, o: u64, poll: &Poller<'_>) -> bool {
        let mut found = false;
        let all = |_: u64, _: u64| true;
        self.delta
            .scan(None, Some(o), &all, &self.missed, &mut |at| {
                found |= matches!(at, At::Pair(..));
                !poll.tick()
            });
        found && !poll.stopped()
    }

    /// Calls `f` with each distinct object of the delta, counting each pair it reads on
    /// `poll` and ending once that has stopped.
    pub(crate) fn delta_objects(&self, poll: &Poller<'_>, f: &mut dyn FnMut(u64)) {
        match &self.delta.os {
            Some(os) => {
                let mut last = None;
                for &(o, _) in os.iter() {
                    if poll.tick() {
                        return;
                    }
                    if last != Some(o) {
                        f(o);
                        last = Some(o);
                    }
                }
            }
            None => {
                // Each pair read counted as it is read (review r1 F1).
                let mut objects: Vec<u64> = poll
                    .counted(self.delta.so.iter())
                    .map(|&(_, o)| o)
                    .collect();
                if poll.stopped() {
                    return;
                }
                objects.sort_unstable();
                objects.dedup();
                objects.into_iter().for_each(f);
            }
        }
    }

    /// Keeps the runs' `(object, subject)` orders if `objects`, else drops them (see
    /// [`Relation::by_subject_only`]).
    fn objects(&mut self, objects: bool) {
        self.by_subject_only = !objects;
        for run in [&mut self.input, &mut self.base, &mut self.recent] {
            run.objects(objects);
        }
        // The delta shares the recent run's pairs or is its own.
        if self.delta.shares(&self.recent) {
            self.delta.os = self.recent.os.clone();
        } else {
            self.delta.objects(objects);
        }
        // The marks follow the delta's orders: an object order built since the delta was
        // marked gets its marks from the subject order's.
        if let Some(marks) = &mut self.marks
            && !marks.all
        {
            match &self.delta.os {
                None => marks.os = None,
                Some(os) if marks.os.is_none() => {
                    let so = &self.delta.so;
                    let words = os
                        .chunks
                        .iter()
                        .map(|chunk| {
                            let mut words = vec![0u64; chunk.len().div_ceil(64)];
                            for (i, &(o, s)) in chunk.iter().enumerate() {
                                let (c, at) = so.position((s, o)).expect("one delta");
                                if marked(&marks.so[c], at) {
                                    words[i / 64] |= 1 << (i % 64);
                                }
                            }
                            words
                        })
                        .collect();
                    marks.os = Some(words);
                }
                Some(_) => {}
            }
        }
    }

    pub(crate) fn contains(&self, s: u64, o: u64) -> bool {
        self.base.contains(s, o) || self.recent.contains(s, o) || self.input.contains(s, o)
    }

    /// [`Self::contains`] with a finger per run it reads, for pairs probed in ascending
    /// order.
    fn contains_from(&self, s: u64, o: u64, fingers: &mut [PairsFinger; 3]) -> bool {
        [&self.base, &self.recent, &self.input]
            .iter()
            .zip(fingers)
            .any(|(run, finger)| run.so.contains_from(finger, (s, o)))
    }

    /// Every pair, in no particular order.
    pub(crate) fn pairs(&self) -> Vec<Pair> {
        let mut out = Vec::with_capacity(self.input.len() + self.base.len() + self.recent.len());
        for run in [&self.input, &self.base, &self.recent] {
            out.extend(run.so.iter());
        }
        out
    }

    /// Hands every position matching the bound positions in `seg` to `f`, as its runs visit
    /// them ([`Run::scan`]); ends once `f` returns false.
    fn scan<F: FnMut(At) -> bool>(&self, s: Option<u64>, o: Option<u64>, seg: Seg, f: &mut F) {
        let all = |_: u64, _: u64| true;
        let missed = &self.missed;
        match seg {
            Seg::Delta | Seg::DeltaNotBy(_) => match self.skips(seg) {
                Some(marks) => self.delta.scan_unmarked(s, o, marks, missed, f),
                None => self.delta.scan(s, o, &all, missed, f),
            },
            Seg::All => {
                self.input.scan(s, o, &all, missed, f);
                self.base.scan(s, o, &all, missed, f);
                self.recent.scan(s, o, &all, missed, f);
            }
            // The delta is in `recent` only, so the input and the base need no filter.
            Seg::Old => {
                self.input.scan(s, o, &all, missed, f);
                self.base.scan(s, o, &all, missed, f);
                if self.recent_is_delta() {
                    return;
                }
                let old = |s: u64, o: u64| {
                    self.old_checks
                        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    !self.delta.contains(s, o)
                };
                self.recent.scan(s, o, &old, missed, f);
            }
        }
    }

    /// The matches of the bound positions in `seg`, as the slices of the runs that
    /// hold them, in [`Relation::scan`]'s order (see [`Slices`]), each made as it is asked
    /// for; `None` if a run can't give them as slices (an object without its order),
    /// decided before any is made.
    fn matching(&self, s: Option<u64>, o: Option<u64>, seg: Seg) -> Option<Slices<'_>> {
        let runs = match seg {
            Seg::Delta | Seg::DeltaNotBy(_) => [
                Some(match self.skips(seg) {
                    Some(marks) => self.delta.matching_unmarked(s, o, marks)?,
                    None => self.delta.matching(s, o, false)?,
                }),
                None,
                None,
            ],
            Seg::All | Seg::Old => [
                Some(self.input.matching(s, o, false)?),
                Some(self.base.matching(s, o, false)?),
                match seg == Seg::Old && self.recent_is_delta() {
                    true => None,
                    false => Some(self.recent.matching(s, o, seg == Seg::Old)?),
                },
            ],
        };
        Some(runs.into_iter().flatten().flatten())
    }

    /// An upper bound on the matches of the bound positions in `seg`.
    fn estimate(&self, s: Option<u64>, o: Option<u64>, seg: Seg, probe: Probe<'_, '_>) -> usize {
        match seg {
            Seg::Delta | Seg::DeltaNotBy(_) => match self.skips(seg) {
                Some(marks) if marks.all => 0,
                _ => self.delta.estimate(s, o, probe),
            },
            Seg::Old | Seg::All => {
                self.input.estimate(s, o, probe)
                    + self.base.estimate(s, o, probe)
                    + self.recent.estimate(s, o, probe)
            }
        }
    }

    /// Makes `new` and `closed` (produced by the family `closed_by` names) the delta, the
    /// pairs of `closed` marked; both sorted, deduplicated, disjoint from each other and
    /// from the relation. Returns the pairs its merges wrote.
    fn advance(&mut self, new: Pairs, marks: Option<Marks>) -> usize {
        let mut written = 0;
        // The old delta goes first: it shares the recent run, which is then merged, and
        // freed as it is, only where nothing else holds it.
        self.delta = Run::default();
        self.marks = None;
        if self.fresh {
            // The first round read the input as its delta (and recent run, shared): it
            // goes apart, unmoved.
            self.input = std::mem::take(&mut self.recent);
            self.fresh = false;
        } else if self.recent.len() * 4 > self.base.len() {
            // Fold the recent run into the base first, so the new delta stays in `recent`.
            let recent = std::mem::take(&mut self.recent);
            self.base = std::mem::take(&mut self.base).merge(recent, &mut written);
        }
        (self.delta, self.marks) = marked_run(new, marks, !self.by_subject_only);
        if self.delta.len() > 0 {
            // An empty recent run becomes the delta itself (shared, not copied).
            self.recent = std::mem::take(&mut self.recent).merge(self.delta.clone(), &mut written);
        }
        written
    }
}

/// The working set: one relation per predicate.
#[derive(Default)]
pub(crate) struct Store {
    relations: Vec<Relation>,
    /// The predicate of each relation.
    predicates: Vec<u64>,
    index: HashMap<u64, usize>,
    /// Relations whose closed part a family claimed beforehand ([`Store::claim`]).
    claims: HashMap<u64, u64>,
    /// The pairs the relations' merges wrote so far (both orders).
    merged: u64,
}

impl Store {
    /// A store holding `input`, all of it as the delta.
    pub(crate) fn new(input: Vec<Triple>) -> Self {
        let mut store = Self::default();
        store.advance(input);
        for relation in &mut store.relations {
            relation.fresh = true;
        }
        store
    }

    /// A store holding `groups` (see [`materialise_grouped`]), all of it as the delta.
    pub(crate) fn from_groups(groups: Vec<(u64, Vec<Pair>)>) -> Self {
        let mut store = Self::default();
        for (p, _) in &groups {
            store.index.insert(*p, store.predicates.len());
            store.predicates.push(*p);
        }
        store.relations = groups
            .into_par_iter()
            .map(|(_, os)| {
                let delta = Run::from_os(os);
                Relation {
                    input: Run::default(),
                    base: Run::default(),
                    recent: delta.clone(),
                    delta,
                    marks: None,
                    closed_by: None,
                    fresh: true,
                    by_subject_only: false,
                    missed: Default::default(),
                    old_checks: Default::default(),
                }
            })
            .collect();
        store
    }

    /// The relations with a non-empty delta, with their predicates.
    pub(crate) fn delta_relations(&self) -> impl Iterator<Item = (u64, &Relation)> {
        self.predicates
            .iter()
            .zip(&self.relations)
            .filter(|(_, r)| r.delta_len() > 0)
            .map(|(&p, r)| (p, r))
    }

    /// The bytes the runs hold.
    pub(crate) fn bytes(&self) -> StoreBytes {
        self.relations
            .iter()
            .map(Relation::bytes)
            .fold(StoreBytes::default(), |sum, bytes| StoreBytes {
                input: sum.input + bytes.input,
                added: sum.added + bytes.added,
                delta: sum.delta + bytes.delta,
            })
    }

    /// Keeps the `(object, subject)` order of the relations whose predicate `objects`
    /// accepts and drops the others' (see [`Relation::by_subject_only`]).
    fn objects(&mut self, objects: &(dyn Fn(u64) -> bool + Sync)) {
        self.relations
            .par_iter_mut()
            .zip(self.predicates.par_iter())
            .for_each(|(relation, &p)| relation.objects(objects(p)));
    }

    pub(crate) fn relation(&self, p: u64) -> Option<&Relation> {
        self.index.get(&p).map(|&i| &self.relations[i])
    }

    /// Keeps the facts of `facts` (sorted by predicate, subject and object: a morsel's
    /// candidates) that the store doesn't hold. Each relation's runs are read forward from
    /// where the last probe landed ([`Finger`]), not searched from the root for each fact.
    /// Those searches were 54 % of LUBM 100's reasoning samples, but mostly the cache
    /// misses at their targets, which a finger keeps: the reasoning took 6 % less.
    /// Returns how often it started the runs from the beginning: once per relation.
    /// Each fact's probe counted on `poll`, a run of at most [`RUN`] at a time (review r1 F1:
    /// a morsel bounds its drivers, not its output). The first run that finds `poll` stopped
    /// ends the keep: `facts` is emptied and [`Stopped`] returned, no candidate after it
    /// looked at (fix check r1, R2-F3: a stopped keep answering "not new" for the rest still
    /// walked them all).
    fn retain_new(&self, facts: &mut Vec<Triple>, poll: &Poller<'_>) -> Result<u64, Stopped> {
        let mut at: Option<(u64, Option<&Relation>)> = None;
        let mut fingers = [PairsFinger::default(); 3];
        let mut starts = 0;
        let len = facts.len();
        let mut kept = 0;
        for i in 0..len {
            #[cfg(test)]
            crate::witness::add(crate::witness::Site::RetainStep, 1);
            if i % RUN == 0 && poll.attempt(RUN.min(len - i)) {
                facts.clear();
                return Err(Stopped);
            }
            let [s, p, o] = facts[i];
            if at.is_none_or(|(q, _)| q != p) {
                at = Some((p, self.relation(p)));
                fingers = [PairsFinger::default(); 3];
                starts += 1;
            }
            let relation = at.and_then(|(_, relation)| relation);
            if relation.is_none_or(|r| !r.contains_from(s, o, &mut fingers)) {
                facts[kept] = facts[i];
                kept += 1;
            }
        }
        facts.truncate(kept);
        Ok(starts)
    }

    /// The relation of `p`, or every relation where `p` is open, with their predicates.
    fn relations_of(&self, p: Option<u64>) -> Box<dyn Iterator<Item = (u64, &Relation)> + '_> {
        match p {
            Some(p) => Box::new(self.relation(p).map(|r| (p, r)).into_iter()),
            None => Box::new(self.predicates.iter().copied().zip(&self.relations)),
        }
    }

    /// Adds `candidates` and makes the new ones the delta; returns the new ones.
    pub(crate) fn advance(&mut self, candidates: Vec<Triple>) -> Vec<Triple> {
        let news = self.news(vec![(None, segments(candidates))], true);
        let mut delta = Vec::with_capacity(news.iter().map(|(open, _)| open.len()).sum());
        for ((pairs, _), &p) in news.iter().zip(&self.predicates) {
            delta.extend(pairs.iter().map(|&(s, o)| [s, p, o]));
        }
        self.install(news);
        delta
    }

    /// [`Self::advance`] for candidates known to be absent from the store (filtered
    /// against it when they were derived), in lists of [`Segments`]: skips the membership
    /// check, and returns only how many were new and whether one is a schema fact, not
    /// the facts.
    pub(crate) fn advance_new(
        &mut self,
        candidates: Vec<Candidates>,
        schema: &Schema,
    ) -> (usize, bool) {
        let news = self.news(candidates, false);
        let schema_facts = news
            .par_iter()
            .zip(self.predicates.par_iter())
            .any(|((pairs, _), &p)| pairs.iter().any(|&(s, o)| schema.is_schema_fact([s, p, o])));
        heap::phase("reasoner: install");
        self.install(news);
        let count = self.relations.iter().map(Relation::delta_len).sum();
        (count, schema_facts)
    }

    /// Every pair added since the input, as facts, in no particular order: what a
    /// materialisation from the input derived. The store is taken apart on the way, so
    /// the facts never sit beside all of it: the input runs and the `(object, subject)`
    /// orders go first, and each relation's runs once their facts are out.
    pub(crate) fn into_derived(self) -> Vec<Triple> {
        let runs: Vec<(u64, [Arc<Pairs>; 2])> = self
            .relations
            .into_iter()
            .zip(self.predicates)
            .map(|(relation, p)| (p, [relation.base.so, relation.recent.so]))
            .collect();
        let mut out = Vec::with_capacity(
            runs.iter()
                .map(|(_, runs)| runs.iter().map(|run| run.len()).sum::<usize>())
                .sum(),
        );
        for (p, pairs) in runs {
            for pairs in pairs {
                // Each chunk freed once its facts are out, where nothing else holds it.
                match Arc::try_unwrap(pairs) {
                    Ok(pairs) => {
                        for chunk in pairs.chunks {
                            out.extend(chunk.iter().map(|&(s, o)| [s, p, o]));
                        }
                    }
                    Err(pairs) => out.extend(pairs.iter().map(|&(s, o)| [s, p, o])),
                }
            }
        }
        out
    }

    /// The input's facts passing `input` and the added ones passing `added`, in no
    /// particular order, taking the store apart as [`Self::into_derived`] does: the
    /// input's are read first, then each relation's added runs, freed once read.
    fn into_listed(
        self,
        input: &(dyn Fn(Triple) -> bool + Sync),
        added: &(dyn Fn(Triple) -> bool + Sync),
        mut out: Vec<Triple>,
    ) -> Vec<Triple> {
        let from_input: Vec<Triple> = self
            .relations
            .par_iter()
            .zip(self.predicates.par_iter())
            .flat_map_iter(|(relation, &p)| {
                relation
                    .input
                    .so
                    .iter()
                    .map(move |&(s, o)| [s, p, o])
                    .filter(|&fact| input(fact))
            })
            .collect();
        let runs: Vec<(u64, [Arc<Pairs>; 2])> = self
            .relations
            .into_iter()
            .zip(self.predicates)
            .map(|(relation, p)| (p, [relation.base.so, relation.recent.so]))
            .collect();
        // At most this many, so the list never grows by doubling: exactly, so a caller can
        // charge the growth before it hands `out` over ([`list`]).
        out.reserve_exact(
            from_input.len()
                + runs
                    .iter()
                    .map(|(_, runs)| runs.iter().map(|run| run.len()).sum::<usize>())
                    .sum::<usize>(),
        );
        out.extend(from_input);
        for (p, pairs) in runs {
            for pairs in pairs {
                let list = |chunk: &[Pair], out: &mut Vec<Triple>| {
                    out.extend(
                        chunk
                            .iter()
                            .map(|&(s, o)| [s, p, o])
                            .filter(|&fact| added(fact)),
                    );
                };
                // Each chunk freed once its facts are out, where nothing else holds it.
                match Arc::try_unwrap(pairs) {
                    Ok(pairs) => pairs.chunks.into_iter().for_each(|c| list(&c, &mut out)),
                    Err(pairs) => pairs.chunks.iter().for_each(|c| list(c, &mut out)),
                }
            }
        }
        out
    }

    /// Every stored fact (input and added) passing `keep`, in no particular order.
    fn facts_where(&self, keep: &(dyn Fn(Triple) -> bool + Sync)) -> Vec<Triple> {
        self.relations
            .par_iter()
            .zip(self.predicates.par_iter())
            .flat_map_iter(|(relation, &p)| {
                [&relation.input, &relation.base, &relation.recent]
                    .into_iter()
                    .flat_map(|run| run.so.iter())
                    .map(move |&(s, o)| [s, p, o])
                    .filter(|&fact| keep(fact))
            })
            .collect()
    }

    /// Every stored fact mentioning one of `terms`, in any position (some more than once);
    /// `None` if `stop` (latched) fired. By the subject and object orders where a run has
    /// them; a run without its object order is read whole for the objects. Each relation is
    /// a task with its own poller, counting each lookup (a term in a run's order) and each
    /// position read (sweep M17: relations × terms lookups).
    fn mentioning(&self, terms: &HashSet<u64>, stop: super::eval::Stop<'_>) -> Option<Vec<Triple>> {
        let facts = self
            .relations
            .par_iter()
            .zip(self.predicates.par_iter())
            .flat_map_iter(|(relation, &p)| {
                let mut out = Vec::new();
                let Some(poll) = Poller::new(stop) else {
                    return out;
                };
                // The pairs of `slices`, each counted as it is read (review r1 F1: a slice is
                // a whole chunk, or the whole input); false once stopped.
                let read = |out: &mut Vec<Triple>,
                            slices: &mut dyn Iterator<Item = &[Pair]>,
                            swapped: bool| {
                    for slice in slices {
                        out.extend(poll.counted(slice).map(|&(a, b)| match swapped {
                            true => [b, p, a],
                            false => [a, p, b],
                        }));
                        if poll.stopped() {
                            return false;
                        }
                    }
                    !poll.tick()
                };
                for run in [&relation.input, &relation.base, &relation.recent] {
                    if terms.contains(&p) {
                        if !read(
                            &mut out,
                            &mut run.so.chunks.iter().map(Vec::as_slice),
                            false,
                        ) {
                            return Vec::new();
                        }
                        continue;
                    }
                    for &t in terms {
                        if !read(&mut out, &mut run.so.range(t), false) {
                            return Vec::new();
                        }
                    }
                    match &run.os {
                        Some(os) => {
                            for &t in terms {
                                if !read(&mut out, &mut os.range(t), true) {
                                    return Vec::new();
                                }
                            }
                        }
                        None => {
                            out.extend(
                                poll.counted(run.so.iter())
                                    .filter(|(_, o)| terms.contains(o))
                                    .map(|&(s, o)| [s, p, o]),
                            );
                            if poll.stopped() {
                                return Vec::new();
                            }
                        }
                    }
                }
                out
            })
            .collect();
        // A task that stopped left facts out; the stop is latched, so asking it again tells.
        (!stop()).then_some(facts)
    }

    /// Makes `news` (per relation: the open part, the closed part) the relations' deltas.
    fn install(&mut self, news: Vec<New>) {
        self.merged += self
            .relations
            .par_iter_mut()
            .zip(news.into_par_iter())
            .map(|(relation, (pairs, marks))| relation.advance(pairs, marks) as u64)
            .sum::<u64>();
    }

    /// Reserves the closed part of `p`'s relation for `family` (a family that reads and
    /// produces it by name), unless one has it already.
    fn claim(&mut self, p: u64, family: u64) {
        self.claims.entry(p).or_insert(family);
    }

    /// The new pairs of `candidates` per relation (relations added for new predicates),
    /// each sorted, distinct and in chunks without spare capacity: those a closed family
    /// produced in the relation's closed part (the first family that produces into a
    /// relation owns its closed part; another's go to the open part), the others in the
    /// open part. A pair in both parts is marked when the delta is built ([`marked_run`]).
    fn news(&mut self, candidates: Vec<Candidates>, check: bool) -> Vec<New> {
        // Each predicate's lists together (a stable sort: the parts keep their order).
        let mut parts: Vec<(u64, Option<u64>, Vec<Pair>)> = candidates
            .into_iter()
            .flat_map(|(family, segments)| {
                segments
                    .into_iter()
                    .map(move |(p, pairs)| (p, family, pairs))
            })
            .filter(|(_, _, pairs)| !pairs.is_empty())
            .collect();
        parts.par_sort_by_key(|&(p, _, _)| p);
        type Lists = (Vec<Vec<Pair>>, Vec<Vec<Pair>>);
        let mut groups: Vec<(usize, Lists)> = Vec::new();
        for (p, family, pairs) in parts {
            let index = match self.index.get(&p) {
                Some(&index) => index,
                None => {
                    self.index.insert(p, self.relations.len());
                    self.relations.push(Relation::default());
                    self.predicates.push(p);
                    self.relations.len() - 1
                }
            };
            if groups.last().is_none_or(|(last, _)| *last != index) {
                groups.push((index, (Vec::new(), Vec::new())));
            }
            let relation = &mut self.relations[index];
            let claimed = self.claims.get(&p).copied();
            let closed = match family {
                Some(family)
                    if claimed.is_none_or(|owner| owner == family)
                        && relation.closed_by.is_none_or(|owner| owner == family) =>
                {
                    relation.closed_by = Some(family);
                    true
                }
                _ => false,
            };
            let (open_lists, closed_lists) = &mut groups.last_mut().expect("pushed").1;
            match closed {
                true => closed_lists.push(pairs),
                false => open_lists.push(pairs),
            }
        }
        // Keep only facts not already known, per predicate and in parallel.
        let groups: Vec<(usize, New)> = groups
            .into_par_iter()
            .map(|(index, (mut open, mut closed))| {
                if check {
                    let relation = &self.relations[index];
                    for part in open.iter_mut().chain(closed.iter_mut()) {
                        part.retain(|&(s, o)| !relation.contains(s, o));
                    }
                }
                (index, new_pairs(open, closed))
            })
            .collect();
        let mut news: Vec<New> = self
            .relations
            .iter()
            .map(|_| (Pairs::default(), None))
            .collect();
        for (index, pairs) in groups {
            news[index] = pairs;
        }
        news
    }
}

/// The store's reads ([`Source`]'s counting): each relation a scan opens is a unit
/// ([`Visit::skip`]), each position its runs visit another ([`Relation::scan`], the ones the
/// old segment's filter passes over included); a range's positions are counted ahead, the
/// ones the delta filter passes over skipped within them. Probes over every relation count
/// each relation.
impl Visits for Store {
    fn visit(&self, [s, p, o]: [Option<u64>; 3], seg: Seg, visit: &mut Visit<'_, '_>) {
        match p {
            Some(p) => {
                if let Some(relation) = self.relation(p) {
                    relation.scan(s, o, seg, &mut |at| on(visit, p, at));
                }
            }
            None => {
                for (relation, &p) in self.relations.iter().zip(&self.predicates) {
                    if !visit.skip() {
                        return;
                    }
                    relation.scan(s, o, seg, &mut |at| on(visit, p, at));
                }
            }
        }
    }

    /// Each relation asked about counted as it is asked, and each chunk range it reads
    /// (review r1 F1, F2); a sum its stop cut short is `Err` (fix check r1, B9).
    fn size(
        &self,
        [s, p, o]: [Option<u64>; 3],
        seg: Seg,
        probe: Probe<'_, '_>,
    ) -> Result<usize, Stopped> {
        let size = match p {
            Some(p) => self.relation(p).map_or(0, |r| r.estimate(s, o, seg, probe)),
            None => probe
                .counted(&self.relations)
                .map(|r| r.estimate(s, o, seg, probe))
                .sum(),
        };
        probe.ended()?;
        Ok(size)
    }

    /// One search in the relation's runs: the call's count is its work.
    fn holds(&self, [s, p, o]: Triple, _: Probe<'_, '_>) -> Result<bool, Stopped> {
        Ok(self.relation(p).is_some_and(|r| r.contains(s, o)))
    }

    fn positions(
        &self,
        [s, p, o]: [Option<u64>; 3],
        seg: Seg,
        probe: Probe<'_, '_>,
    ) -> Result<Option<usize>, Stopped> {
        // Each relation asked about, and each slice it holds of the matches, counted as it is
        // made (review r1 F1, F2; fix check r1 R2-F2: the slices are made one at a time); a
        // length its stop cut short is `Err` (B9).
        let positions = probe
            .counted(self.relations_of(p))
            .map(|(_, relation)| {
                let slices = relation.matching(s, o, seg)?;
                Some(
                    probe
                        .counted(slices)
                        .map(|slice| slice.pairs.len())
                        .sum::<usize>(),
                )
            })
            .sum();
        probe.ended()?;
        Ok(positions)
    }

    fn visit_range(
        &self,
        [s, p, o]: [Option<u64>; 3],
        seg: Seg,
        range: std::ops::Range<usize>,
        visit: &mut Visit<'_, '_>,
    ) {
        // The range's positions were counted ahead, filtered ones included (REAS sol-loop
        // r7): each is delivered or skipped within that count. The relations and slices
        // passed on the way to it are counted as they are made (fix check r1, R2-F2: a
        // range far into one key's chunks passes every slice before it).
        let poll = visit.poller();
        let mut at = 0;
        'relations: for (p, relation) in poll.counted(self.relations_of(p)) {
            let slices = relation
                .matching(s, o, seg)
                .expect("matches_len gave slices");
            for slice in poll.counted(slices) {
                let (start, end) = (at, at + slice.pairs.len());
                at = end;
                if end <= range.start {
                    continue;
                }
                if start >= range.end {
                    break 'relations;
                }
                let part = range.start.max(start) - start..range.end.min(end) - start;
                if slice.old {
                    relation
                        .old_checks
                        .fetch_add(part.len() as u64, std::sync::atomic::Ordering::Relaxed);
                }
                let mut going = true;
                slice.for_each_in(part, |(a, b)| {
                    // Stopped by a read of the reader's: the rest of the range is passed over.
                    if !going {
                        return;
                    }
                    let (s, o) = if slice.swapped { (b, a) } else { (a, b) };
                    going = match slice.old && relation.delta.contains(s, o) {
                        true => visit.skip(),
                        false => visit.fact([s, p, o]),
                    };
                });
                if !going {
                    return;
                }
            }
        }
        if poll.stopped() {
            return;
        }
        // Marked pairs a slice passes over were counted ahead too.
        visit.settle();
    }
}

/// The equality module, batch form (design §4.4): replaces `eq-rep-s/p/o`. The generic
/// rules copy a fact to each `sameAs` partner of its subject, predicate or object, and
/// every copy is copied again in the next round: O(k²) derivations per fact for a class
/// of k members. Here each new fact is expanded once to every combination of its terms'
/// classes (a term's class is the term plus its `sameAs` partners; the relation is kept
/// closed by `eq-sym` and the transitive module). A new `sameAs` pair re-expands the facts
/// that mention either side, so classes that grow later are covered.
pub(crate) struct Equality {
    same_as: u64,
    /// The module's output of the last round: already expanded, so skipped once.
    produced: HashSet<Triple>,
    /// Scans for a term's partners so far (three per expanded fact).
    pub(crate) member_scans: u64,
    /// The terms with a partner, once a round's delta was large enough to collect them
    /// all; kept current from each later round's `sameAs` facts.
    partners: Option<TermSet>,
}

/// The `owl:sameAs` id of `rules`, if they reason with equality (`eq-rep-s`).
pub(crate) fn same_as_of(rules: &[Rule]) -> Option<u64> {
    Equality::for_rules(rules).map(|equality| equality.same_as)
}

/// The rules the equality module replaces.
pub(crate) fn is_replacement_rule(rule: &Rule) -> bool {
    matches!(rule.name.as_str(), "eq-rep-s" | "eq-rep-p" | "eq-rep-o")
}

impl Equality {
    /// The module for `rules`, if they include `eq-rep-s` (whose first atom names
    /// `owl:sameAs`).
    pub(crate) fn for_rules(rules: &[Rule]) -> Option<Self> {
        let rule = rules.iter().find(|r| r.name == "eq-rep-s")?;
        match rule.body.first()?.0[1] {
            super::ir::Term::Const(same_as) => Some(Self {
                same_as,
                produced: HashSet::new(),
                member_scans: 0,
                partners: None,
            }),
            super::ir::Term::Var(_) => None,
        }
    }

    /// The class of `x`: itself and its `sameAs` partners, sorted; [`Stopped`] where its
    /// poller stopped the read, never a part of the class (fix check r1, B9).
    fn members<S: Source + ?Sized>(
        &self,
        store: &S,
        x: u64,
        poll: &Poller<'_>,
    ) -> Result<Vec<u64>, Stopped> {
        let mut members = vec![x];
        store.scan(
            [Some(x), Some(self.same_as), None],
            Seg::All,
            poll,
            &mut |t| {
                members.push(t[2]);
            },
        )?;
        members.sort_unstable();
        members.dedup();
        Ok(members)
    }

    /// The expansions of the delta of `store` (see the type's docs), not yet in it.
    /// [`Interrupted`] for its memory, `operation` refused, where they don't fit what it
    /// may still take at the round's start ([`OperationStop::room`]), charged before each
    /// allocation of them; and once `operation`'s stop (latched) has fired. Its reads count
    /// on one poller for its sequential part and one per parallel task for the expansions,
    /// each fact's three classes and their product of probes (sweep M10). Stopped, its
    /// state (`partners`) may be part-read: the module belongs to the operation, which ends
    /// with the stop.
    pub(crate) fn run<S: Visits + ?Sized>(
        &mut self,
        store: &S,
        operation: &OperationStop<'_>,
    ) -> Result<Vec<Triple>, Interrupted> {
        let stop: super::eval::Stop<'_> = &|| operation.fired();
        let poll = Poller::new(stop).ok_or_else(|| operation.interrupted())?;
        // A read its poller stopped ends the module (fix check r1, rule 2).
        let stopped = |Stopped| operation.interrupted();
        let equalities = store
            .estimate([None, Some(self.same_as), None], Seg::All, &poll)
            .map_err(stopped)?;
        if equalities == 0 {
            self.produced.clear();
            return Ok(Vec::new());
        }
        let same_as = self.same_as;
        // A fact mentioning no term with a partner expands to itself only (#12 of the
        // investigation of 6 October 2026). Which terms have one: for a small delta (a
        // commit's, beside a store's many equalities) each term of it is looked up; for
        // a large one (the batch executor's rounds) they are collected once from all
        // `sameAs` facts and then kept current from each round's.
        if self.partners.is_none()
            && store
                .estimate([None, None, None], Seg::Delta, &poll)
                .map_err(stopped)?
                * 8
                >= equalities
        {
            let mut all = TermSet::default();
            store
                .scan(
                    [None, Some(same_as), None],
                    Seg::All,
                    &poll,
                    &mut |[s, _, o]| {
                        if s != o {
                            all.insert(s);
                            all.insert(o);
                        }
                    },
                )
                .map_err(stopped)?;
            self.partners = Some(all);
        } else if let Some(partners) = &mut self.partners {
            store
                .scan(
                    [None, Some(same_as), None],
                    Seg::Delta,
                    &poll,
                    &mut |[s, _, o]| {
                        if s != o {
                            partners.insert(s);
                            partners.insert(o);
                        }
                    },
                )
                .map_err(stopped)?;
        }
        let mut looked_up: HashMap<u64, bool> = HashMap::new();
        let mut has_partner = |term: u64| -> Result<bool, Stopped> {
            match &self.partners {
                Some(partners) => Ok(partners.contains(term)),
                None => {
                    if let Some(&known) = looked_up.get(&term) {
                        return Ok(known);
                    }
                    // An upper bound: zero means no equality mentions the term.
                    let found =
                        store.estimate([Some(term), Some(same_as), None], Seg::All, &poll)? > 0
                            || store.estimate(
                                [None, Some(same_as), Some(term)],
                                Seg::All,
                                &poll,
                            )? > 0;
                    looked_up.insert(term, found);
                    Ok(found)
                }
            }
        };
        let mut seeds: Vec<Triple> = Vec::new();
        let mut merged: Vec<u64> = Vec::new();
        let produced = &self.produced;
        // A lookup its poller stopped seeds nothing: the scan ends at its next position and
        // answers `Stopped`.
        store
            .scan([None, None, None], Seg::Delta, &poll, &mut |t| {
                if t[1] == same_as {
                    if t[0] != t[2] {
                        merged.push(t[0]);
                    }
                } else if t.iter().any(|&term| has_partner(term) == Ok(true))
                    && !produced.contains(&t)
                {
                    seeds.push(t);
                }
            })
            .map_err(stopped)?;
        // Facts mentioning a term whose class grew.
        merged.sort_unstable();
        merged.dedup();
        for &a in &merged {
            let mut push = |t: Triple| {
                if t[1] != same_as {
                    seeds.push(t);
                }
            };
            for pattern in [
                [Some(a), None, None],
                [None, None, Some(a)],
                [None, Some(a), None],
            ] {
                store
                    .scan(pattern, Seg::All, &poll, &mut push)
                    .map_err(stopped)?;
            }
        }
        if poll.stopped() {
            return Err(operation.interrupted());
        }
        seeds.par_sort_unstable();
        seeds.dedup();
        self.member_scans += 3 * seeds.len() as u64;
        let room = room_budget(operation)?;
        let refused = |_: BudgetExceeded| operation.refuse(MemoryStopCause::OverLimit);
        // Each thread's copies, charged a seed at a time before they are made, then moved
        // into one list once; its reads count on the thread's own poller.
        let chunks = seeds
            .par_iter()
            .try_fold(
                || {
                    (
                        ChargedVec::new(&room),
                        ChargedVec::new(&room),
                        Poller::new(stop),
                    )
                },
                // The fold's error: `Some` a refusal, `None` a stop. Either ends the thread's
                // fold (fix check r1, rule 2: a stopped thread passed over each later seed).
                |(mut copies, mut scratch, poll),
                 &[s, p, o]|
                 -> Result<_, Option<BudgetExceeded>> {
                    if let Some(poll) = poll.as_ref().filter(|poll| !poll.stopped()) {
                        let (ms, mp, mo) = (
                            self.members(store, s, poll).map_err(|Stopped| None)?,
                            self.members(store, p, poll).map_err(|Stopped| None)?,
                            self.members(store, o, poll).map_err(|Stopped| None)?,
                        );
                        let bound = copies_at_most([&ms, &mp, &mo]);
                        // The product of its classes, a probe each, counted ahead a run of at
                        // most POLL_UNITS objects at a time ([`Visit::ahead`]'s rule): stopped,
                        // it ends there (sol-loop r1 S1), and the operation ends with what it
                        // made. A tick and a stop check per probe were a quarter of the read
                        // layer's cost on replication (REAS-2 timing, p2-reas2-abl-v1).
                        let probe = Probe::of(poll);
                        append_copies(&mut copies, &mut scratch, bound, |out| {
                            for &s in &ms {
                                for &p in &mp {
                                    for objects in mo.chunks(RUN) {
                                        if poll.attempt(objects.len()) {
                                            return;
                                        }
                                        for &o in objects {
                                            let fact = [s, p, o];
                                            match store.holds(fact, probe) {
                                                Ok(false) => out.push(fact),
                                                Ok(true) => {}
                                                Err(Stopped) => return,
                                            }
                                        }
                                    }
                                }
                            }
                        })
                        .map_err(Some)?;
                        if poll.stopped() {
                            return Err(None);
                        }
                    } else {
                        return Err(None);
                    }
                    Ok((copies, scratch, poll))
                },
            )
            .map(|folded| folded.map(|(copies, _scratch, _poll)| copies))
            .collect::<Result<Vec<_>, _>>()
            .map_err(|ended| match ended {
                Some(exceeded) => refused(exceeded),
                None => operation.interrupted(),
            })?;
        // A thread that stopped left facts out; the stop is latched, so asking it tells.
        if stop() {
            return Err(operation.interrupted());
        }
        let out = concatenated(&room, chunks).map_err(refused)?;
        // The round's set of them, charged before it is made: under three slots a fact (a
        // power of two, filled to 7/8), each a fact and a control byte.
        let set_bytes = out.len().checked_mul(3 * (size_of::<Triple>() + 1));
        if !set_bytes.is_some_and(|bytes| room.charge(bytes).is_ok()) {
            return Err(operation.refuse(MemoryStopCause::OverLimit));
        }
        let (out, _held) = out.into_parts();
        self.produced = out.iter().copied().collect();
        Ok(out)
    }
}

/// A set of terms with a bitmap in front: most terms are in no such set, and a bit test
/// answers for them without hashing (a term per position of every fact read).
#[derive(Default)]
struct TermSet {
    bits: Vec<u64>,
    terms: HashSet<u64>,
}

impl TermSet {
    /// Bits in the bitmap (8 KiB).
    const BITS: usize = 1 << 16;

    fn slot(term: u64) -> usize {
        (term.wrapping_mul(0x9E37_79B9_7F4A_7C15) >> 48) as usize
    }

    fn insert(&mut self, term: u64) {
        if self.bits.is_empty() {
            self.bits = vec![0; Self::BITS / 64];
        }
        let slot = Self::slot(term);
        self.bits[slot / 64] |= 1 << (slot % 64);
        self.terms.insert(term);
    }

    fn contains(&self, term: u64) -> bool {
        if self.bits.is_empty() {
            return false;
        }
        let slot = Self::slot(term);
        self.bits[slot / 64] & (1 << (slot % 64)) != 0 && self.terms.contains(&term)
    }

    /// Whether a term of `fact` is in the set.
    fn mentioned(&self, [s, p, o]: Triple) -> bool {
        self.contains(s) || self.contains(p) || self.contains(o)
    }

    fn is_empty(&self) -> bool {
        self.terms.is_empty()
    }
}

/// Equality by representatives within the rounds ([`super::representatives`]): the
/// classes so far, and the terms that lost their place as representative. A fact that
/// mentions one is stale: its rewrite is in the store too, and the rules, grounding and
/// the listing read only current facts ([`Current`]).
struct Representatives {
    same_as: u64,
    classes: EqualityClasses,
    /// Former representatives.
    stale: TermSet,
    /// The representatives of classes of two or more.
    shared: TermSet,
    listing: Listing,
    merges: usize,
}

impl Representatives {
    /// Merges the classes `pairs` equate; returns the terms that lost their place.
    fn merge(&mut self, pairs: &[(u64, u64)]) -> HashSet<u64> {
        let lost: HashSet<u64> = self.classes.union_all(pairs).into_iter().collect();
        if !lost.is_empty() {
            self.merges += 1;
            for &term in &lost {
                self.stale.insert(term);
            }
            for (representative, _) in self.classes.classes() {
                if !self.shared.terms.contains(&representative) {
                    self.shared.insert(representative);
                }
            }
        }
        lost
    }

    /// The facts the merge that took the places of `lost` makes stale, rewritten; `None`
    /// if `stop` (latched) fired ([`Store::mentioning`]).
    fn rewrites(
        &self,
        store: &Store,
        lost: &HashSet<u64>,
        stop: super::eval::Stop<'_>,
    ) -> Option<Vec<Triple>> {
        let mut facts = store.mentioning(lost, stop)?;
        facts
            .par_iter_mut()
            .for_each(|fact| *fact = self.classes.rewrite(*fact));
        Some(facts)
    }
}

/// `rule` with every constant replaced by its representative.
fn rewrite_rule(rule: &Rule, classes: &EqualityClasses) -> Rule {
    let term = |t: Term| match t {
        Term::Const(c) => Term::Const(classes.representative(c)),
        variable => variable,
    };
    let atom = |a: &super::ir::Atom| super::ir::Atom(a.0.map(term));
    Rule {
        name: rule.name.clone(),
        body: rule.body.iter().map(atom).collect(),
        guards: rule.guards.clone(),
        head: match &rule.head {
            Head::Facts(atoms) => Head::Facts(atoms.iter().map(atom).collect()),
            Head::Inconsistent => Head::Inconsistent,
        },
    }
}

/// Whether a constant of `rule` is in `terms`.
fn names_any(rule: &Rule, terms: &HashSet<u64>) -> bool {
    let atoms = match &rule.head {
        Head::Facts(atoms) => rule.body.iter().chain(atoms),
        Head::Inconsistent => rule.body.iter().chain(&[]),
    };
    atoms
        .flat_map(|atom| atom.0)
        .any(|t| matches!(t, Term::Const(c) if terms.contains(&c)))
}

/// The store as the rules read it: without stale facts (a former representative in them).
struct Current<'a> {
    store: &'a Store,
    stale: &'a TermSet,
}

/// Reads are the store's ([`Visits for Store`](Store)): its positions are this source's,
/// the stale ones it filters out included.
impl Visits for Current<'_> {
    fn visit(&self, pattern: [Option<u64>; 3], seg: Seg, visit: &mut Visit<'_, '_>) {
        if self.stale.is_empty() {
            return self.store.visit(pattern, seg, visit);
        }
        visit.filtered(&|fact| Ok(!self.stale.mentioned(fact)), |inner| {
            self.store.visit(pattern, seg, inner)
        });
    }

    fn size(
        &self,
        pattern: [Option<u64>; 3],
        seg: Seg,
        probe: Probe<'_, '_>,
    ) -> Result<usize, Stopped> {
        self.store.size(pattern, seg, probe)
    }

    fn holds(&self, fact: Triple, probe: Probe<'_, '_>) -> Result<bool, Stopped> {
        Ok(!self.stale.mentioned(fact) && self.store.holds(fact, probe)?)
    }

    fn positions(
        &self,
        pattern: [Option<u64>; 3],
        seg: Seg,
        probe: Probe<'_, '_>,
    ) -> Result<Option<usize>, Stopped> {
        self.store.positions(pattern, seg, probe)
    }

    fn visit_range(
        &self,
        pattern: [Option<u64>; 3],
        seg: Seg,
        range: std::ops::Range<usize>,
        visit: &mut Visit<'_, '_>,
    ) {
        if self.stale.is_empty() {
            return self.store.visit_range(pattern, seg, range, visit);
        }
        visit.filtered(&|fact| Ok(!self.stale.mentioned(fact)), |inner| {
            self.store.visit_range(pattern, seg, range, inner)
        });
    }
}

/// The transitive module, batch form: for each transitive predicate, its closure by SCC
/// condensation, recomputed in any round after other rules added facts over it.
#[derive(Default)]
struct Transitive {
    /// Each predicate with whether it needs a recomputation.
    predicates: std::collections::BTreeMap<u64, bool>,
    /// New facts each predicate's last recomputation produced.
    produced: HashMap<u64, usize>,
    /// The pairs read in the last [`Transitive::run`].
    read: u64,
}

impl Transitive {
    fn register(&mut self, p: u64) {
        self.predicates.entry(p).or_insert(true);
    }

    /// Every predicate needs recomputing: a merge of `sameAs` classes rewrote facts, so
    /// what the module produced says nothing about what the rules added.
    fn recompute_all(&mut self) {
        self.predicates.values_mut().for_each(|dirty| *dirty = true);
    }

    /// The closure facts of every predicate that needs it, not yet in `store`; `None` if
    /// `stop` fired.
    fn run(
        &mut self,
        store: &Store,
        representatives: Option<&Representatives>,
        stop: super::eval::Stop<'_>,
    ) -> Option<Vec<Triple>> {
        self.produced.clear();
        self.read = 0;
        let mut out = Vec::new();
        for (&p, dirty) in &mut self.predicates {
            if !std::mem::take(dirty) {
                continue;
            }
            let Some(relation) = store.relation(p) else {
                continue;
            };
            let before = out.len();
            let mut pairs = relation.pairs();
            self.read += pairs.len() as u64;
            if let Some(representatives) = representatives.filter(|r| !r.stale.is_empty()) {
                // Over current facts only (every term a representative), which closes them.
                let stale = &representatives.stale;
                if stale.contains(p) {
                    continue;
                }
                pairs.retain(|&(s, o)| !stale.contains(s) && !stale.contains(o));
                out.extend(
                    nrese_exec::graph::transitive_closure_until(&pairs, stop)?
                        .into_iter()
                        .filter(|&(s, o)| !relation.contains(s, o))
                        .map(|(s, o)| [s, p, o]),
                );
                self.produced.insert(p, out.len() - before);
                continue;
            }
            out.extend(
                nrese_exec::graph::transitive_closure_until(&pairs, stop)?
                    .into_iter()
                    .filter(|&(s, o)| !relation.contains(s, o))
                    .map(|(s, o)| [s, p, o]),
            );
            self.produced.insert(p, out.len() - before);
        }
        Some(out)
    }

    /// After a round: a predicate needs recomputing if other rules added facts over it.
    fn observe(&mut self, store: &Store) {
        for (p, dirty) in &mut self.predicates {
            let added = store.relation(*p).map_or(0, Relation::delta_len);
            *dirty |= added > self.produced.get(p).copied().unwrap_or(0);
        }
    }
}

/// The fact rules of `rules`, plus the list rules instantiated over `source`.
pub(crate) fn fact_rules<S: Source + ?Sized>(
    source: &S,
    rules: &[Rule],
    lists: Option<&ListVocabulary>,
    diagnostics: &mut Vec<super::lists::ListDiagnostic>,
) -> Vec<Rule> {
    let mut out: Vec<Rule> = rules
        .iter()
        .filter(|r| r.head != Head::Inconsistent)
        .cloned()
        .collect();
    if let Some(vocabulary) = lists {
        let (list_rules, found) = instantiate(vocabulary, &AllFacts(source));
        out.extend(
            list_rules
                .into_iter()
                .filter(|r| r.head != Head::Inconsistent),
        );
        *diagnostics = found;
    }
    out
}

/// The consistency rules of `rules`, plus the list ones instantiated over `source`.
pub(crate) fn consistency_rules<S: Source + ?Sized>(
    source: &S,
    rules: &[Rule],
    lists: Option<&ListVocabulary>,
) -> Vec<Rule> {
    let mut out: Vec<Rule> = rules
        .iter()
        .filter(|r| r.head == Head::Inconsistent)
        .cloned()
        .collect();
    if let Some(vocabulary) = lists {
        let (list_rules, _) = instantiate(vocabulary, &AllFacts(source));
        out.extend(
            list_rules
                .into_iter()
                .filter(|r| r.head == Head::Inconsistent),
        );
    }
    out
}

/// The closure of `input` under `rules` (plus the list rules when `lists` is given).
pub fn materialise(
    input: &[Triple],
    rules: &[Rule],
    lists: Option<&ListVocabulary>,
    schema: &Schema,
) -> Materialisation {
    materialise_owned(input.to_vec(), rules, lists, schema)
}

/// [`materialise`] taking the input by value, so the working set is its only copy.
pub fn materialise_owned(
    input: Vec<Triple>,
    rules: &[Rule],
    lists: Option<&ListVocabulary>,
    schema: &Schema,
) -> Materialisation {
    let clock = std::time::Instant::now();
    let store = Store::new(input);
    run(
        store,
        clock.elapsed(),
        rules,
        lists,
        schema,
        None,
        NEVER,
        &unlimited(),
    )
    .expect("never stopped")
}

/// [`materialise_owned`], polling `stop` as [`materialise_grouped_until`] does.
pub fn materialise_owned_until(
    input: Vec<Triple>,
    rules: &[Rule],
    lists: Option<&ListVocabulary>,
    schema: &Schema,
    stop: super::eval::Stop<'_>,
) -> Result<Materialisation, Interrupted> {
    materialise_owned_with(input, rules, lists, schema, &OperationStop::new(stop))
}

/// [`materialise_owned_until`] under one operation's `stop`, whose memory limit its caller
/// chose ([`OperationStop::with_poll`]).
pub fn materialise_owned_with(
    input: Vec<Triple>,
    rules: &[Rule],
    lists: Option<&ListVocabulary>,
    schema: &Schema,
    stop: &OperationStop<'_>,
) -> Result<Materialisation, Interrupted> {
    let clock = std::time::Instant::now();
    let store = Store::new(input);
    run_latched(
        store,
        clock.elapsed(),
        rules,
        lists,
        schema,
        None,
        stop,
        &unlimited(),
    )
}

/// [`materialise`] over input already grouped the way the working set stores it: per
/// predicate (each once), its `(object, subject)` pairs, sorted and distinct. A store can
/// stream this from a predicate-object-subject index without an intermediate triple list or
/// sort, holding about 32 bytes per input fact instead of about 72.
pub fn materialise_grouped(
    input: Vec<(u64, Vec<(u64, u64)>)>,
    rules: &[Rule],
    lists: Option<&ListVocabulary>,
    schema: &Schema,
) -> Materialisation {
    materialise_grouped_until(input, rules, lists, schema, NEVER).expect("never stopped")
}

/// [`materialise_grouped`], polling `stop` in every round, rule job and module; a fired
/// `stop` ends it with [`Interrupted`] and nothing of the partial closure.
pub fn materialise_grouped_until(
    input: Vec<(u64, Vec<(u64, u64)>)>,
    rules: &[Rule],
    lists: Option<&ListVocabulary>,
    schema: &Schema,
    stop: super::eval::Stop<'_>,
) -> Result<Materialisation, Interrupted> {
    materialise_grouped_within(input, rules, lists, schema, stop, &unlimited())
}

/// [`materialise_grouped_until`], its consistency violations charged to `memory` (the
/// operation's own part: a refusal ends their list, never the materialisation).
pub fn materialise_grouped_within(
    input: Vec<(u64, Vec<(u64, u64)>)>,
    rules: &[Rule],
    lists: Option<&ListVocabulary>,
    schema: &Schema,
    stop: super::eval::Stop<'_>,
    memory: &Arc<Budget>,
) -> Result<Materialisation, Interrupted> {
    let stop = OperationStop::new(stop);
    materialise_grouped_with(input, rules, lists, schema, &stop, memory)
}

/// [`materialise_grouped_within`] under one operation's `stop`, whose memory limit and room
/// its caller chose ([`OperationStop::within`]): replication's copies are admitted by its
/// room.
pub fn materialise_grouped_with(
    input: Vec<(u64, Vec<(u64, u64)>)>,
    rules: &[Rule],
    lists: Option<&ListVocabulary>,
    schema: &Schema,
    stop: &OperationStop<'_>,
    memory: &Arc<Budget>,
) -> Result<Materialisation, Interrupted> {
    let clock = std::time::Instant::now();
    let store = Store::from_groups(input);
    run_latched(
        store,
        clock.elapsed(),
        rules,
        lists,
        schema,
        None,
        stop,
        memory,
    )
}

/// The closure of `input` under `rules` with equality by representatives
/// ([`super::representatives`]): each `owl:sameAs` class one term, its smallest id, the
/// replacement rules (`eq-rep-*`) and `eq-sym`/`eq-trans` replaced by the classes
/// themselves. A round's new `sameAs` between two representatives merges their classes,
/// and the facts mentioning the representative that lost its place are rewritten into the
/// next round's delta: no outer loop, however long a cascade of merges. `listing` says
/// what [`Materialisation::derived`] holds; rules without equality reasoning give the
/// plain closure.
pub fn materialise_representatives_until(
    input: Input,
    rules: &[Rule],
    lists: Option<&ListVocabulary>,
    schema: &Schema,
    listing: Listing,
    stop: super::eval::Stop<'_>,
) -> Result<Materialisation, Interrupted> {
    materialise_representatives_within(input, rules, lists, schema, listing, stop, &unlimited())
}

/// [`materialise_representatives_until`], its consistency violations charged to `memory`
/// (as [`materialise_grouped_within`]).
pub fn materialise_representatives_within(
    input: Input,
    rules: &[Rule],
    lists: Option<&ListVocabulary>,
    schema: &Schema,
    listing: Listing,
    stop: super::eval::Stop<'_>,
    memory: &Arc<Budget>,
) -> Result<Materialisation, Interrupted> {
    let stop = OperationStop::new(stop);
    materialise_representatives_with(input, rules, lists, schema, listing, &stop, memory)
}

/// [`materialise_representatives_within`] under one operation's `stop`, whose memory limit
/// and room its caller chose ([`OperationStop::within`]): the expanded listing's copies
/// are admitted by its room.
pub fn materialise_representatives_with(
    input: Input,
    rules: &[Rule],
    lists: Option<&ListVocabulary>,
    schema: &Schema,
    listing: Listing,
    stop: &OperationStop<'_>,
    memory: &Arc<Budget>,
) -> Result<Materialisation, Interrupted> {
    let clock = std::time::Instant::now();
    let store = match input {
        Input::Facts(facts) => Store::new(facts),
        Input::Grouped(groups) => Store::from_groups(groups),
    };
    let representatives = super::representatives::same_as(rules).map(|same_as| Representatives {
        same_as,
        classes: EqualityClasses::default(),
        stale: TermSet::default(),
        shared: TermSet::default(),
        listing,
        merges: 0,
    });
    if representatives.is_none() && listing == Listing::Representatives {
        // Every fact: the input's too.
        let input_facts = store.facts_where(&|_| true);
        let mut result = run_latched(
            store,
            clock.elapsed(),
            rules,
            lists,
            schema,
            None,
            stop,
            memory,
        )?;
        result.derived.extend(input_facts);
        result.derived.par_sort_unstable();
        result.derived.dedup();
        return Ok(result);
    }
    run_latched(
        store,
        clock.elapsed(),
        rules,
        lists,
        schema,
        representatives,
        stop,
        memory,
    )
}

/// The rules equality by representatives runs: without the ones the classes stand for.
fn under_representatives(rules: &[Rule]) -> Vec<Rule> {
    rules
        .iter()
        .filter(|r| !is_replacement_rule(r) && !matches!(r.name.as_str(), "eq-sym" | "eq-trans"))
        .cloned()
        .collect()
}

/// [`over_classes`] on `rules`, sharing the classes as they are now when it first changes
/// a rule and makes them (later merges replace them).
fn give_classes(
    rules: &mut [Rule],
    reps: &Representatives,
    classes: &mut Option<SharedClasses>,
    guarded: &mut Guarded,
) {
    let first = classes.is_none();
    let now = over_classes(rules, reps.same_as, classes);
    if first && let Some(classes) = classes {
        classes.set(&reps.classes);
    }
    guarded.facts |= now.facts;
}

/// Whether the rule's guards see the classes ([`Guard::OverClasses`]).
fn sees_classes(rule: &Rule) -> bool {
    matches!(rule.guards.as_slice(), [Guard::OverClasses { .. }])
}

/// Semi-naive evaluation to the fixpoint, from a store holding the input as its delta;
/// with equality by representatives if `representatives` is given. The process's memory
/// limit stops it too, whatever the caller polls. Its consistency violations are charged
/// to `memory`.
#[allow(clippy::too_many_arguments)]
fn run(
    store: Store,
    load: std::time::Duration,
    rules: &[Rule],
    lists: Option<&ListVocabulary>,
    schema: &Schema,
    representatives: Option<Representatives>,
    stop: super::eval::Stop<'_>,
    memory: &Arc<Budget>,
) -> Result<Materialisation, Interrupted> {
    let stop = super::eval::OperationStop::new(stop);
    run_latched(
        store,
        load,
        rules,
        lists,
        schema,
        representatives,
        &stop,
        memory,
    )
}

/// [`run`] under one operation's `stop`: once it fires the run ends, with its cause, and
/// no morsel it made skip is taken for done.
#[allow(clippy::too_many_arguments)]
fn run_latched(
    store: Store,
    load: std::time::Duration,
    rules: &[Rule],
    lists: Option<&ListVocabulary>,
    schema: &Schema,
    representatives: Option<Representatives>,
    stop: &super::eval::OperationStop<'_>,
    memory: &Arc<Budget>,
) -> Result<Materialisation, Interrupted> {
    run_body(
        store,
        load,
        rules,
        lists,
        schema,
        representatives,
        stop,
        memory,
    )
    .map_err(|_| stop.interrupted())
}

/// [`run`]'s work, under the operation's latched stop, `operation`.
#[allow(clippy::too_many_arguments)]
fn run_body(
    mut store: Store,
    load: std::time::Duration,
    rules: &[Rule],
    lists: Option<&ListVocabulary>,
    schema: &Schema,
    mut representatives: Option<Representatives>,
    operation: &OperationStop<'_>,
    memory: &Arc<Budget>,
) -> Result<Materialisation, Interrupted> {
    let stop: super::eval::Stop<'_> = &|| operation.fired();
    let check_stop = || {
        if stop() {
            Err(Interrupted::default())
        } else {
            Ok(())
        }
    };
    let mut phases = Phases {
        load,
        passes: 1,
        ..Phases::default()
    };
    let mut result = Materialisation::default();
    let mut program = GroundProgram::default();
    let mut transitive = Transitive::default();
    // Equality by copying (the module), unless it is by representatives.
    let mut equality = Equality::for_rules(rules).filter(|_| representatives.is_none());
    let mut rules: std::borrow::Cow<'_, [Rule]> = match &representatives {
        Some(_) => under_representatives(rules).into(),
        None => rules.into(),
    };
    let mut regrounding = true;
    // Ground every rule again (rules whose constants were merged, rewritten).
    let mut reground_all = false;
    // Over representatives: the classes the rules' guards see, made with the first rule
    // given them (super::representatives::over_classes), and whether one concludes facts.
    // A merge can make such a guard hold for facts already joined, so the rules concluding
    // facts run in full after one.
    let mut guard_classes: Option<SharedClasses> = None;
    let mut guarded = Guarded::default();
    let mut reevaluate = false;
    // Facts to add in the first round, checked against the store: the input's stale
    // facts rewritten, where the input asserts equalities.
    let mut pending: Vec<Triple> = Vec::new();
    if let Some(reps) = &mut representatives {
        // The input's equalities, read as the rounds' sequential stages read: on a poller
        // of the operation's stop, its result checked before use.
        let poll = Poller::new(stop).ok_or_else(Interrupted::default)?;
        let mut pairs = Vec::new();
        store
            .scan(
                [None, Some(reps.same_as), None],
                Seg::All,
                &poll,
                &mut |[s, _, o]| {
                    if s != o {
                        pairs.push((s, o));
                    }
                },
            )
            .map_err(|Stopped| Interrupted::default())?;
        check_stop()?;
        let lost = reps.merge(&pairs);
        if !lost.is_empty() {
            pending = reps
                .rewrites(&store, &lost, stop)
                .ok_or_else(Interrupted::default)?;
            if rules.iter().any(|rule| names_any(rule, &lost)) {
                rules = rules
                    .iter()
                    .map(|r| rewrite_rule(r, &reps.classes))
                    .collect();
            }
        }
    }
    let none = TermSet::default();
    loop {
        check_stop()?;
        // The round's sequential reads (the object orders' estimate, the bodiless facts'
        // and pending facts' probes, job construction) count on this; each stage checks the
        // latched stop before its output is used.
        let stage = Poller::new(stop).ok_or_else(Interrupted::default)?;
        result.rounds += 1;
        // Where asked (budgets.reasoning_release), what the last round freed (its
        // candidates, the runs a fold replaced) goes back before this one allocates: a
        // lower, steadier peak for more time (nrese_engine::memory).
        nrese_engine::memory::release_between_rounds();
        heap::phase("reasoner: grounding");
        let clock = std::time::Instant::now();
        let stale = representatives.as_ref().map_or(&none, |r| &r.stale);
        // Rules from `evaluated` on were added this round: evaluated once in full.
        let evaluated = program.rules.len();
        if regrounding || reground_all || reevaluate {
            let current = Current {
                store: &store,
                stale,
            };
            let mut source = fact_rules(&current, &rules, lists, &mut result.diagnostics);
            if equality.is_some() {
                source.retain(|r| !is_replacement_rule(r));
            }
            if let Some(reps) = &representatives {
                give_classes(&mut source, reps, &mut guard_classes, &mut guarded);
            }
            // The first round grounds everything; later ones through new schema facts
            // only (list rules in full, deduplicated).
            if result.rounds == 1 || reground_all {
                program.ground(&current, schema, &source);
            } else {
                program.ground_delta(&current, schema, &source);
                if reevaluate {
                    // After a merge, a guard that sees the classes can hold for an instance
                    // grounding left out: those rules ground again in full.
                    source.retain(sees_classes);
                    program.ground(&current, schema, &source);
                }
            }
            reground_all = false;
            for &p in &program.transitive {
                transitive.register(p);
            }
            for (&p, &family) in &program.family_relations {
                store.claim(p, family);
            }
        }
        // The object orders the rules can use, built where they became needed and
        // dropped where nothing reads them (P1-F11).
        let equal = match &equality {
            Some(e) => {
                store
                    .estimate([None, Some(e.same_as), None], Seg::All, &stage)
                    .map_err(|Stopped| Interrupted::default())?
                    > 0
            }
            None => false,
        };
        // The orders are the store's state: built only from an answer that means something.
        check_stop()?;
        let reads = Reads::of(program.rules.iter());
        store.objects(&|p| equal || reads.by_object(p) || schema.read_in_grounding(p));
        phases.grounding += clock.elapsed();
        heap::phase("reasoner: joins");
        let clock = std::time::Instant::now();
        let current = Current {
            store: &store,
            stale,
        };
        // Heads naming a former representative (constants of rules grounded before it
        // lost its place) derive its representative's fact instead.
        let classes = representatives.as_ref().map(|r| &r.classes);
        let rewrite = |fact: Triple| match classes {
            Some(classes) if stale.mentioned(fact) => classes.rewrite(fact),
            _ => fact,
        };
        let rewrite: Option<&(dyn Fn(Triple) -> Triple + Sync)> =
            (!stale.is_empty()).then_some(&rewrite);
        // Semi-naive variants of the evaluated rules that can match the delta, full
        // evaluation of the new ones.
        let mut facts = program.take_facts();
        if let Some(rewrite) = rewrite {
            facts.iter_mut().for_each(|fact| *fact = rewrite(*fact));
        }
        keep_unheld(&store, &mut facts, &stage).map_err(|Stopped| Interrupted::default())?;
        let mut candidates: Vec<Candidates> = vec![(None, segments(facts))];
        keep_unheld(&store, &mut pending, &stage).map_err(|Stopped| Interrupted::default())?;
        candidates.push((None, segments(std::mem::take(&mut pending))));
        // Each job with the closed family its rule belongs to, if any: a variant of such
        // a rule reads the delta without what the family produced, and what a job of the
        // family derives goes to the family's part of the delta.
        let mut jobs = Vec::new();
        let mut families: Vec<Option<u64>> = Vec::new();
        // After a merge, the rules whose guards see the classes run in full.
        let again = |rule: &Rule| reevaluate && sees_classes(rule);
        for (r, i) in program.variants(&store, &stage) {
            if r < evaluated && !again(&program.rules[r]) {
                let family = program.closed_family(r);
                let delta = family.map_or(Seg::Delta, Seg::DeltaNotBy);
                let rule = &program.rules[r];
                if let Some(job) = Job::variant_reading(&current, rule, i, delta, &stage) {
                    jobs.push(job);
                    families.push(family);
                }
            }
        }
        for (r, rule) in program.rules.iter().enumerate().skip(evaluated) {
            if let Some(job) = Job::full(&current, rule, &stage) {
                jobs.push(job);
                families.push(program.closed_family(r));
            }
        }
        if reevaluate {
            for (r, rule) in program.rules.iter().enumerate().take(evaluated) {
                if sees_classes(rule)
                    && let Some(job) = Job::full(&current, rule, &stage)
                {
                    jobs.push(job);
                    families.push(program.closed_family(r));
                }
            }
            reevaluate = false;
        }
        let probes = Probes::for_jobs(jobs.len());
        let starts = std::sync::atomic::AtomicU64::new(0);
        // One probe per fact, counted as they are made ([`Store::retain_new`]). A stopped keep
        // leaves nothing, and the morsel's latched check after it ends the stage.
        let keep = |facts: &mut Vec<Triple>, poll: &Poller<'_>| {
            if let Ok(n) = store.retain_new(facts, poll) {
                starts.fetch_add(n, std::sync::atomic::Ordering::Relaxed);
            }
        };
        candidates.extend(run_jobs_by_morsel(
            &current,
            &jobs,
            &keep,
            rewrite,
            stop,
            &probes,
            &|job, facts| (families[job], split(facts)),
        ));
        let counters = &mut result.counters;
        counters.driver_bytes_copied += jobs.iter().map(Job::copied_bytes).sum::<usize>();
        let asked = probes.probes.into_inner();
        counters.probes.push(asked);
        counters.unordered_probes += probes.unordered.into_inner();
        counters.probe_starts += starts.into_inner();
        let mut round = 0;
        for (job, count) in jobs.iter().zip(probes.bindings) {
            let count = count.into_inner();
            round += count;
            match counters.bindings_by_rule.get_mut(&job.rule.name) {
                Some(total) => *total += count,
                None if count > 0 => {
                    counters
                        .bindings_by_rule
                        .insert(job.rule.name.clone(), count);
                }
                None => {}
            }
        }
        counters.bindings.push(round);
        phases.probes += asked;
        phases.bindings += probes.emitted.into_inner();
        drop(jobs);
        check_stop()?;
        phases.joins += clock.elapsed();
        heap::phase("reasoner: modules");
        let clock = std::time::Instant::now();
        candidates.push((
            None,
            segments(
                transitive
                    .run(&store, representatives.as_ref(), stop)
                    .ok_or_else(Interrupted::default)?,
            ),
        ));
        result.counters.closure_pairs.push(transitive.read);
        if let Some(equality) = &mut equality {
            candidates.push((None, segments(equality.run(&store, operation)?)));
        }
        let mut merged = false;
        // New equalities merge classes: the round's candidates and the stored facts that
        // mention a representative that lost its place are rewritten (egglog's rebuild,
        // within the round). The other candidates stay as they are, in their lists.
        if let Some(reps) = &mut representatives {
            let same_as = reps.same_as;
            let pairs: Vec<(u64, u64)> = candidates
                .iter()
                .flat_map(|(_, segments)| segments)
                .filter(|(p, _)| *p == same_as)
                .flat_map(|(_, pairs)| pairs.iter().copied().filter(|(s, o)| s != o))
                .collect();
            let lost = reps.merge(&pairs);
            if !lost.is_empty() {
                let mut moved: Vec<Triple> = Vec::new();
                for (_, list) in &mut candidates {
                    for (p, pairs) in list.iter_mut() {
                        let p = *p;
                        if lost.contains(&p) {
                            moved.extend(pairs.drain(..).map(|(s, o)| [s, p, o]));
                        } else {
                            // Sorted and distinct still.
                            pairs.retain(|&(s, o)| {
                                let stale = lost.contains(&s) || lost.contains(&o);
                                if stale {
                                    moved.push([s, p, o]);
                                }
                                !stale
                            });
                        }
                    }
                }
                moved.extend(
                    reps.rewrites(&store, &lost, stop)
                        .ok_or_else(Interrupted::default)?,
                );
                moved
                    .par_iter_mut()
                    .for_each(|fact| *fact = reps.classes.rewrite(*fact));
                keep_unheld(&store, &mut moved, &stage)
                    .map_err(|Stopped| Interrupted::default())?;
                // Rewritten: no longer what a family produced.
                candidates.push((None, segments(moved)));
                merged = true;
                if let Some(classes) = &guard_classes {
                    classes.set(&reps.classes);
                }
                reevaluate = guarded.facts;
                if rules.iter().any(|rule| names_any(rule, &lost)) {
                    rules = rules
                        .iter()
                        .map(|r| rewrite_rule(r, &reps.classes))
                        .collect();
                    reground_all = true;
                }
            }
        }
        check_stop()?;
        phases.modules += clock.elapsed();
        heap::phase("reasoner: merge");
        let clock = std::time::Instant::now();
        // Every candidate was checked against the store, which a round doesn't change.
        let (new, schema_facts) = store.advance_new(candidates, schema);
        phases.merge += clock.elapsed();
        phases.new_facts += new as u64;
        result.counters.store_bytes.push(store.bytes());
        if new == 0 && !reground_all && !reevaluate {
            break;
        }
        transitive.observe(&store);
        if merged {
            transitive.recompute_all();
        }
        regrounding = schema_facts;
    }
    result.ground_rules = program.rules.len();
    result.transitive = transitive.predicates.len();
    result.counters.member_scans = equality.as_ref().map_or(0, |e| e.member_scans);
    // And what the last round freed, before the check and what follows it allocate.
    nrese_engine::memory::release_between_rounds();
    heap::phase("reasoner: consistency");
    let clock = std::time::Instant::now();
    let stale = representatives.as_ref().map_or(&none, |r| &r.stale);
    {
        let current = Current {
            store: &store,
            stale,
        };
        let mut checks = consistency_rules(&current, &rules, lists);
        if let Some(reps) = &representatives {
            give_classes(&mut checks, reps, &mut guard_classes, &mut guarded);
        }
        program.ground(&current, schema, &checks);
    }
    let reads = Reads::of(
        program.rules.iter().chain(
            program
                .consistency
                .iter()
                .map(|(_, grounded)| &grounded.rule),
        ),
    );
    let stage = Poller::new(stop).ok_or_else(Interrupted::default)?;
    let equal = match &equality {
        Some(e) => {
            store
                .estimate([None, Some(e.same_as), None], Seg::All, &stage)
                .map_err(|Stopped| Interrupted::default())?
                > 0
        }
        None => false,
    };
    check_stop()?;
    store.objects(&|p| equal || reads.by_object(p) || schema.read_in_grounding(p));
    let mut found = Violations::new(Arc::clone(memory));
    check(
        &Current {
            store: &store,
            stale,
        },
        &program,
        0..program.consistency.len(),
        &[],
        &mut found,
        stop,
    )?;
    result.violations = found;
    result.counters.lookups_without_order = store
        .relations
        .iter()
        .map(|relation| relation.missed.load(std::sync::atomic::Ordering::Relaxed))
        .sum();
    result.counters.old_checks = store
        .relations
        .iter()
        .map(|relation| {
            relation
                .old_checks
                .load(std::sync::atomic::Ordering::Relaxed)
        })
        .sum();
    result.counters.merged_pairs = store.merged;
    phases.consistency = clock.elapsed();
    result.phases = phases;
    result.nil_merged = representatives
        .as_ref()
        .zip(lists)
        .is_some_and(|(reps, lists)| reps.classes.class_of(lists.nil()).is_some());
    heap::phase("reasoner: derived");
    let mut derived = match representatives {
        None => store.into_derived(),
        Some(reps) => {
            let derived = list(store, &reps, operation)?;
            result.classes = reps.classes;
            result.merges = reps.merges;
            derived
        }
    };
    derived.par_sort_unstable();
    result.derived = derived;
    heap::phase("reasoner: done");
    Ok(result)
}

/// What `operation` may still take now ([`OperationStop::room`]), as the budget the copies
/// of `owl:sameAs` classes are charged to before they are made; where it can't be read,
/// `operation` refused for that.
fn room_budget(operation: &OperationStop<'_>) -> Result<nrese_exec::Budget, Interrupted> {
    let room = operation.room().map_err(|cause| operation.refuse(cause))?;
    Ok(nrese_exec::Budget::new(
        usize::try_from(room).unwrap_or(usize::MAX),
    ))
}

/// The most copies a fact makes over its terms' classes `members` (each term's identities):
/// every combination of them. Charged once per fact before its copies are made, so the
/// loop that makes them only checks and writes: no charge per copy, which kept the store's
/// membership check out of line there (1.4 % more instructions in replication). A count no
/// vector can hold is refused by the charge.
fn copies_at_most(members: [&[u64]; 3]) -> usize {
    members.iter().fold(1, |product: usize, class| {
        product.saturating_mul(class.len())
    })
}

/// A thread's copies, charged to the operation's room.
pub(crate) type Copies<'a> = ChargedVec<Triple, &'a nrese_exec::Budget>;

/// One fact's copies, at most `bound` of them, that `make` writes, appended to the thread's
/// `copies`. `make` writes into `scratch`, the thread's room for one fact's copies, charged
/// for `bound` before it is lent and reused from fact to fact, so the loop that makes them
/// only checks and writes. `copies` then grows by what was made, by doubling as a list does:
/// lending it the bound itself kept room for copies not made (most of a fact's, where the
/// store holds them) beside the joined list (#91, `abba-pr91-cost-r2`: 1.81 times the
/// copies in replication's last round).
pub(crate) fn append_copies<'a>(
    copies: &mut Copies<'a>,
    scratch: &mut Copies<'a>,
    bound: usize,
    make: impl FnOnce(&mut nrese_exec::Room<'_, Triple>),
) -> Result<(), BudgetExceeded> {
    scratch.clear();
    scratch.fill(bound, make)?;
    copies.extend_from_slice(scratch)
}

/// Each thread's copies, `chunks`, in one list of exactly their number: its room charged
/// before it is made, beside theirs, then each chunk moved in and freed in turn, so every
/// copy moves once and the list never grows (a pairwise reduction would move them at each
/// of its levels and hold both halves beside their sum at the last). A full chunk alone is
/// the list as it is.
fn concatenated<'a>(
    room: &'a nrese_exec::Budget,
    mut chunks: Vec<ChargedVec<Triple, &'a nrese_exec::Budget>>,
) -> Result<ChargedVec<Triple, &'a nrese_exec::Budget>, BudgetExceeded> {
    chunks.retain(|chunk| !chunk.is_empty());
    if let [chunk] = chunks.as_slice()
        && chunk.len() == chunk.capacity()
    {
        return Ok(chunks.remove(0));
    }
    let copies = chunks.iter().map(|chunk| chunk.len()).sum();
    let mut list = ChargedVec::with_capacity(room, copies)?;
    for chunk in chunks {
        // Within the capacity charged: it doesn't grow.
        list.extend_from_slice(&chunk)?;
    }
    Ok(list)
}

/// What a materialisation with equality by representatives lists ([`Listing`]).
/// [`Interrupted`] for its memory, `operation` refused, where the expanded closure doesn't
/// fit what it may still take ([`OperationStop::room`]); and once `operation`'s stop
/// (latched) has fired while the expansion ran.
fn list(
    store: Store,
    reps: &Representatives,
    operation: &OperationStop<'_>,
) -> Result<Vec<Triple>, Interrupted> {
    let (stale, shared) = (&reps.stale, &reps.shared);
    let current = |fact: Triple| !stale.mentioned(fact);
    Ok(match reps.listing {
        Listing::Representatives => store.into_listed(&current, &current, Vec::new()),
        // Without classes, what was added.
        Listing::Stored | Listing::Expanded if reps.classes.is_empty() => store.into_derived(),
        Listing::Stored => store.into_listed(
            &|fact| current(fact) && shared.mentioned(fact),
            &current,
            Vec::new(),
        ),
        Listing::Expanded => {
            // The facts that mention a class, expanded while the input is there to leave
            // its facts out; the others as they are. Each thread's copies are charged a fact
            // at a time before they are made, then moved into one list once, which gets the
            // room for the others before it is handed on. Each class expansion is a product
            // of members, each copy a probe of the input: counted on the thread's poller
            // before it is made, from the same one class lookup per term (sweep M12).
            let facts = store.facts_where(&|fact| current(fact) && shared.mentioned(fact));
            let room = room_budget(operation)?;
            let refused = |_: BudgetExceeded| operation.refuse(MemoryStopCause::OverLimit);
            let stop: super::eval::Stop<'_> = &|| operation.fired();
            let chunks = facts
                .par_iter()
                .try_fold(
                    || {
                        (
                            ChargedVec::new(&room),
                            ChargedVec::new(&room),
                            Poller::new(stop),
                        )
                    },
                    |(mut copies, mut scratch, poll), fact| {
                        #[cfg(test)]
                        crate::witness::add(crate::witness::Site::ExpandedFact, 1);
                        let [ms, mp, mo] = fact.each_ref().map(|id| {
                            reps.classes
                                .class_of(*id)
                                .unwrap_or(std::slice::from_ref(id))
                        });
                        let bound = copies_at_most([ms, mp, mo]);
                        // Stopped, the thread makes no more copies. A fact's copies are
                        // counted as they are made, a run of at most POLL_UNITS objects at a
                        // time ([`Visit::ahead`]'s rule): a product counted whole ran its
                        // full length past a stop that fired while it was made.
                        if let Some(poll) = poll.as_ref().filter(|poll| !poll.stopped()) {
                            append_copies(&mut copies, &mut scratch, bound, |out| {
                                for &s in ms {
                                    for &p in mp {
                                        let relation = store.relation(p);
                                        for objects in mo.chunks(RUN) {
                                            if poll.attempt(objects.len()) {
                                                return;
                                            }
                                            for &o in objects {
                                                let input = relation.is_some_and(|relation| {
                                                    relation.input.contains(s, o)
                                                });
                                                if !input {
                                                    out.push([s, p, o]);
                                                }
                                            }
                                        }
                                    }
                                }
                            })?;
                        }
                        Ok((copies, scratch, poll))
                    },
                )
                .map(|folded| folded.map(|(copies, _scratch, _poll)| copies))
                .collect::<Result<Vec<_>, _>>()
                .map_err(refused)?;
            // A thread that stopped left copies out; the stop is latched, so asking it tells.
            if stop() {
                return Err(operation.interrupted());
            }
            let expanded = concatenated(&room, chunks).map_err(refused)?;
            // Expanded, they are freed before the list grows by the others.
            drop(facts);
            // What `into_listed` adds at most: the runs' facts. It adds none of the input's
            // (its `input` below takes none), so their room isn't asked for. It grows the
            // list to exactly that once it has freed the store's other parts: the buffer it
            // grows into is charged here, beside the list's, not allocated here; where the
            // list's capacity already holds them, it doesn't grow and nothing is charged.
            let others: usize = store
                .relations
                .iter()
                .map(|relation| relation.base.so.len() + relation.recent.so.len())
                .sum();
            let grown = expanded.len().checked_add(others);
            if grown.is_none_or(|facts| facts > expanded.capacity()) {
                let bytes = grown.and_then(|facts| facts.checked_mul(size_of::<Triple>()));
                if !bytes.is_some_and(|bytes| room.charge(bytes).is_ok()) {
                    return Err(operation.refuse(MemoryStopCause::OverLimit));
                }
            }
            let (expanded, _held) = expanded.into_parts();
            store.into_listed(
                &|_| false,
                &|fact| current(fact) && !shared.mentioned(fact),
                expanded,
            )
        }
    })
}

/// What ground rules read of the relations: which relations they can look up by object
/// alone, where the `(object, subject)` order is needed. An atom is looked up by object
/// alone if its object is a constant or a variable another atom binds, and its subject
/// isn't a constant; a variable predicate can be any relation. Every other lookup goes
/// by subject (both bound: a membership check by subject; nothing bound: a scan).
#[derive(Default)]
struct Reads {
    by_object: HashSet<u64>,
    everything: bool,
}

impl Reads {
    fn of<'a>(rules: impl Iterator<Item = &'a Rule>) -> Self {
        use super::ir::Term;
        let mut reads = Self::default();
        for rule in rules {
            for (i, atom) in rule.body.iter().enumerate() {
                let [subject, predicate, object] = atom.0;
                let bound = match object {
                    Term::Const(_) => true,
                    Term::Var(_) => rule
                        .body
                        .iter()
                        .enumerate()
                        .any(|(j, other)| j != i && other.0.contains(&object)),
                };
                if bound && !matches!(subject, Term::Const(_)) {
                    match predicate {
                        Term::Const(p) => {
                            reads.by_object.insert(p);
                        }
                        Term::Var(_) => reads.everything = true,
                    }
                }
            }
        }
        reads
    }

    fn by_object(&self, p: u64) -> bool {
        self.everything || self.by_object.contains(&p)
    }
}

/// Evaluates consistency instances into `found`: those in `full` over all facts, and the
/// `(instance, atom)` semi-naive variants in `variants` through the delta; then finishes
/// it (sorted, without duplicates). Polls `stop` between morsels of driver matches, and a
/// fired `stop` ends it with [`Interrupted`]: a stopped check is never a report. Once the
/// report refuses (its budget has no room), no further morsel runs: the state is
/// inconsistent whatever the rest would add.
pub(crate) fn check<S: Source + ?Sized>(
    source: &S,
    program: &GroundProgram,
    full: impl IntoIterator<Item = usize>,
    variants: &[(usize, usize)],
    found: &mut Violations,
    stop: super::eval::Stop<'_>,
) -> Result<(), Interrupted> {
    // The record's words: the job's bindings are sized for the grounded instance, which
    // lacks the schema variables when one of them has the highest index.
    fn words<'a>(
        program: &'a GroundProgram,
        c: usize,
        bindings: &'a [Option<u64>],
    ) -> impl Iterator<Item = u64> + 'a {
        let (rule, grounded) = &program.consistency[c];
        (0..rule.variables()).map(move |v| {
            grounded.substitution[v]
                .or_else(|| bindings.get(v).copied().flatten())
                .unwrap_or(0)
        })
    }
    let group_of = |c: usize, found: &mut Violations| {
        let rule = &program.consistency[c].0;
        found.group(&rule.name, rule.variables())
    };
    // One driver can have very many bindings (a rule over independent atoms), firing or
    // not, so the morsel alone is too coarse: the walk's reads poll `stop` per fact they
    // visit, and so do the jobs' constructions (sweep M01).
    let Some(poll) = Poller::new(stop) else {
        return Err(Interrupted::default());
    };
    let mut jobs: Vec<(usize, Job<'_>)> = Vec::new();
    for c in full {
        let ground_rule = &program.consistency[c].1.rule;
        if ground_rule.body.is_empty() {
            if guards_hold(&ground_rule.guards, &program.consistency[c].1.substitution) {
                let Some(id) = group_of(c, found) else { break };
                found.record(id, words(program, c, &[]));
            }
        } else {
            jobs.extend(Job::full(source, ground_rule, &poll).map(|job| (c, job)));
        }
    }
    for &(c, i) in variants {
        let ground_rule = &program.consistency[c].1.rule;
        jobs.extend(Job::variant(source, ground_rule, i, &poll).map(|job| (c, job)));
    }
    'jobs: for (c, job) in jobs {
        // A list that refused (an empty-body rule above, or an earlier job) ends the
        // check: the state is inconsistent whatever the rest would add.
        if !found.complete() {
            break;
        }
        // Made at the job's first firing: a job that never fires charges nothing, so a
        // budget without room for its group can't make consistent data look inconsistent.
        let mut id = None;
        for start in (0..job.drivers()).step_by(MORSEL) {
            let range = start..(start + MORSEL).min(job.drivers());
            let stopped = job.run_until(source, range, &poll, &mut |bindings| {
                // A refused report stops the walk: the state is inconsistent whatever
                // the rest would add.
                let group = match id {
                    Some(group) => group,
                    None => match group_of(c, found) {
                        Some(group) => *id.insert(group),
                        None => return true,
                    },
                };
                !found.record(group, words(program, c, bindings))
            });
            let stopped = stopped.map_err(|Stopped| Interrupted::default())?;
            if stopped {
                break 'jobs;
            }
        }
    }
    if stop() {
        return Err(Interrupted::default());
    }
    found.finish();
    Ok(())
}

#[cfg(test)]
mod counted_tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};

    /// A poller whose stop has fired since it was made: it stops at its next poll.
    fn fired_after(fired: &AtomicBool, test: impl FnOnce(&Poller<'_>)) {
        let stop = || fired.load(Ordering::Relaxed);
        let poll = Poller::new(&stop).expect("not stopped yet");
        fired.store(true, Ordering::Relaxed);
        test(&poll);
    }

    /// REAS-2 (master, 8 Nov 05:29: a counted test per leaf that filters): the store's scan
    /// counts the positions its runs pass over, not only the facts it delivers. A lookup by
    /// object alone in a relation without its object order reads every pair: over three poll
    /// intervals of pairs, none with the object, a poller whose stop has fired polls within
    /// the scan and is stopped by it; uncounted, it would never poll.
    #[test]
    fn a_store_scan_counts_the_pairs_it_passes_over() {
        let n = 3 * u64::from(nrese_exec::POLL_UNITS);
        let mut store = Store::new((0..n).map(|i| [i, 5, 1_000_000 + i]).collect());
        store.objects(&|_| false);
        fired_after(&AtomicBool::new(false), |poll| {
            let mut delivered = 0;
            let scanned = store.scan([None, Some(5), Some(7)], Seg::All, poll, &mut |_| {
                delivered += 1
            });
            assert!(scanned.is_err(), "a scan its poller stopped says so");
            assert_eq!(delivered, 0, "no pair has the object");
            assert!(
                poll.stopped(),
                "{n} pairs passed over without the poller polling"
            );
        });
    }

    /// REAS-2 review r1 F1: the stored facts mentioning a term ([`Store::mentioning`], what an
    /// equality merge rewrites) are read as they are counted. A relation without its object
    /// order is read whole for the objects: its input of 8 × `POLL_UNITS` pairs, none with the
    /// object asked for, asks the stop at least once every `POLL_UNITS` plus one run (the
    /// longest run counted at once, `POLL_UNITS`) of the pairs it reads. Counted all at once,
    /// the whole input was one ask.
    #[test]
    fn a_term_s_mentions_are_read_within_a_run_of_the_stop() {
        let interval = nrese_exec::POLL_UNITS as usize;
        let n = 8 * interval as u64;
        let mut store = Store::new((0..n).map(|i| [i, 5, 1_000_000 + i]).collect());
        store.objects(&|_| false);
        let asks = std::sync::atomic::AtomicUsize::new(0);
        let stop = || {
            asks.fetch_add(1, Ordering::Relaxed);
            false
        };
        // In no fact: subjects are below `n`, objects from 1,000,000 on.
        let terms: HashSet<u64> = [5_000_000].into_iter().collect();
        let found = store.mentioning(&terms, &stop).expect("never stopped");
        assert!(found.is_empty(), "no fact mentions the term");
        let run = interval;
        let asks = asks.load(Ordering::Relaxed);
        assert!(
            asks >= n as usize / (interval + run),
            "{asks} asks of the stop for {n} pairs read: more than {interval} plus one run of \
             {run} read between two of them"
        );
    }

    /// REAS-2 (orchestrator, 8 Nov 15:54): a closed family's read of a marked delta
    /// ([`Run::scan_unmarked`]) counts a slice's positions ahead, so its reader asks its stop
    /// within `POLL_UNITS` plus one run, the longest run counted at once: [`RUN`], which is
    /// `POLL_UNITS` (`Run::scan_unmarked` hands each slice on in pieces of at most `RUN`). One
    /// delta chunk of the production chunk size, 1 << 20 pairs ([`CHUNK_PAIRS`]), none of
    /// them marked: counted as one attempt, the whole chunk was one ask. The chunk is built
    /// as one ([`Pairs::whole`]) rather than through [`set_chunk_pairs`], a process-wide
    /// setting the other unit tests read at 16 while this one runs.
    #[test]
    fn a_marked_delta_scan_asks_its_stop_within_a_run() {
        let n: usize = 1 << 20;
        let pairs: Vec<Pair> = (0..n as u64).map(|i| (i, i + 1)).collect();
        let mut relation = Relation {
            closed_by: Some(1),
            ..Relation::default()
        };
        relation.delta = Run::new(Pairs::whole(pairs), false);
        relation.marks = Some(Marks {
            all: false,
            so: vec![vec![0; n.div_ceil(64)]],
            os: None,
        });
        let asks = std::sync::atomic::AtomicUsize::new(0);
        let stop = || {
            asks.fetch_add(1, Ordering::Relaxed);
            false
        };
        let poll = Poller::new(&stop).expect("not stopped");
        let mut delivered = 0;
        {
            let mut deliver = |_: Triple| delivered += 1;
            let mut visit = Visit::new(&poll, &mut deliver);
            relation.scan(None, None, Seg::DeltaNotBy(1), &mut |at| {
                on(&mut visit, 7, at)
            });
        }
        assert_eq!(delivered, n, "every pair of the delta, none marked");
        let interval = nrese_exec::POLL_UNITS as usize;
        // The longest run the scan counts at once (`Run::scan_unmarked`).
        let run = interval;
        let asks = asks.load(Ordering::Relaxed);
        assert!(
            asks >= n / (interval + run),
            "{asks} asks of the stop for {n} positions: more than {interval} positions plus \
             one run of {run} visited between two of them"
        );
    }

    /// REAS-2 fix check r1 (R2-F2). Spec (design note, rule 1: a producer of descriptors
    /// yields them lazily, each counted as it is made): a job's length of one key's matches
    /// (`Source::matches_len`, what `Job::new` asks) and a morsel's range of them
    /// (`Source::scan_range`) read the key's chunk slices as they count them. One subject
    /// over 3 × `POLL_UNITS` chunks of the unit tests' 16 pairs, in the input (unmarked) and
    /// as a marked delta, each read on a poller whose stop has fired since it was made:
    /// neither holds more than one run of `POLL_UNITS` slice descriptors at once. Built first,
    /// every chunk's descriptor was held before the first was counted.
    #[test]
    fn a_key_s_slices_are_read_as_they_are_counted() {
        use nrese_exec::measure;
        if !measure::alone() {
            return;
        }
        let interval = nrese_exec::POLL_UNITS as usize;
        let chunks = 3 * interval;
        let pairs: Vec<Pair> = (0..(chunks * chunk_pairs()) as u64)
            .map(|i| (7, 1_000 + i))
            .collect();
        let one_run = interval * std::mem::size_of::<Slice<'static>>();
        let mut failed = Vec::new();
        for marked in [false, true] {
            let mut store = Store::new(vec![[7, 5, 1_000]]);
            let at = store.index[&5];
            let relation = &mut store.relations[at];
            let seg = match marked {
                false => {
                    *relation = Relation::default();
                    relation.input = Run::new(Pairs::chunked(pairs.clone()), false);
                    Seg::All
                }
                true => {
                    *relation = Relation {
                        closed_by: Some(1),
                        ..Relation::default()
                    };
                    relation.delta = Run::new(Pairs::chunked(pairs.clone()), false);
                    relation.marks = Some(Marks {
                        all: false,
                        so: vec![vec![0; chunk_pairs().div_ceil(64)]; chunks],
                        os: None,
                    });
                    Seg::DeltaNotBy(1)
                }
            };
            let pattern = [Some(7), Some(5), None];
            let fired = AtomicBool::new(false);
            let stop = || fired.load(Ordering::Relaxed);
            let poll = Poller::new(&stop).expect("not stopped yet");
            fired.store(true, Ordering::Relaxed);
            let (_, held) = measure::measure(|| {
                let _ = Source::matches_len(&store, pattern, seg, &poll);
            });
            if held > one_run {
                failed.push(format!("matches_len, marked {marked}: {held} bytes held"));
            }
            let fired = AtomicBool::new(false);
            let stop = || fired.load(Ordering::Relaxed);
            let poll = Poller::new(&stop).expect("not stopped yet");
            fired.store(true, Ordering::Relaxed);
            let (_, held) = measure::measure(|| {
                let _ = Source::scan_range(&store, pattern, seg, 0..1, &poll, &mut |_| {});
            });
            if held > one_run {
                failed.push(format!("scan_range, marked {marked}: {held} bytes held"));
            }
        }
        assert!(
            failed.is_empty(),
            "more than one run of {interval} slice descriptors ({one_run} bytes) held: \
             {failed:#?}"
        );
    }

    /// REAS-2 fix check r1 (R2-F3, a morsel's keep). Spec (design note, rule 2: a stop ends
    /// the loop around it): a morsel's keep of its candidates (`Store::retain_new`) ends once
    /// its poller has stopped. 16 × `POLL_UNITS` candidates the store doesn't hold, the stop
    /// firing at its first ask after the keep has begun: the keep looks at no more than one
    /// run of [`RUN`] candidates after it, and keeps none. Answering "not new" for the rest,
    /// it looked at every one.
    #[test]
    fn a_morsel_s_keep_ends_at_its_stop() {
        use crate::witness::{self, Site};
        let n = 16 * RUN as u64;
        let store = Store::new(vec![[1, 5, 2]]);
        let mut facts: Vec<Triple> = (0..n).map(|i| [10 + i, 5, 3]).collect();
        let first = witness::FirstWork::new(Site::RetainStep);
        let stop = || first.ask();
        let poll = Poller::new(&stop).expect("not stopped yet");
        let _ = store.retain_new(&mut facts, &poll);
        let after = first.after();
        assert!(
            after <= RUN as u64,
            "{after} candidates looked at after the stop fired: more than one run of {RUN}"
        );
        assert!(facts.is_empty(), "a stopped keep keeps nothing");
    }
}

#[cfg(test)]
mod chunk_tests {
    use super::*;

    /// The pairs whose first component is `key`.
    fn range(pairs: &[Pair], key: u64) -> &[Pair] {
        let start = pairs.partition_point(|p| p.0 < key);
        let len = pairs[start..].partition_point(|p| p.0 == key);
        &pairs[start..start + len]
    }

    /// Pseudo-random pairs over a small range, so that chunks repeat keys at their ends.
    fn pairs(seed: u64, n: usize) -> Vec<Pair> {
        let mut x = seed;
        (0..n)
            .map(|_| {
                x = x
                    .wrapping_mul(6364136223846793005)
                    .wrapping_add(1442695040888963407);
                ((x >> 33) % 23, (x >> 13) % 41)
            })
            .collect()
    }

    fn sorted(mut pairs: Vec<Pair>) -> Vec<Pair> {
        pairs.sort_unstable();
        pairs.dedup();
        pairs
    }

    /// Chunked runs sort, merge and answer like one sorted vector, at chunk boundaries
    /// too (in unit tests a chunk holds 16 pairs).
    #[test]
    fn chunked_runs_answer_as_one_sorted_vector() {
        for seed in 0..40 {
            let parts: Vec<Vec<Pair>> = (0..1 + seed as usize % 7)
                .map(|i| sorted(pairs(seed * 31 + i as u64, 5 + 37 * i)))
                .collect();
            let all = sorted(parts.concat());
            let chunked = sort_chunked(parts.clone(), false);
            assert_eq!(chunked.iter().copied().collect::<Vec<_>>(), all);
            assert!(chunked.chunks.iter().all(|c| c.len() == c.capacity()));
            let swapped = sort_chunked(parts, true);
            let expected = sorted(all.iter().map(|&(a, b)| (b, a)).collect());
            assert_eq!(swapped.iter().copied().collect::<Vec<_>>(), expected);
            for key in 0..24 {
                let found: Vec<Pair> = chunked.range(key).flatten().copied().collect();
                assert_eq!(found, range(&all, key), "range {key}");
            }
            for pair in pairs(seed + 1000, 50) {
                assert_eq!(chunked.contains(pair), all.binary_search(&pair).is_ok());
            }
            // A finger answers the same over sorted probes (across chunks), and over
            // probes in any order.
            let mut finger = PairsFinger::default();
            for pair in sorted(pairs(seed + 2000, 60)) {
                assert_eq!(
                    chunked.contains_from(&mut finger, pair),
                    all.binary_search(&pair).is_ok()
                );
            }
            for pair in pairs(seed + 3000, 40) {
                assert_eq!(
                    chunked.contains_from(&mut finger, pair),
                    all.binary_search(&pair).is_ok()
                );
            }
            // A merge of disjoint runs, one shared (kept) and one owned (freed).
            let (left, right): (Vec<Pair>, Vec<Pair>) =
                all.iter().partition(|pair| (pair.0 + pair.1) % 3 == 0);
            let shared = Arc::new(Pairs::chunked(left.clone()));
            let owned = Arc::new(sort_chunked(vec![right.clone()], false));
            let merged = merge(Cursor::new(shared.clone()), Cursor::new(owned), all.len());
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
                let (swapped, swapped_marks) = sort_marked(parts, true);
                for (chunk, words) in swapped.chunks.iter().zip(&swapped_marks.expect("marked")) {
                    for (i, &(b, a)) in chunk.iter().enumerate() {
                        assert_eq!(marked(words, i), right.binary_search(&(a, b)).is_ok());
                    }
                }
            }
        }
    }

    impl Relation {
        fn advance_with(&mut self, (pairs, marks): New) -> usize {
            self.advance(pairs, marks)
        }
    }

    /// R13's layout: a closed family's part of the delta is marked in the one delta run
    /// ([`Marks`]): it reads as the delta, the family's own reads skip it, and it costs the
    /// merges nothing over the same rounds with every pair open. As a run of its own it
    /// cost each fold into an empty base a copy, and every lookup a fourth run (LUBM 100:
    /// the merges wrote 19.4 M pairs, 12.5 M with every pair open; 18.5 M point lookups for
    /// 12.9 M).
    #[test]
    fn a_closed_part_reads_as_the_delta_and_costs_nothing() {
        let mut x = 7u64;
        let mut next = move || {
            x = x
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            x >> 33
        };
        let (mut split, mut open) = (Relation::default(), Relation::default());
        split.closed_by = Some(1);
        let (mut written, mut plain) = (0, 0);
        let mut known = std::collections::BTreeSet::new();
        let sorted = |mut pairs: Vec<Pair>| {
            pairs.sort_unstable();
            pairs
        };
        for round in 0..14u64 {
            let fresh: Vec<Pair> = (0..40 + round * 30)
                .map(|_| (next() % 61, next() % 53))
                .filter(|&pair| known.insert(pair))
                .collect();
            let fresh = sorted(fresh);
            // A third of the pairs closed; every pair in some rounds, none in others.
            let (closed, rest): (Vec<Pair>, Vec<Pair>) =
                fresh.iter().partition(|pair| match round % 5 {
                    3 => true,
                    4 => false,
                    _ => (pair.0 + pair.1) % 3 == 0,
                });
            let closed_set: std::collections::BTreeSet<Pair> = closed.iter().copied().collect();
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
                let read = |relation: &Relation, seg: Seg| {
                    let mut out = Vec::new();
                    relation.scan(s, o, seg, &mut |at| {
                        if let At::Pair(s, o) = at {
                            out.push((s, o));
                        }
                        true
                    });
                    sorted(out)
                };
                // As morsels read it: the slices, their skipped pairs left out.
                let sliced = |relation: &Relation, seg: Seg| {
                    let mut out = Vec::new();
                    for slice in relation.matching(s, o, seg).expect("slices") {
                        slice.for_each(&mut |(a, b)| {
                            let (s, o) = if slice.swapped { (b, a) } else { (a, b) };
                            if !(slice.old && relation.delta.contains(s, o)) {
                                out.push((s, o));
                            }
                        });
                    }
                    sorted(out)
                };
                for seg in [Seg::Delta, Seg::Old, Seg::All, Seg::DeltaNotBy(2)] {
                    assert_eq!(read(&split, seg), read(&open, seg), "round {round} {seg:?}");
                    assert_eq!(
                        sliced(&split, seg),
                        read(&open, seg),
                        "round {round} {seg:?}"
                    );
                }
                let mut own = read(&open, Seg::Delta);
                own.retain(|pair| !closed_set.contains(pair));
                assert_eq!(read(&split, Seg::DeltaNotBy(1)), own, "round {round}");
                assert_eq!(sliced(&split, Seg::DeltaNotBy(1)), own, "round {round}");
            }
            for s in 0..61 {
                for o in 0..53 {
                    assert_eq!(split.contains(s, o), known.contains(&(s, o)));
                }
            }
        }
        assert_eq!(
            written, plain,
            "pairs the merges wrote, with a closed part and without"
        );
    }
}
