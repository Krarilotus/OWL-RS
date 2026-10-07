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
    AllFacts, GroundProgram, Job, NEVER, Probes, Seg, Source, guards_hold, run_jobs_by_morsel,
};
use super::ir::{Head, Rule, Term};
use super::ir::{Triple, Violation};
use super::lists::{ListVocabulary, instantiate};
use super::representatives::EqualityClasses;
use nrese_exec::heap;
use nrese_exec::search::Finger;

type Pair = (u64, u64);

/// Facts split by predicate: each predicate once, in order, with its `(subject, object)`
/// pairs sorted and distinct, each list at its exact size. A round's candidates are held
/// this way, a list per morsel: 16 bytes a fact instead of 24, and never concatenated.
type Segments = Vec<(u64, Vec<Pair>)>;

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

/// `pairs` without those of `other` (both sorted), in one pass over both; chunks keep no
/// spare capacity.
fn without(pairs: Pairs, other: &Pairs) -> Pairs {
    if other.len() == 0 {
        return pairs;
    }
    let mut others = other.iter().peekable();
    let mut out = Pairs::default();
    for mut chunk in pairs.chunks {
        chunk.retain(|pair| {
            while others.next_if(|&other| other < pair).is_some() {}
            others.peek() != Some(&pair)
        });
        if chunk.len() < chunk.capacity() {
            chunk = chunk.as_slice().to_vec();
        }
        out.push(chunk);
    }
    out
}

/// The pairs of `parts` (each read as `(b, a)` if `swap`), sorted and distinct, in chunks
/// of at most about [`chunk_pairs`] pairs, without ever holding them in one vector: a sample of
/// them gives the chunks' bounds, each part is split by the bounds (and freed, if it is
/// owned), and each chunk's pieces are sorted together. So the parts and their pieces
/// are what is held at once, not the parts beside a sorted copy of all of them (at
/// LUBM 1000, a round's `rdf:type` candidates, 2.2 times its new facts: 1 GB).
fn sort_chunked<P: AsRef<[Pair]> + Send>(parts: Vec<P>, swap: bool) -> Pairs {
    let read = |&(a, b): &Pair| if swap { (b, a) } else { (a, b) };
    let total: usize = parts.iter().map(|part| part.as_ref().len()).sum();
    let chunks = total.div_ceil(chunk_pairs());
    if chunks <= 1 {
        let mut all: Vec<Pair> = Vec::with_capacity(total);
        for part in parts {
            all.extend(part.as_ref().iter().map(read));
        }
        all.par_sort_unstable();
        all.dedup();
        return Pairs::chunked(all);
    }
    // The bounds: chunk i takes the pairs from bounds[i - 1] up to bounds[i].
    let step = (total / (chunks * 64)).max(1);
    let mut sample: Vec<Pair> = Vec::with_capacity(total / step + 1);
    let mut at = 0;
    for part in &parts {
        let part = part.as_ref();
        let first = (step - at % step) % step;
        sample.extend(part.iter().skip(first).step_by(step).map(read));
        at += part.len();
    }
    sample.par_sort_unstable();
    let bounds: Vec<Pair> = (1..chunks)
        .map(|i| sample[i * sample.len() / chunks])
        .collect();
    drop(sample);
    let chunk_of = |pair: &Pair| bounds.partition_point(|bound| bound <= pair);
    // Each part split by the bounds, at the pieces' exact sizes.
    let pieces: Vec<Vec<(usize, Vec<Pair>)>> = parts
        .into_par_iter()
        .map(|part| {
            let pairs = part.as_ref();
            let mut counts = vec![0; chunks];
            for pair in pairs {
                counts[chunk_of(&read(pair))] += 1;
            }
            let mut pieces: Vec<Vec<Pair>> =
                counts.iter().map(|&n| Vec::with_capacity(n)).collect();
            for pair in pairs {
                let pair = read(pair);
                pieces[chunk_of(&pair)].push(pair);
            }
            pieces
                .into_iter()
                .enumerate()
                .filter(|(_, piece)| !piece.is_empty())
                .collect()
        })
        .collect();
    let mut by_chunk: Vec<Vec<Vec<Pair>>> = (0..chunks).map(|_| Vec::new()).collect();
    for (chunk, piece) in pieces.into_iter().flatten() {
        by_chunk[chunk].push(piece);
    }
    let sorted: Vec<Vec<Pair>> = by_chunk
        .into_par_iter()
        .map(|pieces| {
            let mut chunk = Vec::with_capacity(pieces.iter().map(Vec::len).sum());
            for piece in pieces {
                chunk.extend_from_slice(&piece);
            }
            // In parallel too: few chunks may come at once (OWL2Bench RL-1's modules hand
            // over one list of 1.3 M pairs: two chunks, two threads, 13 % slower).
            chunk.par_sort_unstable();
            chunk.dedup();
            if chunk.len() == chunk.capacity() {
                chunk
            } else {
                chunk.as_slice().to_vec()
            }
        })
        .collect();
    let mut out = Pairs::default();
    for chunk in sorted {
        out.push(chunk);
    }
    out
}

/// The result of [`materialise`].
#[derive(Debug, Default)]
pub struct Materialisation {
    /// Facts derived beyond the input, sorted, without duplicates.
    pub derived: Vec<Triple>,
    /// Consistency violations on the closure, sorted.
    pub violations: Vec<Violation>,
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
        let first = self.lasts.partition_point(|last| last.0 < key);
        self.chunks[first..]
            .iter()
            .map_while(move |chunk| (chunk[0].0 <= key).then(|| range(chunk, key)))
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

/// The union of two disjoint feeds, a buffer at a time: a merge's source that is itself a
/// merge, so three runs merge in one pass, without a copy of the union of two of them.
struct Merged<A, B> {
    a: A,
    b: B,
    buffer: Vec<Pair>,
    at: usize,
}

impl<A: Feed, B: Feed> Merged<A, B> {
    fn new(a: A, b: B) -> Self {
        Self {
            a,
            b,
            buffer: Vec::with_capacity(chunk_pairs().min(4096)),
            at: 0,
        }
    }
}

impl<A: Feed, B: Feed> Feed for Merged<A, B> {
    fn rest(&mut self) -> &[Pair] {
        if self.at == self.buffer.len() {
            self.buffer.clear();
            self.at = 0;
            fill(&mut self.a, &mut self.b, &mut self.buffer);
        }
        &self.buffer[self.at..]
    }

    fn read(&mut self, n: usize) {
        self.at += n;
    }
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

    /// Calls `f` with every `(s, o)` matching the bound positions that `keep` accepts.
    fn scan(
        &self,
        s: Option<u64>,
        o: Option<u64>,
        keep: &dyn Fn(u64, u64) -> bool,
        missed: &std::sync::atomic::AtomicU64,
        f: &mut dyn FnMut(u64, u64),
    ) {
        match (s, o) {
            (Some(s), Some(o)) => {
                if self.contains(s, o) && keep(s, o) {
                    f(s, o);
                }
            }
            (Some(s), None) => {
                for &(_, o) in self.so.range(s).flatten() {
                    if keep(s, o) {
                        f(s, o);
                    }
                }
            }
            (None, Some(o)) => match &self.os {
                Some(os) => {
                    for &(_, s) in os.range(o).flatten() {
                        if keep(s, o) {
                            f(s, o);
                        }
                    }
                }
                // Without the object order: every pair read. Never by the rules that
                // decided it isn't kept; counted where it happens all the same.
                None => {
                    if self.len() > 0 {
                        missed.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    }
                    for &(s, other) in self.so.iter() {
                        if other == o && keep(s, o) {
                            f(s, o);
                        }
                    }
                }
            },
            (None, None) => {
                for &(s, o) in self.so.iter() {
                    if keep(s, o) {
                        f(s, o);
                    }
                }
            }
        }
    }

    /// The pairs matching the bound positions, as slices of one order (a slice per chunk
    /// they are in), with whether that order is `(object, subject)` and `old`; `false` if
    /// they aren't one slice per chunk (an object without its order).
    fn matching<'a>(
        &'a self,
        s: Option<u64>,
        o: Option<u64>,
        old: bool,
        out: &mut Slices<'a>,
    ) -> bool {
        match (s, o) {
            (Some(s), Some(o)) => {
                if let Some(chunk) = self.so.chunk_of((s, o))
                    && let Ok(at) = chunk.binary_search(&(s, o))
                {
                    out.push((&chunk[at..=at], false, old));
                }
            }
            (Some(s), None) => out.extend(self.so.range(s).map(|pairs| (pairs, false, old))),
            (None, Some(o)) => match &self.os {
                Some(os) => out.extend(os.range(o).map(|pairs| (pairs, true, old))),
                None => return self.len() == 0,
            },
            (None, None) => out.extend(self.so.chunks.iter().map(|pairs| (&pairs[..], false, old))),
        }
        true
    }

    fn estimate(&self, s: Option<u64>, o: Option<u64>) -> usize {
        match (s, o) {
            (Some(s), Some(o)) => usize::from(self.contains(s, o)),
            (Some(s), None) => self.so.range(s).map(<[Pair]>::len).sum(),
            (None, Some(o)) => match &self.os {
                Some(os) => os.range(o).map(<[Pair]>::len).sum(),
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

    /// [`Run::merge`] with two runs at once (all three disjoint), in one pass: `b` and `c`
    /// are merged as they are read, without a copy of their union.
    fn merge_both(self, b: Run, c: Run, written: &mut usize) -> Run {
        if b.len() == 0 {
            return self.merge(c, written);
        }
        if c.len() == 0 || self.len() == 0 {
            return self.merge(b, written).merge(c, written);
        }
        let three = |a: Arc<Pairs>, b: Arc<Pairs>, c: Arc<Pairs>| {
            let total = a.len() + b.len() + c.len();
            let rest = Merged::new(Cursor::new(b), Cursor::new(c));
            Arc::new(merge(Cursor::new(a), rest, total))
        };
        let so = three(self.so, b.so, c.so);
        let os = match (self.os, b.os, c.os) {
            (Some(a), Some(b), Some(c)) => Some(three(a, b, c)),
            _ => None,
        };
        *written += so.len() + os.as_ref().map_or(0, |os| os.len());
        Run { so, os }
    }
}

/// Slices of runs matching a pattern: each with whether its order is `(object, subject)`,
/// and whether its pairs are read as old (the recent run's, whose delta pairs are
/// skipped).
type Slices<'a> = smallvec::SmallVec<[(&'a [Pair], bool, bool); 4]>;

/// The pairs of one predicate, in sorted runs: the store's input, apart, and what was
/// added since, in a large base and a small recent run that takes each round's delta.
/// The recent run is folded into the base once it reaches a quarter of its size, so a
/// round costs its delta plus the recent run, not the whole relation, and the total
/// merge work stays O(n log n). The input, kept apart, makes the added pairs the base
/// and the recent run: what a materialisation derived, without a list of its own.
#[derive(Default)]
pub(crate) struct Relation {
    /// The pairs the store started with, once the first round has read them as its
    /// delta.
    input: Run,
    base: Run,
    recent: Run,
    /// The last round's pairs (also in `recent`) but those of `closed`.
    delta: Run,
    /// The last round's pairs one closed rule family produced (the family whose schema
    /// link is `closed_by`: `rdfs:subClassOf` for `cax-sco`), held here only: the next
    /// round merges them into `recent` with its own delta, in one pass. The family's
    /// instances read the delta without them ([`Seg::DeltaNotBy`]): it derived them
    /// with all their consequences under the family.
    closed: Run,
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

/// The pairs whose first component is `key`.
fn range(pairs: &[Pair], key: u64) -> &[Pair] {
    let start = pairs.partition_point(|p| p.0 < key);
    let len = pairs[start..].partition_point(|p| p.0 == key);
    &pairs[start..start + len]
}

impl Relation {
    /// The pairs the last round added (both parts).
    pub(crate) fn delta_len(&self) -> usize {
        self.delta.len() + self.closed.len()
    }

    /// Whether the recent run holds nothing but the delta (it is the delta, shared: right
    /// after a fold, or the first round's input), so its old part is empty.
    fn recent_is_delta(&self) -> bool {
        self.recent.shares(&self.delta)
    }

    /// Whether `seg` reads the closed part of the delta.
    fn reads_closed(&self, seg: Seg) -> bool {
        match seg {
            Seg::DeltaNotBy(family) => self.closed_by != Some(family),
            _ => true,
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
                }
                + self.closed.bytes(),
            delta: if shared(&self.delta) {
                0
            } else {
                self.delta.bytes()
            },
        }
    }

    /// Whether the delta has a pair with object `o`.
    pub(crate) fn delta_has_object(&self, o: u64) -> bool {
        let mut found = false;
        let all = |_: u64, _: u64| true;
        for delta in [&self.delta, &self.closed] {
            delta.scan(None, Some(o), &all, &self.missed, &mut |_, _| found = true);
        }
        found
    }

    /// Calls `f` with each distinct object of the delta.
    pub(crate) fn delta_objects(&self, f: &mut dyn FnMut(u64)) {
        match (&self.delta.os, self.closed.len()) {
            (Some(os), 0) => {
                let mut last = None;
                for &(o, _) in os.iter() {
                    if last != Some(o) {
                        f(o);
                        last = Some(o);
                    }
                }
            }
            _ => {
                let mut objects: Vec<u64> = self
                    .delta
                    .so
                    .iter()
                    .chain(self.closed.so.iter())
                    .map(|&(_, o)| o)
                    .collect();
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
        for run in [
            &mut self.input,
            &mut self.base,
            &mut self.recent,
            &mut self.closed,
        ] {
            run.objects(objects);
        }
        // The delta shares the recent run's pairs or is its own.
        if self.delta.shares(&self.recent) {
            self.delta.os = self.recent.os.clone();
        } else {
            self.delta.objects(objects);
        }
    }

    /// The runs a membership check reads, in order. The closed part goes first: small
    /// (one round of one family), and what the next round's rules derive again most often
    /// (on LUBM 100, 9.1 M of 34 M checks end there; read last, each first missed in the
    /// three large runs: the joins 24 % slower than before the partition).
    fn membership(&self) -> [&Run; 4] {
        [&self.closed, &self.base, &self.recent, &self.input]
    }

    pub(crate) fn contains(&self, s: u64, o: u64) -> bool {
        self.membership().iter().any(|run| run.contains(s, o))
    }

    /// [`Self::contains`] with a finger per run of [`Self::membership`], for pairs probed
    /// in ascending order.
    fn contains_from(&self, s: u64, o: u64, fingers: &mut [PairsFinger; 4]) -> bool {
        self.membership()
            .iter()
            .zip(fingers)
            .any(|(run, finger)| run.so.contains_from(finger, (s, o)))
    }

    /// Every pair, in no particular order.
    pub(crate) fn pairs(&self) -> Vec<Pair> {
        let runs = [&self.input, &self.base, &self.recent, &self.closed];
        let mut out = Vec::with_capacity(runs.iter().map(|run| run.len()).sum());
        for run in runs {
            out.extend(run.so.iter());
        }
        out
    }

    /// Calls `f` with every `(s, o)` matching the bound positions in `seg`.
    fn scan(&self, s: Option<u64>, o: Option<u64>, seg: Seg, f: &mut dyn FnMut(u64, u64)) {
        let all = |_: u64, _: u64| true;
        let missed = &self.missed;
        match seg {
            Seg::Delta | Seg::DeltaNotBy(_) => {
                self.delta.scan(s, o, &all, missed, f);
                if self.reads_closed(seg) {
                    self.closed.scan(s, o, &all, missed, f);
                }
            }
            Seg::All => {
                self.input.scan(s, o, &all, missed, f);
                self.base.scan(s, o, &all, missed, f);
                self.recent.scan(s, o, &all, missed, f);
                self.closed.scan(s, o, &all, missed, f);
            }
            // The delta is in `recent` only (its closed part apart), so the input and the
            // base need no filter.
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
    /// hold them, in [`Relation::scan`]'s order (see [`Slices`]); `None` if a run can't
    /// give them as slices (an object without its order).
    fn matching(&self, s: Option<u64>, o: Option<u64>, seg: Seg) -> Option<Slices<'_>> {
        let mut out = Slices::new();
        let whole = match seg {
            Seg::Delta | Seg::DeltaNotBy(_) => {
                self.delta.matching(s, o, false, &mut out)
                    && (!self.reads_closed(seg) || self.closed.matching(s, o, false, &mut out))
            }
            Seg::All => {
                self.input.matching(s, o, false, &mut out)
                    && self.base.matching(s, o, false, &mut out)
                    && self.recent.matching(s, o, false, &mut out)
                    && self.closed.matching(s, o, false, &mut out)
            }
            Seg::Old => {
                self.input.matching(s, o, false, &mut out)
                    && self.base.matching(s, o, false, &mut out)
                    && (self.recent_is_delta() || self.recent.matching(s, o, true, &mut out))
            }
        };
        whole.then_some(out)
    }

    /// An upper bound on the matches of the bound positions in `seg`.
    fn estimate(&self, s: Option<u64>, o: Option<u64>, seg: Seg) -> usize {
        let delta = |seg| {
            self.delta.estimate(s, o)
                + if self.reads_closed(seg) {
                    self.closed.estimate(s, o)
                } else {
                    0
                }
        };
        match seg {
            Seg::Delta | Seg::DeltaNotBy(_) => delta(seg),
            Seg::Old | Seg::All => {
                self.input.estimate(s, o)
                    + self.base.estimate(s, o)
                    + self.recent.estimate(s, o)
                    + self.closed.estimate(s, o)
            }
        }
    }

    /// Makes `new` the delta and `closed` (produced by the family `closed_by` names) its
    /// closed part; both sorted, deduplicated, disjoint from each other and from the
    /// relation. Returns the pairs its merges wrote.
    fn advance(&mut self, new: Pairs, closed: Pairs) -> usize {
        let mut written = 0;
        // The old delta goes first: it shares the recent run, which is then merged, and
        // freed as it is, only where nothing else holds it. The old closed part is old
        // now: it goes where the recent run goes.
        self.delta = Run::default();
        let mut old = std::mem::take(&mut self.closed);
        if self.fresh {
            // The first round read the input as its delta (and recent run, shared): it
            // goes apart, unmoved.
            self.input = std::mem::take(&mut self.recent);
            self.fresh = false;
        } else if (self.recent.len() + old.len()) * 4 > self.base.len() {
            // Fold the recent run and the old closed part into the base first (one pass),
            // so the new delta stays in `recent`.
            let recent = std::mem::take(&mut self.recent);
            let old = std::mem::take(&mut old);
            self.base = std::mem::take(&mut self.base).merge_both(recent, old, &mut written);
        }
        self.delta = Run::new(new, !self.by_subject_only);
        // One pass; an empty recent run becomes the delta itself (shared, not copied).
        self.recent =
            std::mem::take(&mut self.recent).merge_both(old, self.delta.clone(), &mut written);
        self.closed = Run::new(closed, !self.by_subject_only);
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
                    closed: Run::default(),
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
    fn retain_new(&self, facts: &mut Vec<Triple>) -> u64 {
        let mut at: Option<(u64, Option<&Relation>)> = None;
        let mut fingers = [PairsFinger::default(); 4];
        let mut starts = 0;
        facts.retain(|&[s, p, o]| {
            if at.is_none_or(|(q, _)| q != p) {
                at = Some((p, self.relation(p)));
                fingers = [PairsFinger::default(); 4];
                starts += 1;
            }
            let relation = at.and_then(|(_, relation)| relation);
            relation.is_none_or(|r| !r.contains_from(s, o, &mut fingers))
        });
        starts
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
        let count = news
            .iter()
            .map(|(open, closed)| open.len() + closed.len())
            .sum();
        let schema_facts =
            news.par_iter()
                .zip(self.predicates.par_iter())
                .any(|((open, closed), &p)| {
                    open.iter()
                        .chain(closed.iter())
                        .any(|&(s, o)| schema.is_schema_fact([s, p, o]))
                });
        heap::phase("reasoner: install");
        self.install(news);
        (count, schema_facts)
    }

    /// Every pair added since the input, as facts, in no particular order: what a
    /// materialisation from the input derived. The store is taken apart on the way, so
    /// the facts never sit beside all of it: the input runs and the `(object, subject)`
    /// orders go first, and each relation's runs once their facts are out.
    pub(crate) fn into_derived(self) -> Vec<Triple> {
        let runs: Vec<(u64, [Arc<Pairs>; 3])> = self
            .relations
            .into_iter()
            .zip(self.predicates)
            .map(|(relation, p)| {
                (
                    p,
                    [relation.base.so, relation.recent.so, relation.closed.so],
                )
            })
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
        let runs: Vec<(u64, [Arc<Pairs>; 3])> = self
            .relations
            .into_iter()
            .zip(self.predicates)
            .map(|(relation, p)| {
                (
                    p,
                    [relation.base.so, relation.recent.so, relation.closed.so],
                )
            })
            .collect();
        // At most this many, so the list never grows by doubling.
        out.reserve(
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
                [
                    &relation.input,
                    &relation.base,
                    &relation.recent,
                    &relation.closed,
                ]
                .into_iter()
                .flat_map(|run| run.so.iter())
                .map(move |&(s, o)| [s, p, o])
                .filter(|&fact| keep(fact))
            })
            .collect()
    }

    /// Every stored fact mentioning one of `terms`, in any position (some more than once).
    /// By the subject and object orders where a run has them; a run without its object
    /// order is read whole for the objects.
    fn mentioning(&self, terms: &HashSet<u64>) -> Vec<Triple> {
        self.relations
            .par_iter()
            .zip(self.predicates.par_iter())
            .flat_map_iter(|(relation, &p)| {
                let mut out = Vec::new();
                for run in [
                    &relation.input,
                    &relation.base,
                    &relation.recent,
                    &relation.closed,
                ] {
                    if terms.contains(&p) {
                        out.extend(run.so.iter().map(|&(s, o)| [s, p, o]));
                        continue;
                    }
                    for &t in terms {
                        out.extend(run.so.range(t).flatten().map(|&(s, o)| [s, p, o]));
                    }
                    match &run.os {
                        Some(os) => {
                            for &t in terms {
                                out.extend(os.range(t).flatten().map(|&(o, s)| [s, p, o]));
                            }
                        }
                        None => out.extend(
                            run.so
                                .iter()
                                .filter(|(_, o)| terms.contains(o))
                                .map(|&(s, o)| [s, p, o]),
                        ),
                    }
                }
                out
            })
            .collect()
    }

    /// Makes `news` (per relation: the open part, the closed part) the relations' deltas.
    fn install(&mut self, news: Vec<(Pairs, Pairs)>) {
        self.merged += self
            .relations
            .par_iter_mut()
            .zip(news.into_par_iter())
            .map(|(relation, (open, closed))| relation.advance(open, closed) as u64)
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
    /// open part, which leaves out what the closed part holds.
    fn news(&mut self, candidates: Vec<Candidates>, check: bool) -> Vec<(Pairs, Pairs)> {
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
        let groups: Vec<(usize, (Pairs, Pairs))> = groups
            .into_par_iter()
            .map(|(index, (mut open, mut closed))| {
                if check {
                    let relation = &self.relations[index];
                    for part in open.iter_mut().chain(closed.iter_mut()) {
                        part.retain(|&(s, o)| !relation.contains(s, o));
                    }
                }
                let closed = match closed.is_empty() {
                    true => Pairs::default(),
                    false => union(closed),
                };
                let open = without(union(open), &closed);
                (index, (open, closed))
            })
            .collect();
        let mut news: Vec<(Pairs, Pairs)> = self
            .relations
            .iter()
            .map(|_| (Pairs::default(), Pairs::default()))
            .collect();
        for (index, pairs) in groups {
            news[index] = pairs;
        }
        news
    }
}

impl Source for Store {
    fn scan(&self, [s, p, o]: [Option<u64>; 3], seg: Seg, f: &mut dyn FnMut(Triple)) {
        match p {
            Some(p) => {
                if let Some(relation) = self.relation(p) {
                    relation.scan(s, o, seg, &mut |s, o| f([s, p, o]));
                }
            }
            None => {
                for (relation, &p) in self.relations.iter().zip(&self.predicates) {
                    relation.scan(s, o, seg, &mut |s, o| f([s, p, o]));
                }
            }
        }
    }

    fn estimate(&self, [s, p, o]: [Option<u64>; 3], seg: Seg) -> usize {
        match p {
            Some(p) => self.relation(p).map_or(0, |r| r.estimate(s, o, seg)),
            None => self.relations.iter().map(|r| r.estimate(s, o, seg)).sum(),
        }
    }

    fn contains(&self, [s, p, o]: Triple) -> bool {
        self.relation(p).is_some_and(|r| r.contains(s, o))
    }

    fn matches_len(&self, [s, p, o]: [Option<u64>; 3], seg: Seg) -> Option<usize> {
        self.relations_of(p)
            .map(|(_, relation)| {
                let slices = relation.matching(s, o, seg)?;
                Some(slices.iter().map(|(pairs, ..)| pairs.len()).sum::<usize>())
            })
            .sum()
    }

    fn scan_range(
        &self,
        [s, p, o]: [Option<u64>; 3],
        seg: Seg,
        range: std::ops::Range<usize>,
        f: &mut dyn FnMut(Triple),
    ) {
        let mut at = 0;
        for (p, relation) in self.relations_of(p) {
            let slices = relation
                .matching(s, o, seg)
                .expect("matches_len gave slices");
            for &(pairs, swapped, old) in &slices {
                let (start, end) = (at, at + pairs.len());
                at = end;
                if end <= range.start {
                    continue;
                }
                if start >= range.end {
                    return;
                }
                let part = &pairs[range.start.max(start) - start..range.end.min(end) - start];
                if old {
                    relation
                        .old_checks
                        .fetch_add(part.len() as u64, std::sync::atomic::Ordering::Relaxed);
                }
                for &(a, b) in part {
                    let (s, o) = if swapped { (b, a) } else { (a, b) };
                    if !(old && relation.delta.contains(s, o)) {
                        f([s, p, o]);
                    }
                }
            }
        }
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

    /// The class of `x`: itself and its `sameAs` partners, sorted.
    fn members<S: Source + ?Sized>(&self, store: &S, x: u64) -> Vec<u64> {
        let mut members = vec![x];
        store.scan([Some(x), Some(self.same_as), None], Seg::All, &mut |t| {
            members.push(t[2]);
        });
        members.sort_unstable();
        members.dedup();
        members
    }

    /// The expansions of the delta of `store` (see the type's docs), not yet in it.
    pub(crate) fn run<S: Source + ?Sized>(&mut self, store: &S) -> Vec<Triple> {
        let equalities = store.estimate([None, Some(self.same_as), None], Seg::All);
        if equalities == 0 {
            self.produced.clear();
            return Vec::new();
        }
        let same_as = self.same_as;
        // A fact mentioning no term with a partner expands to itself only (#12 of the
        // investigation of 6 October 2026). Which terms have one: for a small delta (a
        // commit's, beside a store's many equalities) each term of it is looked up; for
        // a large one (the batch executor's rounds) they are collected once from all
        // `sameAs` facts and then kept current from each round's.
        if self.partners.is_none()
            && store.estimate([None, None, None], Seg::Delta) * 8 >= equalities
        {
            let mut all = TermSet::default();
            store.scan([None, Some(same_as), None], Seg::All, &mut |[s, _, o]| {
                if s != o {
                    all.insert(s);
                    all.insert(o);
                }
            });
            self.partners = Some(all);
        } else if let Some(partners) = &mut self.partners {
            store.scan([None, Some(same_as), None], Seg::Delta, &mut |[s, _, o]| {
                if s != o {
                    partners.insert(s);
                    partners.insert(o);
                }
            });
        }
        let mut looked_up: HashMap<u64, bool> = HashMap::new();
        let mut has_partner = |term: u64| match &self.partners {
            Some(partners) => partners.contains(term),
            None => *looked_up.entry(term).or_insert_with(|| {
                // An upper bound: zero means no equality mentions the term.
                store.estimate([Some(term), Some(same_as), None], Seg::All) > 0
                    || store.estimate([None, Some(same_as), Some(term)], Seg::All) > 0
            }),
        };
        let mut seeds: Vec<Triple> = Vec::new();
        let mut merged: Vec<u64> = Vec::new();
        let produced = &self.produced;
        store.scan([None, None, None], Seg::Delta, &mut |t| {
            if t[1] == same_as {
                if t[0] != t[2] {
                    merged.push(t[0]);
                }
            } else if t.iter().any(|&term| has_partner(term)) && !produced.contains(&t) {
                seeds.push(t);
            }
        });
        // Facts mentioning a term whose class grew.
        merged.sort_unstable();
        merged.dedup();
        for &a in &merged {
            let mut push = |t: Triple| {
                if t[1] != same_as {
                    seeds.push(t);
                }
            };
            store.scan([Some(a), None, None], Seg::All, &mut push);
            store.scan([None, None, Some(a)], Seg::All, &mut push);
            store.scan([None, Some(a), None], Seg::All, &mut push);
        }
        seeds.par_sort_unstable();
        seeds.dedup();
        self.member_scans += 3 * seeds.len() as u64;
        let out: Vec<Triple> = seeds
            .par_iter()
            .flat_map_iter(|&[s, p, o]| {
                let (ms, mp, mo) = (
                    self.members(store, s),
                    self.members(store, p),
                    self.members(store, o),
                );
                let mut facts = Vec::new();
                for &s in &ms {
                    for &p in &mp {
                        for &o in &mo {
                            let fact = [s, p, o];
                            if !store.contains(fact) {
                                facts.push(fact);
                            }
                        }
                    }
                }
                facts
            })
            .collect();
        self.produced = out.iter().copied().collect();
        out
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

    /// The facts the merge that took the places of `lost` makes stale, rewritten.
    fn rewrites(&self, store: &Store, lost: &HashSet<u64>) -> Vec<Triple> {
        let mut facts = store.mentioning(lost);
        facts
            .par_iter_mut()
            .for_each(|fact| *fact = self.classes.rewrite(*fact));
        facts
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

impl Source for Current<'_> {
    fn scan(&self, pattern: [Option<u64>; 3], seg: Seg, f: &mut dyn FnMut(Triple)) {
        if self.stale.is_empty() {
            return self.store.scan(pattern, seg, f);
        }
        self.store.scan(pattern, seg, &mut |fact| {
            if !self.stale.mentioned(fact) {
                f(fact);
            }
        });
    }

    fn estimate(&self, pattern: [Option<u64>; 3], seg: Seg) -> usize {
        self.store.estimate(pattern, seg)
    }

    fn contains(&self, fact: Triple) -> bool {
        !self.stale.mentioned(fact) && self.store.contains(fact)
    }

    fn matches_len(&self, pattern: [Option<u64>; 3], seg: Seg) -> Option<usize> {
        self.store.matches_len(pattern, seg)
    }

    fn scan_range(
        &self,
        pattern: [Option<u64>; 3],
        seg: Seg,
        range: std::ops::Range<usize>,
        f: &mut dyn FnMut(Triple),
    ) {
        if self.stale.is_empty() {
            return self.store.scan_range(pattern, seg, range, f);
        }
        self.store.scan_range(pattern, seg, range, &mut |fact| {
            if !self.stale.mentioned(fact) {
                f(fact);
            }
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
    run(store, clock.elapsed(), rules, lists, schema, None, NEVER).expect("never stopped")
}

/// [`materialise_owned`], polling `stop` as [`materialise_grouped_until`] does.
pub fn materialise_owned_until(
    input: Vec<Triple>,
    rules: &[Rule],
    lists: Option<&ListVocabulary>,
    schema: &Schema,
    stop: super::eval::Stop<'_>,
) -> Result<Materialisation, Interrupted> {
    let clock = std::time::Instant::now();
    let store = Store::new(input);
    run(store, clock.elapsed(), rules, lists, schema, None, stop)
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
    let clock = std::time::Instant::now();
    let store = Store::from_groups(input);
    run(store, clock.elapsed(), rules, lists, schema, None, stop)
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
        let mut result = run(store, clock.elapsed(), rules, lists, schema, None, stop)?;
        result.derived.extend(input_facts);
        result.derived.par_sort_unstable();
        result.derived.dedup();
        return Ok(result);
    }
    run(
        store,
        clock.elapsed(),
        rules,
        lists,
        schema,
        representatives,
        stop,
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

/// Semi-naive evaluation to the fixpoint, from a store holding the input as its delta;
/// with equality by representatives if `representatives` is given.
fn run(
    mut store: Store,
    load: std::time::Duration,
    rules: &[Rule],
    lists: Option<&ListVocabulary>,
    schema: &Schema,
    mut representatives: Option<Representatives>,
    stop: super::eval::Stop<'_>,
) -> Result<Materialisation, Interrupted> {
    // The process's memory limit stops it too, whatever the caller polls.
    let stop: super::eval::Stop<'_> = &|| stop() || super::eval::over_memory_limit();
    let check_stop = || if stop() { Err(Interrupted) } else { Ok(()) };
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
    // Facts to add in the first round, checked against the store: the input's stale
    // facts rewritten, where the input asserts equalities.
    let mut pending: Vec<Triple> = Vec::new();
    if let Some(reps) = &mut representatives {
        let mut pairs = Vec::new();
        store.scan([None, Some(reps.same_as), None], Seg::All, &mut |[
            s,
            _,
            o,
        ]| {
            if s != o {
                pairs.push((s, o));
            }
        });
        let lost = reps.merge(&pairs);
        if !lost.is_empty() {
            pending = reps.rewrites(&store, &lost);
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
        result.rounds += 1;
        heap::phase("reasoner: grounding");
        let clock = std::time::Instant::now();
        let stale = representatives.as_ref().map_or(&none, |r| &r.stale);
        // Rules from `evaluated` on were added this round: evaluated once in full.
        let evaluated = program.rules.len();
        if regrounding || reground_all {
            let current = Current {
                store: &store,
                stale,
            };
            let mut source = fact_rules(&current, &rules, lists, &mut result.diagnostics);
            if equality.is_some() {
                source.retain(|r| !is_replacement_rule(r));
            }
            // The first round grounds everything; later ones through new schema facts
            // only (list rules in full, deduplicated).
            if result.rounds == 1 || reground_all {
                program.ground(&current, schema, &source);
            } else {
                program.ground_delta(&current, schema, &source);
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
        let equal = equality
            .as_ref()
            .is_some_and(|e| store.estimate([None, Some(e.same_as), None], Seg::All) > 0);
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
        facts.retain(|&f| !store.contains(f));
        let mut candidates: Vec<Candidates> = vec![(None, segments(facts))];
        pending.retain(|&fact| !store.contains(fact));
        candidates.push((None, segments(std::mem::take(&mut pending))));
        // Each job with the closed family its rule belongs to, if any: a variant of such
        // a rule reads the delta without what the family produced, and what a job of the
        // family derives goes to the family's part of the delta.
        let mut jobs = Vec::new();
        let mut families: Vec<Option<u64>> = Vec::new();
        for (r, i) in program.variants(&store) {
            if r < evaluated {
                let family = program.closed_family(r);
                let delta = family.map_or(Seg::Delta, Seg::DeltaNotBy);
                if let Some(job) = Job::variant_reading(&current, &program.rules[r], i, delta) {
                    jobs.push(job);
                    families.push(family);
                }
            }
        }
        for (r, rule) in program.rules.iter().enumerate().skip(evaluated) {
            if let Some(job) = Job::full(&current, rule) {
                jobs.push(job);
                families.push(program.closed_family(r));
            }
        }
        let probes = Probes::for_jobs(jobs.len());
        let starts = std::sync::atomic::AtomicU64::new(0);
        let keep = |facts: &mut Vec<Triple>| {
            let n = store.retain_new(facts);
            starts.fetch_add(n, std::sync::atomic::Ordering::Relaxed);
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
                    .ok_or(Interrupted)?,
            ),
        ));
        result.counters.closure_pairs.push(transitive.read);
        if let Some(equality) = &mut equality {
            candidates.push((None, segments(equality.run(&store))));
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
                moved.extend(reps.rewrites(&store, &lost));
                moved
                    .par_iter_mut()
                    .for_each(|fact| *fact = reps.classes.rewrite(*fact));
                moved.retain(|&fact| !store.contains(fact));
                // Rewritten: no longer what a family produced.
                candidates.push((None, segments(moved)));
                merged = true;
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
        if new == 0 && !reground_all {
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
    heap::phase("reasoner: consistency");
    let clock = std::time::Instant::now();
    let stale = representatives.as_ref().map_or(&none, |r| &r.stale);
    {
        let current = Current {
            store: &store,
            stale,
        };
        program.ground(
            &current,
            schema,
            &consistency_rules(&current, &rules, lists),
        );
    }
    let reads = Reads::of(
        program.rules.iter().chain(
            program
                .consistency
                .iter()
                .map(|(_, grounded)| &grounded.rule),
        ),
    );
    let equal = equality
        .as_ref()
        .is_some_and(|e| store.estimate([None, Some(e.same_as), None], Seg::All) > 0);
    store.objects(&|p| equal || reads.by_object(p) || schema.read_in_grounding(p));
    let mut found = HashSet::new();
    check(
        &Current {
            store: &store,
            stale,
        },
        &program,
        0..program.consistency.len(),
        &[],
        &mut found,
    );
    result.violations = sorted(found);
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
    heap::phase("reasoner: derived");
    let mut derived = match representatives {
        None => store.into_derived(),
        Some(reps) => {
            let derived = list(store, &reps);
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

/// What a materialisation with equality by representatives lists ([`Listing`]).
fn list(store: Store, reps: &Representatives) -> Vec<Triple> {
    let (stale, shared) = (&reps.stale, &reps.shared);
    let current = |fact: Triple| !stale.mentioned(fact);
    match reps.listing {
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
            // its facts out; the others as they are.
            let expanded: Vec<Triple> = store
                .facts_where(&|fact| current(fact) && shared.mentioned(fact))
                .into_par_iter()
                .flat_map_iter(|fact| reps.classes.expand(fact))
                .filter(|&[s, p, o]| {
                    !store
                        .relation(p)
                        .is_some_and(|relation| relation.input.contains(s, o))
                })
                .collect();
            store.into_listed(
                &|_| false,
                &|fact| current(fact) && !shared.mentioned(fact),
                expanded,
            )
        }
    }
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

/// Violations sorted by rule and bindings.
pub(crate) fn sorted(found: HashSet<Violation>) -> Vec<Violation> {
    let mut violations: Vec<Violation> = found.into_iter().collect();
    violations.sort_by(|a, b| (&a.rule, &a.bindings).cmp(&(&b.rule, &b.bindings)));
    violations
}

/// Evaluates consistency instances into `found`: those in `full` over all facts, and the
/// `(instance, atom)` semi-naive variants in `variants` through the delta.
pub(crate) fn check<S: Source + ?Sized>(
    source: &S,
    program: &GroundProgram,
    full: impl IntoIterator<Item = usize>,
    variants: &[(usize, usize)],
    found: &mut HashSet<Violation>,
) {
    let record = |c: usize, bindings: &[Option<u64>], found: &mut HashSet<Violation>| {
        let (rule, grounded) = &program.consistency[c];
        // The job's bindings are sized for the grounded instance, which lacks the schema
        // variables when one of them has the highest index.
        let bindings = (0..rule.variables())
            .map(|v| {
                grounded.substitution[v]
                    .or_else(|| bindings.get(v).copied().flatten())
                    .unwrap_or(0)
            })
            .collect();
        found.insert(Violation {
            rule: rule.name.clone(),
            bindings,
        });
    };
    let mut jobs: Vec<(usize, Job<'_>)> = Vec::new();
    for c in full {
        let ground_rule = &program.consistency[c].1.rule;
        if ground_rule.body.is_empty() {
            if guards_hold(&ground_rule.guards, &program.consistency[c].1.substitution) {
                record(c, &vec![None; program.consistency[c].0.variables()], found);
            }
        } else {
            jobs.extend(Job::full(source, ground_rule).map(|job| (c, job)));
        }
    }
    for &(c, i) in variants {
        let ground_rule = &program.consistency[c].1.rule;
        jobs.extend(Job::variant(source, ground_rule, i).map(|job| (c, job)));
    }
    for (c, job) in jobs {
        job.run(source, 0..job.drivers(), &mut |bindings| {
            record(c, bindings, found)
        });
    }
}

#[cfg(test)]
mod chunk_tests {
    use super::*;

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
            // Three at once, two of them merged as they are read; and a difference.
            let (middle, last): (Vec<Pair>, Vec<Pair>) =
                right.iter().partition(|pair| pair.0 % 2 == 0);
            let rest = Merged::new(
                Cursor::new(Arc::new(Pairs::chunked(middle.clone()))),
                Cursor::new(Arc::new(Pairs::chunked(last))),
            );
            let three = merge(Cursor::new(shared.clone()), rest, all.len());
            assert_eq!(three.iter().copied().collect::<Vec<_>>(), all);
            let rest = without(three, &Pairs::chunked(middle.clone()));
            let expected: Vec<Pair> = all
                .iter()
                .copied()
                .filter(|pair| middle.binary_search(pair).is_err())
                .collect();
            assert_eq!(rest.iter().copied().collect::<Vec<_>>(), expected);
            assert_eq!(rest.len(), expected.len());
        }
    }

    /// R13's layout: a closed family's part of the delta reads as the delta (the family's
    /// own reads skip it) and costs the merges one copy of its pairs, over the same
    /// rounds with every pair open. Kept beside the recent run for good, it cost each fold
    /// a second pass over the base (LUBM 100: the merges wrote 26.5 M pairs, 12.5 M with
    /// every pair open, 19.4 M since).
    #[test]
    fn a_closed_part_reads_as_the_delta_and_costs_one_copy() {
        let mut x = 7u64;
        let mut next = move || {
            x = x
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            x >> 33
        };
        let (mut split, mut open) = (Relation::default(), Relation::default());
        split.closed_by = Some(1);
        let (mut written, mut plain, mut copies) = (0, 0, 0);
        let mut known = std::collections::BTreeSet::new();
        let sorted = |mut pairs: Vec<Pair>| {
            pairs.sort_unstable();
            pairs
        };
        for round in 0..12u64 {
            let fresh: Vec<Pair> = (0..40 + round * 30)
                .map(|_| (next() % 61, next() % 53))
                .filter(|&pair| known.insert(pair))
                .collect();
            let fresh = sorted(fresh);
            let (closed, rest): (Vec<Pair>, Vec<Pair>) =
                fresh.iter().partition(|pair| (pair.0 + pair.1) % 3 == 0);
            copies += closed.len();
            let closed_set: std::collections::BTreeSet<Pair> = closed.iter().copied().collect();
            written += split.advance(Pairs::chunked(rest), Pairs::chunked(closed));
            plain += open.advance(Pairs::chunked(fresh), Pairs::default());
            for (s, o) in [
                (None, None),
                (Some(3), None),
                (None, Some(5)),
                (Some(3), Some(5)),
            ] {
                let read = |relation: &Relation, seg: Seg| {
                    let mut out = Vec::new();
                    relation.scan(s, o, seg, &mut |s, o| out.push((s, o)));
                    sorted(out)
                };
                for seg in [Seg::Delta, Seg::Old, Seg::All, Seg::DeltaNotBy(2)] {
                    assert_eq!(read(&split, seg), read(&open, seg), "round {round} {seg:?}");
                }
                let mut own = read(&open, Seg::Delta);
                own.retain(|pair| !closed_set.contains(pair));
                assert_eq!(read(&split, Seg::DeltaNotBy(1)), own, "round {round}");
            }
            for s in 0..61 {
                for o in 0..53 {
                    assert_eq!(split.contains(s, o), known.contains(&(s, o)));
                }
            }
            // What the family produced is answered by the first run a check reads.
            for &(s, o) in &closed_set {
                assert!(split.membership()[0].contains(s, o), "round {round}");
            }
        }
        assert!(
            written <= plain + copies,
            "the merges wrote {written} pairs, {plain} with every pair open, {copies} closed"
        );
    }
}
