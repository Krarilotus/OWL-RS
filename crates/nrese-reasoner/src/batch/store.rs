//! The working set: a relation per predicate (its runs by subject and, where read, by
//! object; the last round's delta), and how a round's candidates become the next delta.

use super::*;

/// Facts split by predicate: each predicate once, in order, with its `(subject, object)`
/// pairs sorted and distinct, each list at its exact size. A round's candidates are held
/// this way, a list per morsel: 16 bytes a fact instead of 24, and never concatenated.
pub(super) type Segments<R> = Vec<(u64, Vec<R>)>;

/// Candidate facts with the closed rule family that produced them, if one did (its schema
/// link; see [`Relation::<R>::closed_by`]).
pub(super) type Candidates<R> = (Option<Family>, Segments<R>);

/// `facts`, sorted by (predicate, subject, object) and distinct, as [`Segments<R>`].
pub(super) fn split<R: Representation>(facts: Vec<Triple>) -> Segments<R> {
    facts
        .chunk_by(|a, b| a[1] == b[1])
        .map(|chunk| {
            (
                chunk[0][1],
                chunk.iter().map(|&[s, _, o]| R::pair(s, o)).collect(),
            )
        })
        .collect()
}

/// A morsel's candidates (packed facts of numbers, sorted and distinct: [`crate::eval::Candidate`] for
/// `u128`) as [`Segments<R>`]: the low word of each is its pair already.
pub(super) fn split_packed<R: Representation>(facts: Vec<R::Candidate>) -> Segments<R> {
    facts
        .chunk_by(|a, b| R::candidate(*a).0 == R::candidate(*b).0)
        .map(|chunk| {
            (
                R::candidate(chunk[0]).0,
                chunk.iter().map(|&c| R::candidate(c).1).collect(),
            )
        })
        .collect()
}

/// Any `facts` as [`Segments<R>`].
pub(super) fn segments<R: Representation>(mut facts: Vec<Triple>) -> Segments<R> {
    facts.par_sort_unstable_by_key(|&[s, p, o]| (p, s, o));
    facts.dedup();
    split(facts)
}

/// The pairs of one predicate, in sorted runs: the store's input, apart; what the rounds
/// before the last added, in a few runs ([`Relation::<R>::added`]); and the last round's
/// pairs, the delta, a run of its own. Old reads are the input and the added runs, every
/// read adds the delta. The input, kept apart, makes the added runs and the delta what a
/// materialisation derived, without a list of its own.
#[derive(Default)]
pub(crate) struct Relation<R: Representation> {
    /// The pairs the store started with, once the first round has read them as its
    /// delta.
    pub(super) input: Run<R>,
    /// The pairs added before the last round, largest run first: a delta joins them as
    /// it is once the next round's arrives, and the last two runs are merged while the
    /// last is more than half the one before it, or while there are more than
    /// [`ADDED_RUNS`]. So a pair is written about once per doubling of what follows it,
    /// and a read finds it in one of a few runs.
    pub(super) added: Vec<Run<R>>,
    /// The last round's pairs.
    pub(super) delta: Run<R>,
    /// Those of the delta one closed rule family produced (the family whose schema link
    /// is `closed_by`: `rdfs:subClassOf` for `cax-sco`), if it produced some: the family's
    /// instances read the delta without them ([`Seg::DeltaNotBy`]), as it derived them
    /// with all their consequences under the family.
    pub(super) marks: Option<Marks>,
    pub(super) closed_by: Option<Family>,
    /// Whether the delta is still the input (the store's first round is to come).
    pub(super) fresh: bool,
    /// Whether no rule looks the relation up by object alone, so its runs keep no
    /// `(object, subject)` order (unused relations, P1-F11).
    pub(super) by_subject_only: bool,
    /// Lookups by object alone in its runs without that order: the analysis that drops
    /// the order ([`Reads`]) says there are none, and the closure tests check it.
    pub(super) missed: std::sync::atomic::AtomicU64,
    /// The time its rounds' advances took: the delta's object order and marks, the
    /// merges.
    pub(super) advance_parts: [std::time::Duration; 2],
}

/// The most runs [`Relation::<R>::added`] holds.
pub(super) const ADDED_RUNS: usize = 3;

/// How many times larger than the next an added run stays: the last two are merged while
/// the last is more than the one before it over this.
const GROWTH: usize = 2;

/// The runs a relation reads at most: the input, the added runs, the delta.
pub(super) const RUNS: usize = ADDED_RUNS + 2;

impl<R: Representation> Relation<R> {
    /// The subject orders of its added runs and its delta, the relation taken apart.
    fn into_added(self) -> Vec<Arc<Pairs<R>>> {
        let mut runs: Vec<Arc<Pairs<R>>> = self.added.into_iter().map(|run| run.so).collect();
        runs.push(self.delta.so);
        runs
    }
}

impl<R: Representation> Relation<R> {
    /// The runs an old read reads: the input and the added runs.
    fn old_runs(&self) -> impl Iterator<Item = &Run<R>> {
        std::iter::once(&self.input).chain(&self.added)
    }

    /// Every run, the delta's included.
    fn runs(&self) -> impl Iterator<Item = &Run<R>> {
        self.old_runs().chain(std::iter::once(&self.delta))
    }

    pub(crate) fn delta_len(&self) -> usize {
        self.delta.len()
    }

    /// Its pairs, the input's included.
    pub(super) fn len_all(&self) -> usize {
        self.runs().map(Run::<R>::len).sum()
    }

    /// Whether the delta's pairs (the closed part's left out) meet fewer than `limit`
    /// pairs: each pair's object's pairs by subject and its subject's by object, what a
    /// transitivity join over the delta reads. Stops counting at the limit.
    pub(super) fn delta_degrees_below(&self, limit: usize) -> bool {
        let mut degrees = 0usize;
        let mut below = true;
        let family = self.closed_by.map_or(Seg::Delta, Seg::DeltaNotBy);
        self.scan(None, None, family, &mut |s, o| {
            if below {
                degrees +=
                    self.estimate(Some(o), None, Seg::All) + self.estimate(None, Some(s), Seg::All);
                below = degrees < limit;
            }
        });
        below
    }

    /// The marks `seg` skips: the closed part of the delta, read by its own family.
    pub(super) fn skips(&self, seg: Seg) -> Option<&Marks> {
        match seg {
            Seg::DeltaNotBy(family) if self.closed_by == Some(family) => self.marks.as_ref(),
            _ => None,
        }
    }

    pub(super) fn bytes(&self) -> StoreBytes {
        StoreBytes {
            input: self.input.bytes(),
            added: self.added.iter().map(Run::<R>::bytes).sum(),
            delta: self.delta.bytes() + self.marks.as_ref().map_or(0, Marks::bytes),
        }
    }

    /// Whether the delta has a pair with object `o`.
    pub(crate) fn delta_has_object(&self, o: u64) -> bool {
        let mut found = false;
        self.delta
            .scan(None, Some(o), &self.missed, &mut |_, _| found = true);
        found
    }

    /// Calls `f` with each distinct object of the delta.
    pub(crate) fn delta_objects(&self, f: &mut dyn FnMut(u64)) {
        match &self.delta.os {
            Some(os) => {
                let mut last = None;
                for &p in os.iter() {
                    let o = R::first(p);
                    if last != Some(o) {
                        f(o);
                        last = Some(o);
                    }
                }
            }
            None => {
                let mut objects: Vec<u64> = self.delta.so.iter().map(|&p| R::second(p)).collect();
                objects.sort_unstable();
                objects.dedup();
                objects.into_iter().for_each(f);
            }
        }
    }

    /// Keeps the runs' `(object, subject)` orders if `objects`, else drops them (see
    /// [`Relation::<R>::by_subject_only`]).
    pub(super) fn objects(&mut self, objects: bool) {
        self.by_subject_only = !objects;
        for run in std::iter::once(&mut self.input)
            .chain(&mut self.added)
            .chain(std::iter::once(&mut self.delta))
        {
            run.objects(objects);
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
                            for (i, &p) in chunk.iter().enumerate() {
                                let (c, at) = so.position(R::swapped(p)).expect("one delta");
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
        self.runs().any(|run| run.contains(s, o))
    }

    /// [`Self::contains`] with a finger per run it reads, for pairs probed in ascending
    /// order; a run whose range doesn't hold the pair isn't searched.
    pub(super) fn contains_from(&self, pair: R, fingers: &mut [PairsFinger; RUNS]) -> bool {
        self.runs()
            .zip(fingers)
            .any(|(run, finger)| run.so.spans(pair) && run.so.contains_from(finger, pair))
    }

    /// Every pair, in no particular order.
    pub(crate) fn pairs(&self) -> Vec<R> {
        let mut out = Vec::with_capacity(self.len_all());
        for run in self.runs() {
            out.extend(run.so.iter());
        }
        out
    }

    /// Calls `f` with every `(s, o)` matching the bound positions in `seg`.
    pub(super) fn scan(
        &self,
        s: Option<u64>,
        o: Option<u64>,
        seg: Seg,
        f: &mut dyn FnMut(u64, u64),
    ) {
        let missed = &self.missed;
        match seg {
            Seg::Delta | Seg::DeltaNotBy(_) => match self.skips(seg) {
                Some(marks) => self.delta.scan_unmarked(s, o, marks, missed, f),
                None => self.delta.scan(s, o, missed, f),
            },
            Seg::All => self.runs().for_each(|run| run.scan(s, o, missed, f)),
            Seg::Old => self.old_runs().for_each(|run| run.scan(s, o, missed, f)),
        }
    }

    /// Calls `f` with every `(s, o)` matching the bound positions in `seg`, reading the
    /// runs in place ([`Run::<R>::each`]); `false` where only a scan could find them (an object
    /// without its order), [`Self::scan`] then.
    #[inline]
    pub(super) fn each(
        &self,
        s: Option<u64>,
        o: Option<u64>,
        seg: Seg,
        fingers: &mut [RangeFinger; RUNS],
        f: &mut impl FnMut(u64, u64),
    ) -> bool {
        // Decided before any match is read, so a caller that scans instead repeats none.
        if s.is_none() && o.is_some() && self.by_subject_only {
            return false;
        }
        match seg {
            Seg::Delta | Seg::DeltaNotBy(_) => {
                self.delta.each(s, o, self.skips(seg), &mut fingers[0], f)
            }
            Seg::All => self
                .runs()
                .zip(fingers)
                .all(|(run, finger)| run.len() == 0 || run.each(s, o, None, finger, f)),
            Seg::Old => self
                .old_runs()
                .zip(fingers)
                .all(|(run, finger)| run.len() == 0 || run.each(s, o, None, finger, f)),
        }
    }

    /// The matches of the bound positions in `seg`, as the slices of the runs that
    /// hold them, in [`Relation::<R>::scan`]'s order (see [`Slices<R>`]); `None` if a run can't
    /// give them as slices (an object without its order).
    pub(super) fn matching(
        &self,
        s: Option<u64>,
        o: Option<u64>,
        seg: Seg,
    ) -> Option<Slices<'_, R>> {
        let mut out = Slices::<R>::new();
        let whole = match seg {
            Seg::Delta | Seg::DeltaNotBy(_) => match self.skips(seg) {
                Some(marks) => self.delta.matching_unmarked(s, o, marks, &mut out),
                None => self.delta.matching(s, o, &mut out),
            },
            Seg::All => self.runs().all(|run| run.matching(s, o, &mut out)),
            Seg::Old => self.old_runs().all(|run| run.matching(s, o, &mut out)),
        };
        whole.then_some(out)
    }

    /// An upper bound on the matches of the bound positions in `seg`.
    pub(super) fn estimate(&self, s: Option<u64>, o: Option<u64>, seg: Seg) -> usize {
        match seg {
            Seg::Delta | Seg::DeltaNotBy(_) => match self.skips(seg) {
                Some(marks) if marks.all => 0,
                _ => self.delta.estimate(s, o),
            },
            Seg::Old => self.old_runs().map(|run| run.estimate(s, o)).sum(),
            Seg::All => self.runs().map(|run| run.estimate(s, o)).sum(),
        }
    }

    /// Makes `new` and `closed` (produced by the family `closed_by` names) the delta, the
    /// pairs of `closed` marked; both sorted, deduplicated, disjoint from each other and
    /// from the relation. Returns the pairs its merges wrote.
    pub(super) fn advance(&mut self, new: Pairs<R>, marks: Option<Marks>) -> usize {
        let mut written = 0;
        let clock = std::time::Instant::now();
        self.marks = None;
        let delta = std::mem::take(&mut self.delta);
        if self.fresh {
            // The first round read the input as its delta: it goes apart, unmoved.
            self.input = delta;
            self.fresh = false;
        } else if delta.len() > 0 {
            self.added.push(delta);
        }
        while let [.., before, last] = &self.added[..]
            && (self.added.len() > ADDED_RUNS || last.len() * GROWTH > before.len())
        {
            let last = self.added.pop().expect("two runs");
            let before = self.added.pop().expect("two runs");
            self.added.push(before.merge(last, &mut written));
        }
        self.advance_parts[1] += clock.elapsed();
        let clock = std::time::Instant::now();
        (self.delta, self.marks) = marked_run(new, marks, !self.by_subject_only);
        self.advance_parts[0] += clock.elapsed();
        written
    }
}

/// The working set: one relation per predicate.
#[derive(Default)]
pub(crate) struct Store<R: Representation> {
    pub(super) relations: Vec<Relation<R>>,
    /// The predicate of each relation.
    pub(super) predicates: Vec<u64>,
    pub(super) index: HashMap<u64, usize>,
    /// Relations whose closed part a family claimed beforehand ([`Store::<R>::claim`]).
    pub(super) claims: HashMap<u64, Family>,
    /// The pairs the relations' merges wrote so far (both orders).
    pub(super) merged: u64,
    /// The rounds' new pairs: sorting and deduplicating the candidates.
    pub(super) news_time: std::time::Duration,
}

impl<R: Representation> Store<R> {
    /// A store in the operation's selected layout. Incremental/support callers use wide.
    pub(crate) fn new(input: Vec<Triple>) -> Self {
        let mut store = Self::default();
        store.advance(input);
        for relation in &mut store.relations {
            relation.fresh = true;
        }
        store
    }

    pub(super) fn dense(input: Vec<Triple>) -> Self {
        Self::new(input)
    }

    /// A store holding `groups` (see [`materialise_grouped`]; each predicate's `(object,
    /// subject)` pairs, whose ids are numbers below 2^32 already), all of it as the delta.
    pub(super) fn from_groups(groups: Vec<(u64, Vec<R>)>) -> Self {
        let mut store = Self::default();
        for (p, _) in &groups {
            store.index.insert(*p, store.predicates.len());
            store.predicates.push(*p);
        }
        store.relations = groups
            .into_par_iter()
            .map(|(_, os)| {
                let delta = Run::<R>::from_os(os);
                Relation {
                    input: Run::<R>::default(),
                    added: Vec::new(),
                    delta,
                    marks: None,
                    closed_by: None,
                    fresh: true,
                    by_subject_only: false,
                    missed: Default::default(),
                    advance_parts: Default::default(),
                }
            })
            .collect();
        store
    }

    /// The relations with a non-empty delta.
    pub(crate) fn delta_relations(&self) -> impl Iterator<Item = DeltaRelation<'_, R>> {
        self.predicates
            .iter()
            .zip(&self.relations)
            .filter(|(_, r)| r.delta_len() > 0)
            .map(|(&predicate, relation)| DeltaRelation {
                relation,
                predicate,
            })
    }

    /// The bytes the runs hold.
    pub(crate) fn bytes(&self) -> StoreBytes {
        self.relations
            .iter()
            .map(Relation::<R>::bytes)
            .fold(StoreBytes::default(), |sum, bytes| StoreBytes {
                input: sum.input + bytes.input,
                added: sum.added + bytes.added,
                delta: sum.delta + bytes.delta,
            })
    }

    /// Keeps the `(object, subject)` order of the relations whose predicate `objects`
    /// accepts and drops the others' (see [`Relation::<R>::by_subject_only`]).
    pub(super) fn objects(&mut self, objects: &(dyn Fn(u64) -> bool + Sync)) {
        self.relations
            .par_iter_mut()
            .zip(self.predicates.par_iter())
            .for_each(|(relation, &p)| relation.objects(objects(p)));
    }

    pub(crate) fn relation(&self, p: u64) -> Option<&Relation<R>> {
        self.index.get(&p).map(|&i| &self.relations[i])
    }

    /// Keeps the facts of `facts` (sorted by predicate, subject and object: a morsel's
    /// candidates) that the store doesn't hold. Each relation's runs are read forward from
    /// where the last probe landed ([`Finger`]), not searched from the root for each fact.
    /// Those searches were 54 % of LUBM 100's reasoning samples, but mostly the cache
    /// misses at their targets, which a finger keeps: the reasoning took 6 % less.
    /// Returns how often it started the runs from the beginning: once per relation.
    pub(super) fn retain_new(&self, facts: &mut Vec<R::Candidate>) -> u64 {
        let mut at: Option<(u64, Option<&Relation<R>>)> = None;
        let mut fingers = [PairsFinger::default(); RUNS];
        let mut starts = 0;
        facts.retain(|&fact| {
            let (p, pair) = R::candidate(fact);
            if at.is_none_or(|(q, _)| q != p) {
                at = Some((p, self.relation(p)));
                fingers = [PairsFinger::default(); RUNS];
                starts += 1;
            }
            let relation = at.and_then(|(_, relation)| relation);
            relation.is_none_or(|r| !r.contains_from(pair, &mut fingers))
        });
        starts
    }

    /// The relation of `p`, or every relation where `p` is open, with their predicates.
    pub(super) fn relations_of(
        &self,
        p: Option<u64>,
    ) -> Box<dyn Iterator<Item = (u64, &Relation<R>)> + '_> {
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
            delta.extend(pairs.iter().map(|&q| [R::first(q), p, R::second(q)]));
        }
        self.install(news);
        delta
    }

    /// [`Self::advance`] for candidates known to be absent from the store (filtered
    /// against it when they were derived), in lists of [`Segments<R>`]: skips the membership
    /// check, and returns only how many were new and whether one is a schema fact, not
    /// the facts.
    pub(crate) fn advance_new(
        &mut self,
        candidates: Vec<Candidates<R>>,
        schema: &Schema,
    ) -> (usize, bool) {
        let clock = std::time::Instant::now();
        let news = self.news(candidates, false);
        self.news_time += clock.elapsed();
        let schema_facts =
            news.par_iter()
                .zip(self.predicates.par_iter())
                .any(|((pairs, _), &p)| {
                    pairs
                        .iter()
                        .any(|&q| schema.is_schema_fact([R::first(q), p, R::second(q)]))
                });
        heap::phase("reasoner: install");
        self.install(news);
        let count = self.relations.iter().map(Relation::<R>::delta_len).sum();
        (count, schema_facts)
    }

    /// Every pair added since the input, as facts, in no particular order: what a
    /// materialisation from the input derived. The store is taken apart on the way, so
    /// the facts never sit beside all of it: the input runs and the `(object, subject)`
    /// orders go first, and each relation's runs once their facts are out.
    pub(crate) fn into_derived(self) -> Vec<Triple> {
        let runs: Vec<(u64, Vec<Arc<Pairs<R>>>)> = self
            .relations
            .into_iter()
            .zip(self.predicates)
            .map(|(relation, p)| (p, relation.into_added()))
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
                            out.extend(chunk.iter().map(|&q| [R::first(q), p, R::second(q)]));
                        }
                    }
                    Err(pairs) => out.extend(pairs.iter().map(|&q| [R::first(q), p, R::second(q)])),
                }
            }
        }
        out
    }

    /// What [`Self::into_derived`] lists, per predicate as the relations hold it
    /// ([`Output::Relations`]): their added runs, handed over, not copied.
    pub(super) fn into_relations(self) -> Vec<DerivedRelation> {
        let take = |run: Arc<Pairs<R>>| match Arc::try_unwrap(run) {
            Ok(pairs) => pairs.chunks,
            Err(shared) => shared.chunks.clone(),
        };
        let mut out: Vec<DerivedRelation> = self
            .relations
            .into_iter()
            .zip(self.predicates)
            .filter_map(|(mut relation, p)| {
                let mut runs = std::mem::take(&mut relation.added);
                runs.push(std::mem::take(&mut relation.delta));
                let runs: Vec<Run<R>> = runs.into_iter().filter(|run| run.len() > 0).collect();
                if runs.is_empty() {
                    return None;
                }
                let whole = runs.iter().all(|run| run.os.is_some());
                let (mut by_subject, mut by_object) = (Vec::new(), Vec::new());
                for run in runs {
                    by_subject.push(R::export(take(run.so)));
                    by_object.extend(run.os.map(|run| R::export(take(run))));
                }
                // In numbers: the caller sets the table that maps them (`dense::Dense::back`).
                Some(DerivedRelation {
                    predicate: p,
                    by_subject,
                    by_object: whole.then_some(by_object),
                })
            })
            .collect();
        out.sort_by_key(|relation| relation.predicate);
        out
    }

    /// The input's facts passing `input` and the added ones passing `added`, in no
    /// particular order, taking the store apart as [`Self::into_derived`] does: the
    /// input's are read first, then each relation's added runs, freed once read.
    pub(super) fn into_listed(
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
                    .map(move |&q| [R::first(q), p, R::second(q)])
                    .filter(|&fact| input(fact))
            })
            .collect();
        let runs: Vec<(u64, Vec<Arc<Pairs<R>>>)> = self
            .relations
            .into_iter()
            .zip(self.predicates)
            .map(|(relation, p)| (p, relation.into_added()))
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
                let list = |chunk: &[R], out: &mut Vec<Triple>| {
                    out.extend(
                        chunk
                            .iter()
                            .map(|&q| [R::first(q), p, R::second(q)])
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
    pub(super) fn facts_where(&self, keep: &(dyn Fn(Triple) -> bool + Sync)) -> Vec<Triple> {
        self.relations
            .par_iter()
            .zip(self.predicates.par_iter())
            .flat_map_iter(|(relation, &p)| {
                relation
                    .runs()
                    .flat_map(|run| run.so.iter())
                    .map(move |&q| [R::first(q), p, R::second(q)])
                    .filter(|&fact| keep(fact))
            })
            .collect()
    }

    /// Every stored fact mentioning one of `terms`, in any position (some more than once).
    /// By the subject and object orders where a run has them; a run without its object
    /// order is read whole for the objects.
    pub(super) fn mentioning(&self, terms: &HashSet<u64>) -> Vec<Triple> {
        self.relations
            .par_iter()
            .zip(self.predicates.par_iter())
            .flat_map_iter(|(relation, &p)| {
                let mut out = Vec::new();
                for run in relation.runs() {
                    if terms.contains(&p) {
                        out.extend(run.so.iter().map(|&q| [R::first(q), p, R::second(q)]));
                        continue;
                    }
                    for &t in terms {
                        out.extend(
                            run.so
                                .range(t)
                                .flatten()
                                .map(|&q| [R::first(q), p, R::second(q)]),
                        );
                    }
                    match &run.os {
                        Some(os) => {
                            for &t in terms {
                                out.extend(
                                    os.range(t)
                                        .flatten()
                                        .map(|&q| [R::second(q), p, R::first(q)]),
                                );
                            }
                        }
                        None => out.extend(
                            run.so
                                .iter()
                                .filter(|&&q| terms.contains(&R::second(q)))
                                .map(|&q| [R::first(q), p, R::second(q)]),
                        ),
                    }
                }
                out
            })
            .collect()
    }

    /// Makes `news` (per relation: the open part, the closed part) the relations' deltas.
    pub(super) fn install(&mut self, news: Vec<New<R>>) {
        self.merged += self
            .relations
            .par_iter_mut()
            .zip(news.into_par_iter())
            .map(|(relation, (pairs, marks))| relation.advance(pairs, marks) as u64)
            .sum::<u64>();
    }

    /// Reserves the closed part of `p`'s relation for `family` (a family that reads and
    /// produces it by name), unless one has it already.
    pub(super) fn claim(&mut self, p: u64, family: Family) {
        self.claims.entry(p).or_insert(family);
    }

    /// The new pairs of `candidates` per relation (relations added for new predicates),
    /// each sorted, distinct and in chunks without spare capacity: those a closed family
    /// produced in the relation's closed part (the first family that produces into a
    /// relation owns its closed part; another's go to the open part), the others in the
    /// open part. A pair in both parts is marked when the delta is built ([`marked_run`]).
    pub(super) fn news(&mut self, candidates: Vec<Candidates<R>>, check: bool) -> Vec<New<R>> {
        // Each predicate's lists together (a stable sort: the parts keep their order).
        let mut parts: Vec<(u64, Option<Family>, Vec<R>)> = candidates
            .into_iter()
            .flat_map(|(family, segments)| {
                segments
                    .into_iter()
                    .map(move |(p, pairs)| (p, family, pairs))
            })
            .filter(|(_, _, pairs)| !pairs.is_empty())
            .collect();
        parts.par_sort_by_key(|&(p, _, _)| p);
        type Lists<R> = (Vec<Vec<R>>, Vec<Vec<R>>);
        let mut groups: Vec<(usize, Lists<R>)> = Vec::new();
        for (p, family, pairs) in parts {
            let index = match self.index.get(&p) {
                Some(&index) => index,
                None => {
                    self.index.insert(p, self.relations.len());
                    self.relations.push(Relation::<R>::default());
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
        let groups: Vec<(usize, New<R>)> = groups
            .into_par_iter()
            .map(|(index, (mut open, mut closed))| {
                if check {
                    let relation = &self.relations[index];
                    for part in open.iter_mut().chain(closed.iter_mut()) {
                        part.retain(|&q| !relation.contains(R::first(q), R::second(q)));
                    }
                }
                (index, new_pairs(open, closed))
            })
            .collect();
        let mut news: Vec<New<R>> = self
            .relations
            .iter()
            .map(|_| (Pairs::<R>::default(), None))
            .collect();
        for (index, pairs) in groups {
            news[index] = pairs;
        }
        news
    }
}

impl<R: Representation> Source for Store<R> {
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
                Some(slices.iter().map(|slice| slice.pairs.len()).sum::<usize>())
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
            for slice in &slices {
                let (start, end) = (at, at + slice.pairs.len());
                at = end;
                if end <= range.start {
                    continue;
                }
                if start >= range.end {
                    return;
                }
                let part = range.start.max(start) - start..range.end.min(end) - start;
                slice.for_each_in(part, |q| {
                    let (a, b) = (R::first(q), R::second(q));
                    let (s, o) = if slice.swapped { (b, a) } else { (a, b) };
                    f([s, p, o]);
                });
            }
        }
    }
}

/// A relation of a store's delta ([`Store::<R>::delta_relations`]), read in ids.
pub(crate) struct DeltaRelation<'a, R: Representation> {
    relation: &'a Relation<R>,
    pub(crate) predicate: u64,
}

impl<R: Representation> DeltaRelation<'_, R> {
    /// Its delta's pairs.
    pub(crate) fn len(&self) -> usize {
        self.relation.delta_len()
    }

    /// Whether its delta has a pair with object `o`.
    pub(crate) fn has_object(&self, o: u64) -> bool {
        self.relation.delta_has_object(o)
    }

    /// Calls `f` with each distinct object of its delta.
    pub(crate) fn objects(&self, f: &mut dyn FnMut(u64)) {
        self.relation.delta_objects(f);
    }
}
