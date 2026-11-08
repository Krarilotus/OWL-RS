//! Rule evaluation shared by the batch executor ([`super::batch`]) and the delta executor
//! ([`super::delta`]). Everything here is generic over a [`Source`] of facts, which splits
//! them into the segments semi-naive evaluation needs.
//!
//! - [`Schema`] and [`ground`]: the schema atoms of a rule are evaluated against the facts
//!   and substituted into it (design §3.2). [`ground_delta`] grounds only through changed
//!   schema facts, so an edit to the TBox yields exactly the rule instances it adds or
//!   removes.
//! - [`Job`] and [`run_jobs`]: one variant of one rule, driven by its first atom's matches,
//!   run in parallel morsels; the other atoms are index lookups ordered by [`plan`].

use std::collections::HashMap;
use std::ops::ControlFlow;

use hashbrown::HashSet;
use rayon::prelude::*;

use super::ir::Triple;
use super::ir::{Atom, Guard, Head, OWL, RDF, RDFS, Rule, Term, Vocabulary};
use super::lists::Facts;

/// Driver matches per parallel work unit.
pub(crate) const MORSEL: usize = 4096;

/// Which facts an atom reads in a semi-naive variant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Seg {
    /// Facts known before the last round.
    Old,
    /// Facts the last round added.
    Delta,
    /// Both.
    All,
    /// The facts the last round added but those a closed rule family produced, the one
    /// whose schema link is this predicate (`rdfs:subClassOf` for `cax-sco`): what the
    /// family's own instances read, as they would derive nothing new from them (the
    /// batch executor's partitioned delta). Sources that don't partition their delta
    /// read it as [`Seg::Delta`] ([`Seg::plain`]).
    DeltaNotBy(u64),
}

impl Seg {
    /// The segment with [`Seg::DeltaNotBy`] read as the whole delta.
    pub fn plain(self) -> Seg {
        match self {
            Seg::DeltaNotBy(_) => Seg::Delta,
            seg => seg,
        }
    }
}

/// Facts, split into the last round's delta and the rest.
///
/// Every read takes the poller of the task that reads ([`Poller`]) and counts the work it
/// attempts there before doing it: a call is one unit, and each position a scan visits is
/// one more, the ones it filters out included (a read repeated per item of another
/// enumeration must not multiply after the stop: REAS-2, sweep M01–M17). The rule-evaluation
/// walk counts nothing itself: its source does. On a stopped poller a read does no more
/// work: a scan delivers nothing more, [`Source::estimate`] says 0, [`Source::contains`]
/// false and [`Source::matches_len`] `Some(0)`, answers that mean nothing; whoever reads them
/// asks its (latched) stop again and discards what it decided with them. A read that no stop
/// bounds takes [`Poller::never`], by name.
///
/// A read is written with the task's poller:
///
/// ```
/// use nrese_reasoner::eval::{Poller, Seg, Source};
/// fn polled<S: Source>(source: &S, poll: &Poller<'_>) -> usize {
///     source.estimate([None, None, None], Seg::All, poll)
/// }
/// ```
///
/// and can't be written without one:
///
/// ```compile_fail
/// use nrese_reasoner::eval::{Seg, Source};
/// fn unpolled<S: Source>(source: &S) -> usize {
///     source.estimate([None, None, None], Seg::All)
/// }
/// ```
///
/// The counting is the trait's, not each implementation's: an implementation provides
/// [`Visits`], whose scans deliver each fact through a [`Visit`] (which counts it before the
/// caller sees it) and whose probes get a [`Probe`] only after the call is counted. `Source`
/// itself is implemented for every `Visits` and for nothing else (master, 8 Nov 05:29: the
/// tick at the owner, where a leaf can't skip it). What a leaf still counts by itself are
/// the positions it filters out before delivering ([`Visit::skip`], [`Visit::ahead`]): those
/// leaves each have a counted test (the design note lists them).
pub trait Source: Sync + sealed::Read {
    /// Calls `f` with every fact of `seg` matching `pattern` (`None` = any term).
    fn scan(
        &self,
        pattern: [Option<u64>; 3],
        seg: Seg,
        poll: &Poller<'_>,
        f: &mut dyn FnMut(Triple),
    );
    /// An upper bound on the matches of `pattern` in `seg`, for planning. Zero must mean
    /// that nothing matches (or, on a stopped poller, nothing).
    fn estimate(&self, pattern: [Option<u64>; 3], seg: Seg, poll: &Poller<'_>) -> usize;
    /// Whether `fact` is in [`Seg::All`].
    fn contains(&self, fact: Triple, poll: &Poller<'_>) -> bool;
    /// The number of positions of `seg`'s matches of `pattern` in a fixed order, if the
    /// source can visit any range of them without collecting them first
    /// ([`Source::scan_range`]); `None` if it can't. A position may hold a fact that
    /// isn't in `seg` (the source skips it), so this may exceed the matches.
    fn matches_len(&self, pattern: [Option<u64>; 3], seg: Seg, poll: &Poller<'_>) -> Option<usize>;
    /// Calls `f` with the matches of `pattern` in `seg` at positions `range` of the order
    /// [`Source::matches_len`] counts, counting the range's positions before it reads them.
    /// Called only where that is `Some`.
    fn scan_range(
        &self,
        pattern: [Option<u64>; 3],
        seg: Seg,
        range: std::ops::Range<usize>,
        poll: &Poller<'_>,
        f: &mut dyn FnMut(Triple),
    );
}

/// What a [`Source`] implementation provides: its reads, each counted by the caller's side
/// before it runs ([`Source`]'s counting rule).
pub trait Visits: Sync {
    /// Hands every position of `seg`'s matches of `pattern` to `visit`: a match with
    /// [`Visit::fact`], a position filtered out with [`Visit::skip`] (or a run of them
    /// counted ahead, [`Visit::ahead`]); stops once either returns false.
    fn visit(&self, pattern: [Option<u64>; 3], seg: Seg, visit: &mut Visit<'_, '_>);
    /// [`Source::estimate`], the call already counted; further work on `probe`.
    fn size(&self, pattern: [Option<u64>; 3], seg: Seg, probe: Probe<'_, '_>) -> usize;
    /// [`Source::contains`], the call already counted; further work on `probe`.
    fn holds(&self, fact: Triple, probe: Probe<'_, '_>) -> bool;
    /// [`Source::matches_len`], the call already counted.
    fn positions(
        &self,
        _pattern: [Option<u64>; 3],
        _seg: Seg,
        _probe: Probe<'_, '_>,
    ) -> Option<usize> {
        None
    }
    /// [`Source::scan_range`], the range's positions already counted: its next
    /// `range.len()` positions are delivered or skipped without another tick.
    fn visit_range(
        &self,
        pattern: [Option<u64>; 3],
        seg: Seg,
        range: std::ops::Range<usize>,
        visit: &mut Visit<'_, '_>,
    ) {
        // Every position is visited (and counted, past the range counted ahead); those in
        // the range are delivered.
        let mut at = 0;
        visit.within(
            &mut |fact, outer| {
                if range.contains(&at) {
                    outer.counted(fact);
                }
                at += 1;
            },
            |inner| self.visit(pattern, seg, inner),
        );
    }
}

mod sealed {
    /// Only [`super::Visits`] implementations are [`super::Source`]s.
    pub trait Read {}
    impl<T: super::Visits + ?Sized> Read for T {}
}

impl<T: Visits + ?Sized> Source for T {
    #[inline]
    fn scan(
        &self,
        pattern: [Option<u64>; 3],
        seg: Seg,
        poll: &Poller<'_>,
        f: &mut dyn FnMut(Triple),
    ) {
        if poll.tick() {
            return;
        }
        self.visit(pattern, seg, &mut Visit::new(poll, f));
    }

    #[inline]
    fn estimate(&self, pattern: [Option<u64>; 3], seg: Seg, poll: &Poller<'_>) -> usize {
        match poll.tick() {
            true => 0,
            false => self.size(pattern, seg, Probe(poll)),
        }
    }

    #[inline]
    fn contains(&self, fact: Triple, poll: &Poller<'_>) -> bool {
        !poll.tick() && self.holds(fact, Probe(poll))
    }

    fn matches_len(&self, pattern: [Option<u64>; 3], seg: Seg, poll: &Poller<'_>) -> Option<usize> {
        match poll.tick() {
            true => Some(0),
            false => self.positions(pattern, seg, Probe(poll)),
        }
    }

    fn scan_range(
        &self,
        pattern: [Option<u64>; 3],
        seg: Seg,
        range: std::ops::Range<usize>,
        poll: &Poller<'_>,
        f: &mut dyn FnMut(Triple),
    ) {
        let mut visit = Visit::new(poll, f);
        if visit.ahead(range.len()) {
            self.visit_range(pattern, seg, range, &mut visit);
        }
    }
}

/// How a source hands a scan's positions to its reader: each counted on the reading task's
/// poller before the reader sees it, so no source can deliver work the poller didn't count.
/// Only [`Source`]'s reads make one; a source that reads another hands it on
/// ([`Visit::within`]).
pub struct Visit<'v, 'p> {
    poll: &'v Poller<'p>,
    f: &'v mut dyn FnMut(Triple),
    /// Positions counted ahead ([`Visit::ahead`]), delivered or skipped without a tick.
    ahead: usize,
}

impl<'v, 'p> Visit<'v, 'p> {
    /// For the reads of [`Source`] and [`crate::delta::Base`], after they count the call.
    pub(crate) fn new(poll: &'v Poller<'p>, f: &'v mut dyn FnMut(Triple)) -> Self {
        Self { poll, f, ahead: 0 }
    }

    /// Counts one position; false once the poller has stopped. A position of a run counted
    /// ahead ([`Self::ahead`]) isn't checked again: the run's own count asked the poller
    /// before it began, and the next count after it asks again, so a stop that fires inside
    /// the run is seen within the run, at most `POLL_UNITS` long (REAS-2's bound: `POLL_UNITS`
    /// plus one run).
    #[inline]
    fn count(&mut self) -> bool {
        match self.ahead {
            0 => !self.poll.tick(),
            _ => {
                self.ahead -= 1;
                true
            }
        }
    }

    /// One position, a match: counted, then delivered. False once the poller has stopped
    /// (then nothing is delivered, and the source visits no more).
    #[inline]
    pub fn fact(&mut self, fact: Triple) -> bool {
        if !self.count() {
            return false;
        }
        (self.f)(fact);
        true
    }

    /// One position passed over (filtered out before delivery): counted. False once the
    /// poller has stopped.
    #[inline]
    pub fn skip(&mut self) -> bool {
        self.count()
    }

    /// `n` positions about to be visited (a slice, a run), counted up front: the next `n`
    /// [`Self::fact`] and [`Self::skip`] calls don't tick or check the poller again. False
    /// once stopped. `n` is at most `POLL_UNITS` (the store's runs, `batch::RUN`, and the
    /// morsels' ranges, `MORSEL`): a longer run would deliver past a stop by its length.
    #[inline]
    pub fn ahead(&mut self, n: usize) -> bool {
        debug_assert!(
            n <= nrese_exec::POLL_UNITS as usize,
            "a run of {n} positions counted ahead: at most POLL_UNITS"
        );
        if self.poll.attempt(n) {
            return false;
        }
        self.ahead += n;
        true
    }

    /// Ends a run counted ahead: what of it went unvisited (positions passed over without
    /// [`Self::skip`], counted already) is dropped, so later positions count again.
    #[inline]
    pub fn settle(&mut self) {
        self.ahead = 0;
    }

    /// Whether the poller has stopped: the source visits no more.
    #[inline]
    pub fn stopped(&self) -> bool {
        self.poll.stopped()
    }

    /// The poller, for this source's own probes of other sources (an overlay's membership
    /// checks of what its base delivers).
    pub fn poller(&self) -> &'v Poller<'p> {
        self.poll
    }

    /// Reads `read` (another source's visit, of the source this one wraps) and delivers
    /// what passes `keep`: counted once, by the source that visits (a wrapper's filter).
    #[inline]
    pub fn filtered(
        &mut self,
        keep: &dyn Fn(Triple) -> bool,
        read: impl FnOnce(&mut Visit<'_, 'p>),
    ) {
        self.within(
            &mut |fact, outer| {
                if keep(fact) {
                    outer.counted(fact);
                }
            },
            read,
        );
    }

    /// Reads `read` with a visit on the same poller whose deliveries go to `through` with
    /// this visit: each counted by the inner source, so `through` hands them on with
    /// [`Self::counted`] and no second tick. Positions counted ahead go to the inner visit,
    /// which visits them.
    fn within(
        &mut self,
        through: &mut dyn FnMut(Triple, &mut Visit<'v, 'p>),
        read: impl FnOnce(&mut Visit<'_, 'p>),
    ) {
        let poll = self.poll;
        let ahead = std::mem::take(&mut self.ahead);
        let outer = &mut *self;
        let mut deliver = |fact: Triple| through(fact, &mut *outer);
        let mut inner = Visit {
            poll,
            f: &mut deliver,
            ahead,
        };
        read(&mut inner);
    }

    /// Delivers a fact an inner read counted ([`Self::within`]; a crate source over a
    /// [`crate::delta::Base`] it reads with [`crate::delta::Base::scan`] on this visit's
    /// poller); nothing once stopped. Not for sources outside the crate.
    #[inline]
    pub(crate) fn counted(&mut self, fact: Triple) {
        if !self.poll.stopped() {
            (self.f)(fact);
        }
    }
}

/// A probe's further work ([`Visits::size`], [`Visits::holds`]), the call already counted:
/// what more it reads is counted with [`Probe::units`], and the sources it asks are asked
/// on it.
#[derive(Clone, Copy)]
pub struct Probe<'v, 'p>(&'v Poller<'p>);

impl<'v, 'p> Probe<'v, 'p> {
    /// For the probes of [`Source`] and [`crate::delta::Base`], after they count the call.
    pub(crate) fn of(poll: &'v Poller<'p>) -> Self {
        Self(poll)
    }

    /// Counts `n` more units of the probe's work, at most `POLL_UNITS` ([`Poller::attempt`]);
    /// false once the poller has stopped. A longer list of work goes through
    /// [`Self::counted`].
    #[inline]
    pub fn units(self, n: usize) -> bool {
        !self.0.attempt(n)
    }

    /// [`Poller::counted`] on the probe's poller.
    pub fn counted<I: IntoIterator>(self, items: I) -> Counted<'v, 'p, I::IntoIter> {
        self.0.counted(items)
    }

    /// Counts `n` units of work another owner reports as it does it (an engine count's
    /// visitor, `Snapshot::count_in_visiting`): a step it took in one go, counted in runs
    /// of at most `POLL_UNITS`; false once the poller has stopped, which tells the owner to
    /// stop.
    pub fn visited(self, n: u64) -> bool {
        let run = u64::from(nrese_exec::POLL_UNITS);
        let mut left = n;
        while left > 0 {
            let k = left.min(run);
            if self.0.attempt(k as usize) {
                return false;
            }
            left -= k;
        }
        !self.0.stopped()
    }

    /// The poller, for the probe's reads of other sources.
    pub fn poller(self) -> &'v Poller<'p> {
        self.0
    }
}

/// A [`Source`]'s facts (all segments) for list instantiation, which is grounding: it runs
/// to its end ([`Poller::never`]; a program grounded part-way would be kept as if whole).
pub struct AllFacts<'a, S: ?Sized>(pub &'a S);

impl<S: Source + ?Sized> Facts for AllFacts<'_, S> {
    fn objects(&self, subject: u64, predicate: u64) -> Vec<u64> {
        let mut out = Vec::new();
        self.0.scan(
            [Some(subject), Some(predicate), None],
            Seg::All,
            &Poller::never(),
            &mut |t| out.push(t[2]),
        );
        out.sort_unstable();
        out.dedup();
        out
    }

    fn pairs(&self, predicate: u64) -> Vec<(u64, u64)> {
        let mut out = Vec::new();
        self.0.scan(
            [None, Some(predicate), None],
            Seg::All,
            &Poller::never(),
            &mut |t| out.push((t[0], t[2])),
        );
        out.sort_unstable();
        out.dedup();
        out
    }
}

/// The TBox vocabulary. Atoms over these predicates, and `rdf:type` atoms with these
/// classes as object, are grounded before evaluation.
#[derive(Clone)]
pub struct Schema {
    rdf_type: u64,
    thing: u64,
    /// Unnamed classes whose memberships aren't derived (work package W7,
    /// [`super::unnamed`]): each with whether a membership stands for `owl:Thing`.
    hidden: HashMap<u64, bool>,
    predicates: HashSet<u64>,
    classes: HashSet<u64>,
}

impl Schema {
    /// The id of `rdf:type`.
    pub fn rdf_type(&self) -> u64 {
        self.rdf_type
    }

    /// The RDFS and OWL 2 schema vocabulary. It includes the list vocabulary, so a change
    /// to a list axiom re-instantiates the list rules.
    pub fn owl(vocabulary: &mut impl Vocabulary) -> Self {
        let rdf_type = vocabulary.iri(&format!("{RDF}type"));
        let mut predicates = HashSet::new();
        for local in ["subClassOf", "subPropertyOf", "domain", "range"] {
            predicates.insert(vocabulary.iri(&format!("{RDFS}{local}")));
        }
        for local in ["first", "rest"] {
            predicates.insert(vocabulary.iri(&format!("{RDF}{local}")));
        }
        for local in [
            "equivalentClass",
            "equivalentProperty",
            "inverseOf",
            "onProperty",
            "onClass",
            "someValuesFrom",
            "allValuesFrom",
            "hasValue",
            "maxCardinality",
            "maxQualifiedCardinality",
            "disjointWith",
            "complementOf",
            "propertyDisjointWith",
            "sourceIndividual",
            "assertionProperty",
            "targetIndividual",
            "targetValue",
            "intersectionOf",
            "unionOf",
            "oneOf",
            "hasKey",
            "propertyChainAxiom",
            "members",
            "distinctMembers",
        ] {
            predicates.insert(vocabulary.iri(&format!("{OWL}{local}")));
        }
        let classes = [
            "Class",
            "ObjectProperty",
            "DatatypeProperty",
            "TransitiveProperty",
            "SymmetricProperty",
            "AsymmetricProperty",
            "FunctionalProperty",
            "InverseFunctionalProperty",
            "IrreflexiveProperty",
            "AllDifferent",
            "AllDisjointClasses",
            "AllDisjointProperties",
        ]
        .iter()
        .map(|local| vocabulary.iri(&format!("{OWL}{local}")))
        .collect();
        Self {
            rdf_type,
            thing: vocabulary.iri(&format!("{OWL}Thing")),
            hidden: HashMap::new(),
            predicates,
            classes,
        }
    }

    /// This schema, with memberships in `hidden` classes not derived: a ground rule that
    /// would derive one derives `owl:Thing` instead where the value is `true`, else
    /// nothing ([`super::unnamed`]).
    #[must_use]
    pub fn hiding(mut self, hidden: HashMap<u64, bool>) -> Self {
        self.hidden = hidden;
        self
    }

    pub fn hidden(&self) -> &HashMap<u64, bool> {
        &self.hidden
    }

    /// `rule` with its heads' memberships in hidden classes rewritten; `None` if nothing
    /// is left to derive.
    fn rewrite_heads(&self, mut rule: Rule) -> Option<Rule> {
        if self.hidden.is_empty() {
            return Some(rule);
        }
        if let Head::Facts(atoms) = &mut rule.head {
            atoms.retain_mut(|atom| match atom.0 {
                [_, Term::Const(p), Term::Const(c)] if p == self.rdf_type => {
                    match self.hidden.get(&c) {
                        Some(true) => {
                            atom.0[2] = Term::Const(self.thing);
                            true
                        }
                        Some(false) => false,
                        None => true,
                    }
                }
                _ => true,
            });
            if atoms.is_empty() {
                return None;
            }
        }
        Some(rule)
    }

    pub fn is_schema_atom(&self, atom: &Atom) -> bool {
        match atom.0 {
            [_, Term::Const(p), _] if self.predicates.contains(&p) => true,
            [_, Term::Const(p), Term::Const(c)] => p == self.rdf_type && self.classes.contains(&c),
            _ => false,
        }
    }

    /// Whether grounding reads relation `p` (schema atoms: a schema predicate, or
    /// `rdf:type` with a schema class), where any order may be needed.
    pub(crate) fn read_in_grounding(&self, p: u64) -> bool {
        p == self.rdf_type || self.predicates.contains(&p)
    }

    pub fn is_schema_fact(&self, [_, p, o]: Triple) -> bool {
        self.predicates.contains(&p) || (p == self.rdf_type && self.classes.contains(&o))
    }
}

pub fn value(term: Term, bindings: &[Option<u64>]) -> Option<u64> {
    match term {
        Term::Const(c) => Some(c),
        Term::Var(v) => bindings[usize::from(v)],
    }
}

/// The atom's pattern under `bindings` (`None` for free positions).
pub fn pattern(atom: &Atom, bindings: &[Option<u64>]) -> [Option<u64>; 3] {
    atom.0.map(|t| value(t, bindings))
}

/// The atom's constants (`None` for variables).
pub fn constants(atom: &Atom) -> [Option<u64>; 3] {
    atom.0.map(|t| match t {
        Term::Const(c) => Some(c),
        Term::Var(_) => None,
    })
}

/// Binds the atom's free variables to `fact`; `None` if a repeated variable disagrees.
/// Returns the variables it bound, as a bitmask over the atom's three positions.
pub fn bind(atom: &Atom, fact: Triple, bindings: &mut [Option<u64>]) -> Option<u8> {
    let mut newly = 0u8;
    for (position, term) in atom.0.iter().enumerate() {
        if let Term::Var(v) = *term {
            match bindings[usize::from(v)] {
                Some(existing) if existing != fact[position] => {
                    unbind(atom, newly, bindings);
                    return None;
                }
                Some(_) => {}
                None => {
                    bindings[usize::from(v)] = Some(fact[position]);
                    newly |= 1 << position;
                }
            }
        } else if let Term::Const(c) = *term
            && c != fact[position]
        {
            unbind(atom, newly, bindings);
            return None;
        }
    }
    Some(newly)
}

pub fn unbind(atom: &Atom, newly: u8, bindings: &mut [Option<u64>]) {
    for (position, term) in atom.0.iter().enumerate() {
        if newly & (1 << position) != 0
            && let Term::Var(v) = *term
        {
            bindings[usize::from(v)] = None;
        }
    }
}

/// Guards whose terms are both bound; unbound ones are decided later.
pub fn guards_hold(guards: &[Guard], bindings: &[Option<u64>]) -> bool {
    guards
        .iter()
        .all(|guard| guard.holds(|term| value(term, bindings)))
}

/// Evaluates `order[depth..]` of `body`, calling `emit` for each complete binding. Its
/// scans count on `poll` ([`Source`]: each scan and each fact it visits, bound or not);
/// once its stop has fired, they deliver nothing more and nothing deeper is scanned, so
/// one driver's fan-out can't outlive the stop by more than [`nrese_exec::POLL_UNITS`]
/// units (RSH-18).
#[allow(clippy::too_many_arguments)]
pub fn walk<S: Source + ?Sized>(
    source: &S,
    body: &[Atom],
    guards: &[Guard],
    order: &[(usize, Seg)],
    depth: usize,
    bindings: &mut [Option<u64>],
    poll: &Poller<'_>,
    emit: &mut dyn FnMut(&[Option<u64>]),
) {
    let Some(&(index, seg)) = order.get(depth) else {
        if guards_hold(guards, bindings) {
            emit(bindings);
        }
        return;
    };
    let atom = &body[index];
    source.scan(pattern(atom, bindings), seg, poll, &mut |fact| {
        if let Some(newly) = bind(atom, fact, bindings) {
            walk(source, body, guards, order, depth + 1, bindings, poll, emit);
            unbind(atom, newly, bindings);
        }
    });
}

/// [`walk`] that stops once `emit` returns true: the rest of a scan under way is passed
/// over (a scan can't be stopped) and nothing deeper is scanned, so asking for one
/// derivation doesn't enumerate the whole join (the review of 3 October 2026, P6).
/// Returns whether `emit` stopped it. It polls as [`walk`] does: a search `poll` stopped
/// returns false, and what it found so far is incomplete ([`Poller::stopped`]). Kept apart
/// from [`walk`], which materialisation runs and which `emit` never stops.
#[allow(clippy::too_many_arguments)]
pub fn walk_until<S: Source + ?Sized>(
    source: &S,
    body: &[Atom],
    guards: &[Guard],
    order: &[(usize, Seg)],
    depth: usize,
    bindings: &mut [Option<u64>],
    poll: &Poller<'_>,
    emit: &mut dyn FnMut(&[Option<u64>]) -> bool,
) -> bool {
    let Some(&(index, seg)) = order.get(depth) else {
        return guards_hold(guards, bindings) && emit(bindings);
    };
    let atom = &body[index];
    let mut stopped = false;
    source.scan(pattern(atom, bindings), seg, poll, &mut |fact| {
        if stopped {
            return;
        }
        if let Some(newly) = bind(atom, fact, bindings) {
            stopped = walk_until(source, body, guards, order, depth + 1, bindings, poll, emit);
            unbind(atom, newly, bindings);
        }
    });
    stopped
}

/// Orders `atoms` after `first`: most bound positions first, then fewest matches (each
/// estimate counted on `poll`).
pub fn plan<S: Source + ?Sized>(
    source: &S,
    rule: &Rule,
    atoms: &[usize],
    first: usize,
    seg_of: impl Fn(usize) -> Seg,
    poll: &Poller<'_>,
) -> Vec<(usize, Seg)> {
    plan_bound(
        source,
        rule,
        atoms,
        first,
        &vec![false; rule.variables()],
        seg_of,
        poll,
    )
}

/// [`plan`], with the variables in `bound` bound beforehand.
fn plan_bound<S: Source + ?Sized>(
    source: &S,
    rule: &Rule,
    atoms: &[usize],
    first: usize,
    bound: &[bool],
    seg_of: impl Fn(usize) -> Seg,
    poll: &Poller<'_>,
) -> Vec<(usize, Seg)> {
    fn mark(atom: &Atom, bound: &mut [bool]) {
        for term in atom.0 {
            if let Term::Var(v) = term {
                bound[usize::from(v)] = true;
            }
        }
    }
    let mut bound = bound.to_vec();
    let mut order = vec![(first, seg_of(first))];
    mark(&rule.body[first], &mut bound);
    let mut rest: Vec<usize> = atoms.iter().copied().filter(|&i| i != first).collect();
    while !rest.is_empty() {
        let (k, _) = rest
            .iter()
            .enumerate()
            .max_by_key(|&(_, &i)| {
                let atom = &rule.body[i];
                let known = atom
                    .0
                    .iter()
                    .filter(|t| match t {
                        Term::Const(_) => true,
                        Term::Var(v) => bound[usize::from(*v)],
                    })
                    .count();
                (
                    known,
                    std::cmp::Reverse(source.estimate(constants(atom), seg_of(i), poll)),
                )
            })
            .expect("rest is not empty");
        let i = rest.remove(k);
        mark(&rule.body[i], &mut bound);
        order.push((i, seg_of(i)));
    }
    order
}

fn substitute(term: Term, substitution: &[Option<u64>]) -> Term {
    match term {
        Term::Var(v) => substitution[usize::from(v)].map_or(term, Term::Const),
        constant => constant,
    }
}

fn substitute_atom(atom: &Atom, substitution: &[Option<u64>]) -> Atom {
    Atom(atom.0.map(|t| substitute(t, substitution)))
}

/// A rule after grounding its schema atoms.
#[derive(Clone)]
pub struct Grounded {
    pub rule: Rule,
    /// The schema variables' values; the rest is `None`.
    pub substitution: Vec<Option<u64>>,
}

/// The rule instance for one binding of the schema variables; `None` if a guard fails.
fn instance(
    rule: &Rule,
    instance_atoms: &[usize],
    substitution: &[Option<u64>],
) -> Option<Grounded> {
    let mut guards = Vec::new();
    for guard in &rule.guards {
        match *guard {
            Guard::NotEqual(a, b) => {
                match (substitute(a, substitution), substitute(b, substitution)) {
                    (Term::Const(a), Term::Const(b)) if a == b => return None,
                    (Term::Const(_), Term::Const(_)) => {}
                    (a, b) => guards.push(Guard::NotEqual(a, b)),
                }
            }
            Guard::NotIn(term, low, high) => match substitute(term, substitution) {
                Term::Const(v) if (low..=high).contains(&v) => return None,
                Term::Const(_) => {}
                term => guards.push(Guard::NotIn(term, low, high)),
            },
            Guard::SameList(a, b, ref index) => {
                match (substitute(a, substitution), substitute(b, substitution)) {
                    (Term::Const(a), Term::Const(b)) if a >= b || !index.together(a, b) => {
                        return None;
                    }
                    (Term::Const(_), Term::Const(_)) => {}
                    (a, b) => guards.push(Guard::SameList(a, b, index.clone())),
                }
            }
            // Kept with the schema variables' values, which stand for their classes: the
            // classes can still grow. An instance without body atoms concludes now, so its
            // guards are decided now.
            Guard::OverClasses {
                guards: ref over,
                ref bound,
                ref classes,
            } => {
                let mut bound = bound.clone();
                for term in over.iter().flat_map(Guard::terms) {
                    if let Term::Var(v) = term
                        && let Some(id) = substitution[usize::from(v)]
                        && !bound.iter().any(|&(b, _)| b == v)
                    {
                        bound.push((v, id));
                    }
                }
                let guard = Guard::OverClasses {
                    guards: over.clone(),
                    bound,
                    classes: classes.clone(),
                };
                if instance_atoms.is_empty()
                    && matches!(rule.head, Head::Facts(_))
                    && !guard.holds(|term| value(term, substitution))
                {
                    return None;
                }
                guards.push(guard);
            }
        }
    }
    let head = match &rule.head {
        Head::Facts(atoms) => Head::Facts(
            atoms
                .iter()
                .map(|a| substitute_atom(a, substitution))
                .collect(),
        ),
        Head::Inconsistent => Head::Inconsistent,
    };
    Some(Grounded {
        rule: Rule {
            name: rule.name.clone(),
            body: instance_atoms
                .iter()
                .map(|&i| substitute_atom(&rule.body[i], substitution))
                .collect(),
            guards,
            head,
        },
        substitution: substitution.to_vec(),
    })
}

fn split_schema(schema: &Schema, rule: &Rule) -> (Vec<usize>, Vec<usize>) {
    (0..rule.body.len()).partition(|&i| schema.is_schema_atom(&rule.body[i]))
}

/// Grounds `rule`'s schema atoms against all of `source`: calls `emit` with every
/// instance rule. A rule without schema atoms is emitted as it is.
pub fn ground<S: Source + ?Sized>(
    source: &S,
    schema: &Schema,
    rule: &Rule,
    emit: &mut dyn FnMut(Grounded),
) {
    let (schema_atoms, instance_atoms) = split_schema(schema, rule);
    if schema_atoms.is_empty() {
        emit(Grounded {
            rule: rule.clone(),
            substitution: vec![None; rule.variables()],
        });
        return;
    }
    // Grounding runs to its end: a program grounded part-way would be kept as if whole.
    let poll = Poller::never();
    let first = *schema_atoms
        .iter()
        .min_by_key(|&&i| source.estimate(constants(&rule.body[i]), Seg::All, &poll))
        .expect("not empty");
    let order = plan(source, rule, &schema_atoms, first, |_| Seg::All, &poll);
    let mut bindings = vec![None; rule.variables()];
    walk(
        source,
        &rule.body,
        &rule.guards,
        &order,
        0,
        &mut bindings,
        &poll,
        &mut |substitution| {
            if let Some(grounded) = instance(rule, &instance_atoms, substitution) {
                emit(grounded);
            }
        },
    );
}

/// Grounds `rule` only through the delta: the instances whose schema binding uses at least
/// one fact of [`Seg::Delta`] (the semi-naive variants of the schema atoms). Each such
/// instance is emitted at least once. Rules without schema atoms emit nothing.
pub fn ground_delta<S: Source + ?Sized>(
    source: &S,
    schema: &Schema,
    rule: &Rule,
    emit: &mut dyn FnMut(Grounded),
) {
    let (schema_atoms, instance_atoms) = split_schema(schema, rule);
    // Runs to its end, as [`ground`] does.
    let poll = Poller::never();
    for (k, &first) in schema_atoms.iter().enumerate() {
        if source.estimate(constants(&rule.body[first]), Seg::Delta, &poll) == 0 {
            continue;
        }
        let seg_of = |i: usize| {
            let position = schema_atoms
                .iter()
                .position(|&a| a == i)
                .expect("a schema atom");
            match position.cmp(&k) {
                std::cmp::Ordering::Less => Seg::Old,
                std::cmp::Ordering::Equal => Seg::Delta,
                std::cmp::Ordering::Greater => Seg::All,
            }
        };
        let order = plan(source, rule, &schema_atoms, first, seg_of, &poll);
        let mut bindings = vec![None; rule.variables()];
        walk(
            source,
            &rule.body,
            &rule.guards,
            &order,
            0,
            &mut bindings,
            &poll,
            &mut |substitution| {
                if let Some(grounded) = instance(rule, &instance_atoms, substitution) {
                    emit(grounded);
                }
            },
        );
    }
}

/// The fact a head atom derives under `bindings`.
pub fn instantiate_head(atom: &Atom, bindings: &[Option<u64>]) -> Triple {
    atom.0
        .map(|t| value(t, bindings).expect("safe rules bind head variables"))
}

/// One variant of one rule: the atom order (driver first) and the driver's matches.
pub struct Job<'r> {
    pub rule: &'r Rule,
    order: Vec<(usize, Seg)>,
    /// The driver atom's constants and segment.
    pattern: [Option<u64>; 3],
    seg: Seg,
    drivers: Drivers,
}

/// A job's driver matches: collected, or read in place by position where the source can
/// ([`Source::matches_len`]). Collecting copied every match of every job before a round
/// ran: 391 M facts (9.4 GB) in the second OWL 2 RL round of LUBM 1000, the round's
/// memory peak (5 October 2026).
enum Drivers {
    Collected(Vec<Triple>),
    InPlace(usize),
}

impl<'r> Job<'r> {
    /// The variant driven by `first` (read from `seg_of(first)`), or `None` if it can't
    /// match. Its reads (estimates, the drivers) count on `poll`: on a stopped one it is
    /// `None`, which means nothing (sweep M01).
    pub fn new<S: Source + ?Sized>(
        source: &S,
        rule: &'r Rule,
        first: usize,
        seg_of: impl Fn(usize) -> Seg,
        poll: &Poller<'_>,
    ) -> Option<Self> {
        // An atom over a predicate without facts can't match.
        for (i, atom) in rule.body.iter().enumerate() {
            if let Term::Const(p) = atom.0[1]
                && source.estimate([None, Some(p), None], seg_of(i), poll) == 0
            {
                return None;
            }
        }
        let atoms: Vec<usize> = (0..rule.body.len()).collect();
        let order = plan(source, rule, &atoms, first, &seg_of, poll);
        let (pattern, seg) = (constants(&rule.body[first]), seg_of(first));
        let drivers = match source.matches_len(pattern, seg, poll) {
            Some(0) => return None,
            Some(len) => Drivers::InPlace(len),
            None => {
                let mut drivers = Vec::new();
                source.scan(pattern, seg, poll, &mut |fact| drivers.push(fact));
                if drivers.is_empty() {
                    return None;
                }
                Drivers::Collected(drivers)
            }
        };
        Some(Self {
            rule,
            order,
            pattern,
            seg,
            drivers,
        })
    }

    /// The semi-naive variant with atom `i` on the delta: atoms before it read the old
    /// facts, atoms after it all facts.
    pub fn variant<S: Source + ?Sized>(
        source: &S,
        rule: &'r Rule,
        i: usize,
        poll: &Poller<'_>,
    ) -> Option<Self> {
        Self::variant_reading(source, rule, i, Seg::Delta, poll)
    }

    /// [`Job::variant`] with atom `i` reading `delta` (the delta, or the delta without
    /// what a closed family produced: [`Seg::DeltaNotBy`]).
    pub fn variant_reading<S: Source + ?Sized>(
        source: &S,
        rule: &'r Rule,
        i: usize,
        delta: Seg,
        poll: &Poller<'_>,
    ) -> Option<Self> {
        let seg_of = |j: usize| match j.cmp(&i) {
            std::cmp::Ordering::Less => Seg::Old,
            std::cmp::Ordering::Equal => delta,
            std::cmp::Ordering::Greater => Seg::All,
        };
        Self::new(source, rule, i, seg_of, poll)
    }

    /// The full evaluation over all facts, driven by the most selective atom.
    pub fn full<S: Source + ?Sized>(source: &S, rule: &'r Rule, poll: &Poller<'_>) -> Option<Self> {
        let first = (0..rule.body.len())
            .min_by_key(|&i| source.estimate(constants(&rule.body[i]), Seg::All, poll))
            .expect("a rule with a body");
        Self::new(source, rule, first, |_| Seg::All, poll)
    }

    /// The bytes of driver matches the job copied out of its source: none where the
    /// source reads them in place.
    pub fn copied_bytes(&self) -> usize {
        match &self.drivers {
            Drivers::Collected(drivers) => drivers.len() * std::mem::size_of::<Triple>(),
            Drivers::InPlace(_) => 0,
        }
    }

    /// The driver positions, which [`Job::run`] takes ranges of.
    pub fn drivers(&self) -> usize {
        match &self.drivers {
            Drivers::Collected(drivers) => drivers.len(),
            Drivers::InPlace(len) => *len,
        }
    }

    /// Runs the variant for `drivers[range]`, calling `emit` with each complete binding,
    /// counting the range's driver positions before it is read (a source may filter some
    /// out before they reach the walk: [`Source::scan_range`] counts them; collected ones
    /// are counted here) and each read of the walk on `poll` ([`walk`]): once its stop has
    /// fired, the rest is passed over and `poll` says so ([`Poller::stopped`]).
    pub fn run<S: Source + ?Sized>(
        &self,
        source: &S,
        range: std::ops::Range<usize>,
        poll: &Poller<'_>,
        emit: &mut dyn FnMut(&[Option<u64>]),
    ) {
        if self.counts_collected(&range, poll) {
            return;
        }
        let mut bindings = vec![None; self.rule.variables()];
        let driver = &self.rule.body[self.order[0].0];
        let mut visit = |fact: Triple| {
            if poll.stopped() {
                return;
            }
            if let Some(newly) = bind(driver, fact, &mut bindings) {
                walk(
                    source,
                    &self.rule.body,
                    &self.rule.guards,
                    &self.order,
                    1,
                    &mut bindings,
                    poll,
                    emit,
                );
                unbind(driver, newly, &mut bindings);
            }
        };
        match &self.drivers {
            Drivers::Collected(drivers) => drivers[range].iter().for_each(|&fact| visit(fact)),
            Drivers::InPlace(_) => {
                source.scan_range(self.pattern, self.seg, range, poll, &mut visit)
            }
        }
    }

    /// Counts a range of collected drivers before it is read (true: stopped); in-place
    /// ones are the source's to count ([`Source::scan_range`]).
    fn counts_collected(&self, range: &std::ops::Range<usize>, poll: &Poller<'_>) -> bool {
        match self.drivers {
            Drivers::Collected(_) => poll.attempt(range.len()),
            Drivers::InPlace(_) => poll.stopped(),
        }
    }

    /// [`Job::run`] that stops once `emit` returns true ([`walk_until`]): the rest of the
    /// scans under way, the drivers' included, is passed over. Returns whether `emit`
    /// stopped it. It polls `poll` as [`Job::run`] does: once that has stopped, what was
    /// emitted is incomplete ([`Poller::stopped`]). For the consistency check, whose report
    /// can refuse; materialisation runs [`Job::run`], which `emit` never stops.
    pub fn run_until<S: Source + ?Sized>(
        &self,
        source: &S,
        range: std::ops::Range<usize>,
        poll: &Poller<'_>,
        emit: &mut dyn FnMut(&[Option<u64>]) -> bool,
    ) -> bool {
        // The range's driver positions, before it is read ([`Job::run`]).
        if self.counts_collected(&range, poll) {
            return false;
        }
        let mut bindings = vec![None; self.rule.variables()];
        let driver = &self.rule.body[self.order[0].0];
        let mut stopped = false;
        let mut visit = |fact: Triple| {
            if stopped || poll.stopped() {
                return;
            }
            if let Some(newly) = bind(driver, fact, &mut bindings) {
                stopped = walk_until(
                    source,
                    &self.rule.body,
                    &self.rule.guards,
                    &self.order,
                    1,
                    &mut bindings,
                    poll,
                    emit,
                );
                unbind(driver, newly, &mut bindings);
            }
        };
        match &self.drivers {
            Drivers::Collected(drivers) => drivers[range].iter().for_each(|&fact| visit(fact)),
            Drivers::InPlace(_) => {
                source.scan_range(self.pattern, self.seg, range, poll, &mut visit)
            }
        }
        stopped
    }
}

/// Asked between units of work: `true` means stop (the caller gave up). It is asked at each
/// morsel's start and, through a [`Poller`], every [`nrese_exec::POLL_UNITS`] facts a
/// morsel scans: inside a source's scan, so it must take no lock (the store's stops read
/// atomics, a clock and the process's memory).
pub type Stop<'a> = &'a (dyn Fn() -> bool + Sync);

/// What a task polls a [`Stop`] with: nrese-exec's [`Ticker`](nrese_exec::Ticker) over it,
/// one unit per unit of work attempted, and whether it has fired. One per morsel, check
/// job, search or sequential stage (each a sequential task), owned by that task and never
/// shared across threads. It counts through `&self` (the ticker's countdown and the stopped
/// flag are cells: a plain load and store per tick, no borrow flag, no atomic), so a
/// source's scan can tick it while the callback it calls reads through it too ([`Source`]);
/// `Send`, not `Sync`. The shared stop is read only when the countdown runs out.
pub struct Poller<'a> {
    ticker: nrese_exec::Ticker<Stop<'a>>,
    stopped: std::cell::Cell<bool>,
}

impl<'a> Poller<'a> {
    /// A poller of `stop`, or `None` if it has fired already.
    pub fn new(stop: Stop<'a>) -> Option<Self> {
        nrese_exec::Ticker::new(stop).ok().map(|ticker| Self {
            ticker,
            stopped: std::cell::Cell::new(false),
        })
    }

    /// One that never fires, for work that runs to its end: grounding (a program grounded
    /// part-way would be kept as if whole), explanations and the support sets.
    pub fn never() -> Poller<'static> {
        Poller::new(NEVER).expect("NEVER doesn't stop")
    }

    /// Whether the stop fired: what was found since it was made is incomplete.
    pub fn stopped(&self) -> bool {
        self.stopped.get()
    }

    /// Counts one unit: a position a scan visits, a probe ([`Self::attempt`]).
    #[inline]
    pub fn tick(&self) -> bool {
        self.attempt(1)
    }

    /// Counts `units` of attempted work, whether it delivers anything or not (a search, a
    /// producer, a range of driver positions, a read: [`Source`]); true once the stop has
    /// fired. The poller bounds attempted work, not delivered rows. The common path tests
    /// `stopped` and the ticker's countdown: no call and no atomic.
    ///
    /// `units` is at most `POLL_UNITS`: work counted at once runs to its end before the stop
    /// is asked again, so a longer run would do its length past a stop (REAS-2's bound:
    /// `POLL_UNITS` plus one run). A list of work of any length goes through
    /// [`Self::counted`] (review r1 F1).
    #[inline]
    pub fn attempt(&self, units: usize) -> bool {
        debug_assert!(
            units <= nrese_exec::POLL_UNITS as usize,
            "{units} units counted at once: at most POLL_UNITS (Poller::counted)"
        );
        if self.stopped.get() {
            return true;
        }
        let units = u32::try_from(units).unwrap_or(u32::MAX);
        if self.ticker.tick(units).is_err() {
            self.stopped.set(true);
        }
        self.stopped.get()
    }

    /// The items of `items` as the work they are, each counted before it is handed on: a run
    /// of at most `POLL_UNITS` counted at once (as many as the items say are left, at least
    /// one), the stop asked at each run; no more once the poller has stopped. The way a leaf
    /// reads a list it holds (a slice, a map's entries, a member list): one count per run, a
    /// decrement per item (review r1 F1).
    pub fn counted<I: IntoIterator>(&self, items: I) -> Counted<'_, 'a, I::IntoIter> {
        Counted {
            poll: self,
            items: items.into_iter(),
            ahead: 0,
        }
    }
}

/// [`Poller::counted`]'s items.
pub struct Counted<'v, 'p, I> {
    poll: &'v Poller<'p>,
    items: I,
    /// Items counted ahead and not yet handed on.
    ahead: usize,
}

impl<I: Iterator> Iterator for Counted<'_, '_, I> {
    type Item = I::Item;

    #[inline]
    fn next(&mut self) -> Option<I::Item> {
        if self.ahead == 0 {
            let run = self
                .items
                .size_hint()
                .0
                .clamp(1, nrese_exec::POLL_UNITS as usize);
            if self.poll.attempt(run) {
                return None;
            }
            self.ahead = run;
        }
        let item = self.items.next()?;
        self.ahead -= 1;
        Some(item)
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        (0, self.items.size_hint().1)
    }
}

fn never() -> bool {
    false
}

/// A [`Stop`] that never fires.
pub const NEVER: Stop<'static> = &never;

/// Whether the process holds more than its memory limit
/// ([`nrese_exec::memory::set_process_limit`]), or its memory can't be read:
/// materialisations and commits stop then, whatever `stop` their caller passes
/// ([`OperationStop`]). A collected closure can outgrow any time budget (a DL bound's
/// evaluation reached 49 GB under a deadline, 6 October 2026).
pub fn over_memory_limit() -> bool {
    nrese_exec::memory::process_limit_exceeded()
}

/// What polls the process's memory limit for an operation, and why it stops it
/// ([`nrese_exec::memory::process_limit_stop`]).
pub type MemoryPoll<'a> = &'a (dyn Fn() -> Option<nrese_exec::memory::MemoryStopCause> + Sync);

/// What an operation's own memory limit leaves it now, read when asked: a store's watch
/// ([`nrese_exec::memory::MemoryWatch::room`]), whose limit the process's needn't be.
pub type MemoryRoom<'a> = &'a (dyn Fn() -> nrese_exec::memory::Reading + Sync);

/// What the process's memory limit leaves now, if one is set (the limit less what the
/// process holds, as [`over_memory_limit`] reads it); without one, no bound.
fn process_room() -> nrese_exec::memory::Reading {
    use nrese_exec::memory;
    match memory::process_limit() {
        Some(limit) => memory::process_bytes().map(|held| limit.saturating_sub(held)),
        None => Ok(u64::MAX),
    }
}

/// One operation's stop (a materialisation, a commit's maintenance): its caller's `stop`,
/// or the process's memory limit. Once it fires it stays fired for this operation, so a
/// morsel it made skip is never taken for done by a later poll that finds the memory under
/// the limit again, and it keeps the memory's cause of that moment ([`Self::interrupted`]),
/// whatever another operation's poll reads since. The next operation makes its own and may
/// run. It also says what memory the operation may still take ([`Self::room`]), and ends
/// it for its memory where an allocation of a known size doesn't fit ([`Self::refuse`]).
pub struct OperationStop<'a> {
    caller: Stop<'a>,
    memory: MemoryPoll<'a>,
    /// The operation's own memory limit's room, beside the process's ([`Self::within`]).
    room: Option<MemoryRoom<'a>>,
    /// 0: not fired; else what fired it ([`OperationStop::fired`]).
    fired: std::sync::atomic::AtomicU8,
}

impl<'a> OperationStop<'a> {
    /// Under the process's memory limit.
    pub fn new(caller: Stop<'a>) -> Self {
        Self::with_poll(caller, &nrese_exec::memory::process_limit_stop)
    }

    /// Under the limit `memory` polls (the process's, or a test's).
    pub fn with_poll(caller: Stop<'a>, memory: MemoryPoll<'a>) -> Self {
        Self {
            caller,
            memory,
            room: None,
            fired: std::sync::atomic::AtomicU8::new(0),
        }
    }

    /// Under the operation's own memory limit too, whose room `room` reads: what it admits
    /// is the least of that room and [`Self::room`]'s others.
    pub fn within(self, room: MemoryRoom<'a>) -> Self {
        Self {
            room: Some(room),
            ..self
        }
    }

    /// What the operation may still take now: the least of what its own memory limit
    /// leaves ([`Self::within`]), what the process's leaves, if one is set, and what the
    /// system leaves the process ([`nrese_exec::memory::remaining_bytes`]). Read when
    /// asked, once before allocations whose size is known, which a poll would only see
    /// once they are made. A reading that can't be taken is no room: its cause is
    /// [`MemoryStopCause::Unread`](nrese_exec::memory::MemoryStopCause).
    pub fn room(&self) -> Result<u64, nrese_exec::memory::MemoryStopCause> {
        let own = self.room.map_or(Ok(u64::MAX), |room| room());
        [own, process_room(), nrese_exec::memory::remaining_bytes()]
            .into_iter()
            .try_fold(u64::MAX, |least, room| room.map(|room| least.min(room)))
            .map_err(|_| nrese_exec::memory::MemoryStopCause::Unread)
    }

    /// Ends the operation for its memory (`cause`): it fires from now on, and the
    /// interruption returned, as [`Self::interrupted`] gives it later, says why, unless it
    /// had fired before (the first to fire it names it).
    pub fn refuse(&self, cause: nrese_exec::memory::MemoryStopCause) -> super::delta::Interrupted {
        use nrese_exec::memory::MemoryStopCause;
        let code = match cause {
            MemoryStopCause::OverLimit => Self::OVER_LIMIT,
            MemoryStopCause::Unread => Self::UNREAD,
        };
        let _ = self.fired.compare_exchange(
            0,
            code,
            std::sync::atomic::Ordering::Relaxed,
            std::sync::atomic::Ordering::Relaxed,
        );
        self.interrupted()
    }

    /// Whether the operation is to stop: an atomic load once it has.
    pub fn fired(&self) -> bool {
        use nrese_exec::memory::MemoryStopCause;
        use std::sync::atomic::Ordering::Relaxed;
        if self.fired.load(Relaxed) != 0 {
            return true;
        }
        let code = if (self.caller)() {
            Self::CALLER
        } else {
            match (self.memory)() {
                None => return false,
                Some(MemoryStopCause::OverLimit) => Self::OVER_LIMIT,
                Some(MemoryStopCause::Unread) => Self::UNREAD,
            }
        };
        // The first to fire it names it.
        let _ = self.fired.compare_exchange(0, code, Relaxed, Relaxed);
        true
    }

    /// The interruption it ends the operation with: the memory's cause, if the memory
    /// limit fired it.
    pub fn interrupted(&self) -> super::delta::Interrupted {
        use nrese_exec::memory::MemoryStopCause;
        let memory = match self.fired.load(std::sync::atomic::Ordering::Relaxed) {
            Self::OVER_LIMIT => Some(MemoryStopCause::OverLimit),
            Self::UNREAD => Some(MemoryStopCause::Unread),
            _ => None,
        };
        super::delta::Interrupted { memory }
    }

    const CALLER: u8 = 1;
    const OVER_LIMIT: u8 = 2;
    const UNREAD: u8 = 3;
}

/// The filter of a morsel's derived facts, reading with the morsel's poller.
pub type Keep<'k> = &'k (dyn Fn(Triple, &Poller<'_>) -> bool + Sync);

/// Every fact `jobs` derive that `keep` accepts (its reads count on the morsel's poller).
/// If `stop` fires, the morsels under way end within their poll interval and the rest are
/// skipped: the result is incomplete, and the caller must ask its (latched,
/// [`OperationStop`]) `stop` again and discard it.
pub fn run_jobs<S: Source + ?Sized>(
    source: &S,
    jobs: &[Job<'_>],
    keep: Keep<'_>,
    stop: Stop<'_>,
) -> Vec<Triple> {
    run_jobs_with(source, jobs, keep, false, stop, None)
}

/// How often [`run_jobs_by_morsel`] asked `keep` (the membership probes), how many of
/// those asks came out of (predicate, subject, object) order within their morsel, and
/// the complete bindings each job enumerated (the joins' work, §5.1 of the 6 October
/// investigation; empty if not asked for, see [`Probes::for_jobs`]).
#[derive(Debug, Default)]
pub struct Probes {
    pub probes: std::sync::atomic::AtomicU64,
    pub unordered: std::sync::atomic::AtomicU64,
    /// Head facts the bindings produced before each morsel's deduplication (one per
    /// binding and head): with the new facts, the bindings per derived fact.
    pub emitted: std::sync::atomic::AtomicU64,
    pub bindings: Vec<std::sync::atomic::AtomicU64>,
}

impl Probes {
    /// Counts with a binding counter for each of `jobs` jobs.
    pub fn for_jobs(jobs: usize) -> Self {
        Self {
            bindings: (0..jobs).map(|_| Default::default()).collect(),
            ..Self::default()
        }
    }
}

/// [`run_jobs`] by morsel: each morsel's facts, sorted by (predicate, subject, object) and
/// distinct, are handed to `finish` with the index of the job they come from, and its results come back in the morsels' order,
/// not concatenated (a caller that regroups them needs no copy of all of them). `keep`
/// removes the facts the source already holds from a morsel's facts in that order, all at
/// once (a store reads its runs forward across them), reading with the morsel's poller.
/// Counts the probes into `probes` (once per morsel). `rewrite`, if given, maps each
/// derived fact before it is checked (equality by representatives: to its
/// representatives).
pub fn run_jobs_by_morsel<S: Source + ?Sized, T: Send>(
    source: &S,
    jobs: &[Job<'_>],
    keep: &(dyn Fn(&mut Vec<Triple>, &Poller<'_>) + Sync),
    rewrite: Option<&(dyn Fn(Triple) -> Triple + Sync)>,
    stop: Stop<'_>,
    probes: &Probes,
    finish: &(dyn Fn(usize, Vec<Triple>) -> T + Sync),
) -> Vec<T> {
    run_morsels(
        source,
        jobs,
        keep,
        false,
        rewrite,
        stop,
        Some(probes),
        finish,
    )
}

/// [`run_jobs`] without circular derivations: those whose head is one of their own
/// premises under the binding, such as `(?x p ?x) -> (?x p ?x)` from a symmetric `p`, or
/// transitivity with `?y = ?z`. A well-founded proof never uses such a step, so
/// overdeletion can skip them; it would otherwise cascade through them. Instances that
/// are circular under every binding (`(?x type C) -> (?x type C)` from `C subClassOf C`)
/// aren't in a [`GroundProgram`] at all.
pub fn run_jobs_acyclic<S: Source + ?Sized>(
    source: &S,
    jobs: &[Job<'_>],
    keep: Keep<'_>,
    stop: Stop<'_>,
) -> Vec<Triple> {
    run_jobs_with(source, jobs, keep, true, stop, None)
}

fn run_jobs_with<S: Source + ?Sized>(
    source: &S,
    jobs: &[Job<'_>],
    keep: Keep<'_>,
    acyclic: bool,
    stop: Stop<'_>,
    probes: Option<&Probes>,
) -> Vec<Triple> {
    run_morsels(
        source,
        jobs,
        &|facts: &mut Vec<Triple>, poll: &Poller<'_>| facts.retain(|&fact| keep(fact, poll)),
        acyclic,
        None,
        stop,
        probes,
        &|_, facts| facts,
    )
    .concat()
}

#[allow(clippy::too_many_arguments)]
fn run_morsels<S: Source + ?Sized, T: Send>(
    source: &S,
    jobs: &[Job<'_>],
    keep: &(dyn Fn(&mut Vec<Triple>, &Poller<'_>) + Sync),
    acyclic: bool,
    rewrite: Option<&(dyn Fn(Triple) -> Triple + Sync)>,
    stop: Stop<'_>,
    probes: Option<&Probes>,
    finish: &(dyn Fn(usize, Vec<Triple>) -> T + Sync),
) -> Vec<T> {
    let tasks: Vec<(usize, std::ops::Range<usize>)> = jobs
        .iter()
        .enumerate()
        .flat_map(|(j, job)| {
            (0..job.drivers())
                .step_by(MORSEL)
                .map(move |start| (j, start..(start + MORSEL).min(job.drivers())))
        })
        .collect();
    tasks
        .par_iter()
        .map(|(j, range)| {
            let job = &jobs[*j];
            let Head::Facts(heads) = &job.rule.head else {
                return finish(*j, Vec::new());
            };
            let Some(poll) = Poller::new(stop) else {
                return finish(*j, Vec::new());
            };
            let mut out = Vec::new();
            let mut enumerated = 0u64;
            job.run(source, range.clone(), &poll, &mut |bindings| {
                enumerated += 1;
                for head in heads {
                    let fact = instantiate_head(head, bindings);
                    let circular = acyclic
                        && job
                            .rule
                            .body
                            .iter()
                            .any(|atom| instantiate_head(atom, bindings) == fact);
                    if !circular {
                        out.push(rewrite.map_or(fact, |rewrite| rewrite(fact)));
                    }
                }
            });
            // Stopped part-way: nothing of it is kept or probed. The stop is the operation's
            // and stays fired, so its caller discards the round when it asks again.
            if poll.stopped() {
                return finish(*j, Vec::new());
            }
            // The morsel's duplicates go before `keep` probes the source, and the probes
            // run in (predicate, subject, object) order: about half of an OWL 2 RL round's
            // candidates repeat within it, and sorted probes let a store read a relation's
            // sorted runs forward from the last probe's place instead of searching each
            // from the root, missing the cache at every level.
            let emitted = out.len() as u64;
            out.sort_unstable_by_key(|&[s, p, o]| (p, s, o));
            out.dedup();
            let asked = out.len() as u64;
            let unordered = out
                .windows(2)
                .filter(|w| (w[0][1], w[0][0], w[0][2]) > (w[1][1], w[1][0], w[1][2]))
                .count() as u64;
            keep(&mut out, &poll);
            // `keep`'s reads count on the morsel's poller: stopped in them, its answers mean
            // nothing, and nothing of the morsel is kept.
            if poll.stopped() {
                return finish(*j, Vec::new());
            }
            // What `keep` removed goes back now, not when every morsel is done: a round's
            // morsels are all held until their results are collected. A commit whose
            // round derived nothing new held 30,876 emptied buffers of 400 heads each,
            // about 360 MiB, beside its jobs (DMW's subclass chain, fleet pm-dmwab-3,
            // 21 Oct 2026).
            out.shrink_to_fit();
            if let Some(probes) = probes {
                use std::sync::atomic::Ordering::Relaxed;
                probes.probes.fetch_add(asked, Relaxed);
                probes.unordered.fetch_add(unordered, Relaxed);
                if let Some(count) = probes.bindings.get(*j) {
                    count.fetch_add(enumerated, Relaxed);
                }
                probes.emitted.fetch_add(emitted, Relaxed);
            }
            finish(*j, out)
        })
        .collect()
}

/// Calls `emit` with every binding of `rule`'s body under which its head derives `fact`,
/// reading `seg` (a backward, one-step derivation check: DRed's rederivation, and
/// explanations later). It polls `poll` as [`walk_until`] does: once that has stopped, the
/// bindings emitted so far are not all there are.
pub fn derivations<S: Source + ?Sized>(
    source: &S,
    rule: &Rule,
    fact: Triple,
    seg: Seg,
    poll: &Poller<'_>,
    emit: &mut dyn FnMut(&[Option<u64>]) -> bool,
) {
    let Head::Facts(heads) = &rule.head else {
        return;
    };
    // The search is a unit of attempted work, whether it finds anything or not; on a poller
    // that has stopped (one shared by several searches) it opens no scan at all, not even
    // the first (sol-loop r4, R4-1; review r1, F1).
    if poll.attempt(1) {
        return;
    }
    let mut bindings = vec![None; rule.variables()];
    for head in heads {
        let Some(newly) = bind(head, fact, &mut bindings) else {
            continue;
        };
        let stop = if rule.body.is_empty() {
            guards_hold(&rule.guards, &bindings) && emit(&bindings)
        } else {
            let bound: Vec<bool> = bindings.iter().map(Option::is_some).collect();
            let atoms: Vec<usize> = (0..rule.body.len()).collect();
            // Start from the atom with the most bound positions, then the fewest matches.
            let first = (0..rule.body.len())
                .max_by_key(|&i| {
                    let bound = pattern(&rule.body[i], &bindings);
                    let known = bound.iter().filter(|p| p.is_some()).count();
                    (known, std::cmp::Reverse(source.estimate(bound, seg, poll)))
                })
                .expect("a body");
            let order = plan_bound(source, rule, &atoms, first, &bound, |_| seg, poll);
            walk_until(
                source,
                &rule.body,
                &rule.guards,
                &order,
                0,
                &mut bindings,
                poll,
                emit,
            )
        };
        unbind(head, newly, &mut bindings);
        if stop || poll.stopped() {
            return;
        }
    }
}

/// The predicate `p` of a transitivity rule `(?x p ?y), (?y p ?z) -> (?x p ?z)`: `prp-trp`
/// after grounding, `scm-sco` and `scm-spo` before.
pub fn transitive_predicate(rule: &Rule) -> Option<u64> {
    let ([a, b], Head::Facts(head)) = (rule.body.as_slice(), &rule.head) else {
        return None;
    };
    let [Atom([hx, Term::Const(p), hz])] = head.as_slice() else {
        return None;
    };
    if !rule.guards.is_empty() {
        return None;
    }
    for (first, second) in [(a, b), (b, a)] {
        let (Atom([x, Term::Const(p1), y]), Atom([y2, Term::Const(p2), z])) = (first, second)
        else {
            continue;
        };
        let distinct = matches!((x, y, z), (Term::Var(x), Term::Var(y), Term::Var(z)) if x != y && y != z && x != z);
        if distinct && p1 == p && p2 == p && y == y2 && x == hx && z == hz {
            return Some(*p);
        }
    }
    None
}

/// `rule` without the head atoms that are one of its body atoms; `None` if no head is
/// left. Whenever the body holds, such a head's fact is already a premise: the tautology
/// `A → A` that reflexive schema facts ground into (`C subClassOf C` gives
/// `(?x type C) → (?x type C)` under `cax-sco`, `p subPropertyOf p` gives
/// `(?x p ?y) → (?x p ?y)` under `prp-spo1`). Dropping it changes no closure and no
/// well-founded proof, and saves re-deriving every type and property fact once more.
fn without_tautologies(mut rule: Rule) -> Option<Rule> {
    if let Head::Facts(heads) = &mut rule.head {
        heads.retain(|head| !rule.body.contains(head));
        if heads.is_empty() {
            return None;
        }
    }
    Some(rule)
}

/// The schema link of `rule` if it is a closed family: one schema atom `(?a S ?b)` over
/// two variables, one instance atom `A` mentioning `?a` and not `?b`, no guard, and one
/// head atom, `A` with `?a` replaced by `?b`. `cax-sco` (`S` = `rdfs:subClassOf`:
/// `(?x type ?a) -> (?x type ?b)`) and `prp-spo1` (`rdfs:subPropertyOf`:
/// `(?x ?a ?y) -> (?x ?b ?y)`) are such families.
///
/// With `S` transitive (its closure complete when the program has grounded the rule
/// over it), an instance's output needs no further work from the family: a fact
/// `A[b]` the instance for `a S b` derived from `A[a]` has, for every `b S c`, the
/// instance for `a S c` derive `A[c]` from the same `A[a]` (grounded now, or later and
/// then evaluated over all facts). So the family's instances needn't read what it
/// produced: the investigation's idempotent propagators, which the batch executor uses
/// to keep a deep hierarchy from re-deriving every inherited type once per ancestor.
pub fn closed_family(schema: &Schema, rule: &Rule) -> Option<u64> {
    let ([first, second], Head::Facts(heads)) = (rule.body.as_slice(), &rule.head) else {
        return None;
    };
    let [head] = heads.as_slice() else {
        return None;
    };
    if !rule.guards.is_empty() {
        return None;
    }
    let (link, instance) = match (schema.is_schema_atom(first), schema.is_schema_atom(second)) {
        (true, false) => (first, second),
        (false, true) => (second, first),
        _ => return None,
    };
    let Atom([Term::Var(a), Term::Const(s), Term::Var(b)]) = *link else {
        return None;
    };
    if a == b || !instance.0.contains(&Term::Var(a)) || instance.0.contains(&Term::Var(b)) {
        return None;
    }
    let replaced = instance.0.map(|t| match t {
        Term::Var(v) if v == a => Term::Var(b),
        t => t,
    });
    (head.0 == replaced).then_some(s)
}

/// Identity of a ground rule, for deduplication (the name doesn't matter).
pub type RuleKey = (Vec<Atom>, Vec<Guard>, Head);

pub fn rule_key(rule: &Rule) -> RuleKey {
    (rule.body.clone(), rule.guards.clone(), rule.head.clone())
}

/// A (rule, atom) reference into a [`GroundProgram`].
type AtomRef = (u32, u8);

/// Test builds only: the dispatch references read on this thread, for tests that count
/// enumeration work no source sees (sol-loop r8, R8-1).
#[cfg(test)]
pub(crate) mod references_read {
    use std::cell::Cell;

    thread_local! {
        static READ: Cell<usize> = const { Cell::new(0) };
    }

    pub(crate) fn add(n: usize) {
        READ.with(|read| read.set(read.get() + n));
    }

    /// The references read since the last call.
    pub(crate) fn take() -> usize {
        READ.with(|read| read.replace(0))
    }
}

/// Ground rules' atoms indexed by what they can match, so a round only visits the rules
/// with an atom that can match a fact of the delta (the "dispatch tables" of design §3.2):
/// `(?x type C)` atoms by `(type, C)`, other constant-predicate atoms by predicate.
#[derive(Default, Clone)]
pub struct Dispatch {
    /// Constant predicate and object, by predicate then object.
    by_object: hashbrown::HashMap<u64, hashbrown::HashMap<u64, Vec<AtomRef>>>,
    /// Constant predicate, variable object.
    by_predicate: hashbrown::HashMap<u64, Vec<AtomRef>>,
    /// Variable predicate.
    any: Vec<AtomRef>,
}

impl Dispatch {
    fn push(&mut self, index: usize, rule: &Rule) {
        self.push_atoms(index, &rule.body);
    }

    fn push_atoms(&mut self, index: usize, atoms: &[Atom]) {
        let index = u32::try_from(index).expect("fewer than 2^32 ground rules");
        for (i, atom) in atoms.iter().enumerate() {
            let entry = (index, u8::try_from(i).expect("fewer than 256 atoms"));
            match atom.0 {
                [_, Term::Const(p), Term::Const(o)] => {
                    self.by_object
                        .entry(p)
                        .or_default()
                        .entry(o)
                        .or_default()
                        .push(entry);
                }
                [_, Term::Const(p), _] => self.by_predicate.entry(p).or_default().push(entry),
                _ => self.any.push(entry),
            }
        }
    }

    /// The rule of each (rule, atom) reference whose atom can match `fact`, read lazily in
    /// ascending rule order, a rule once per reference (so repeated when several of its atoms
    /// match). Each of the three lists is in ascending rule order, as rules and their
    /// references are only appended ([`GroundProgram::add`]); this merges them as they are
    /// read, with no copy and no sort. The one enumeration of a fact's producers
    /// ([`GroundProgram::for_each_producer`] counts each reference it reads).
    fn rules_matching(&self, [_, p, o]: Triple) -> impl Iterator<Item = usize> + '_ {
        let by_object = self
            .by_object
            .get(&p)
            .and_then(|objects| objects.get(&o))
            .map_or(&[][..], Vec::as_slice);
        let by_predicate = self.by_predicate.get(&p).map_or(&[][..], Vec::as_slice);
        let mut lists = [by_object, by_predicate, self.any.as_slice()];
        std::iter::from_fn(move || {
            let (k, _) = lists
                .iter()
                .enumerate()
                .filter_map(|(k, list)| list.first().map(|&(r, _)| (k, r)))
                .min_by_key(|&(_, r)| r)?;
            let (&(r, _), rest) = lists[k].split_first()?;
            lists[k] = rest;
            #[cfg(test)]
            references_read::add(1);
            Some(r as usize)
        })
    }

    /// The (rule, atom) pairs whose atom can match `fact`.
    fn matching_fact(&self, [_, p, o]: Triple, out: &mut Vec<(usize, usize)>) {
        let entries = self
            .by_object
            .get(&p)
            .and_then(|objects| objects.get(&o))
            .into_iter()
            .flatten()
            .chain(self.by_predicate.get(&p).into_iter().flatten())
            .chain(&self.any);
        let before = out.len();
        out.extend(entries.map(|&(r, a)| (r as usize, usize::from(a))));
        #[cfg(test)]
        references_read::add(out.len() - before);
        #[cfg(not(test))]
        let _ = before;
    }

    /// The (rule, atom) pairs whose atom can match a fact of `delta`'s delta, sorted. Each
    /// relation it opens, reference it copies and probe it makes of the delta counts on
    /// `poll` (sweep M04): stopped, the pairs are incomplete and mean nothing.
    pub(crate) fn matching(
        &self,
        delta: &super::batch::Store,
        poll: &Poller<'_>,
    ) -> Vec<(usize, usize)> {
        let mut out: Vec<AtomRef> = Vec::new();
        let mut any_delta = false;
        for (p, relation) in delta.delta_relations() {
            if poll.tick() {
                return Vec::new();
            }
            any_delta = true;
            // Each reference copied as it is counted; a stop ends the matching, whatever was
            // copied before it (review r1 F1).
            if let Some(entries) = self.by_predicate.get(&p) {
                out.extend(poll.counted(entries).copied());
            }
            if poll.stopped() {
                return Vec::new();
            }
            let Some(objects) = self.by_object.get(&p) else {
                continue;
            };
            // Probe whichever side is smaller: the indexed objects or the delta's.
            if objects.len() <= relation.delta_len() {
                for (&o, entries) in objects {
                    if relation.delta_has_object(o, poll) {
                        out.extend(poll.counted(entries).copied());
                    }
                    if poll.stopped() {
                        return Vec::new();
                    }
                }
            } else {
                relation.delta_objects(poll, &mut |o| {
                    if let Some(entries) = objects.get(&o) {
                        out.extend(poll.counted(entries).copied());
                    }
                });
                if poll.stopped() {
                    return Vec::new();
                }
            }
        }
        if any_delta {
            out.extend(poll.counted(&self.any).copied());
        }
        if poll.stopped() {
            return Vec::new();
        }
        out.sort_unstable();
        out.dedup();
        out.into_iter()
            .map(|(r, a)| (r as usize, usize::from(a)))
            .collect()
    }
}

/// A ground program's enumerations without a poller ([`GroundProgram::unpolled`]): for work
/// that runs to its end, explanations and tests. During an operation that has a stop they
/// are read through [`GroundProgram::for_each_alternative`] and
/// [`GroundProgram::for_each_producer`], which count each one.
pub struct Unpolled<'a>(&'a GroundProgram);

impl<'a> Unpolled<'a> {
    /// Every set of schema facts `grounding` was grounded on (the first, then the others).
    pub fn premise_alternatives(&self, grounding: Grounding) -> Vec<&'a [Triple]> {
        self.0.alternatives(grounding).collect()
    }

    /// The rules (indexes into `rules`) whose head can give `fact`, each once.
    pub fn producers(&self, fact: Triple) -> Vec<usize> {
        self.0.producer_list(fact)
    }

    /// [`GroundProgram::named_derivations`] run to its end (`limit` bounds the bodies).
    pub fn named_derivations<S: Source + ?Sized>(
        &self,
        source: &S,
        fact: Triple,
        limit: usize,
    ) -> Vec<(String, Vec<Triple>)> {
        self.0
            .named_derivations(source, fact, limit, &Poller::never())
    }

    /// [`Self::named_derivations`] with every set of schema facts each ground rule (and
    /// transitivity) was grounded on, not only the first: the derivations a justification
    /// enumeration needs (a bodiless instance's are [`Self::premise_alternatives`]). At most
    /// `limit`.
    pub fn every_named_derivation<S: Source + ?Sized>(
        &self,
        source: &S,
        fact: Triple,
        limit: usize,
    ) -> Vec<(String, Vec<Triple>)> {
        let program = self.0;
        let transitive = program
            .transitive
            .contains(&fact[1])
            .then(|| transitivity(fact[1]));
        let rules = self
            .producers(fact)
            .into_iter()
            .map(|r| (&program.rules[r], Grounding::Rule(r)))
            .chain(
                transitive
                    .as_ref()
                    .map(|rule| (rule, Grounding::Transitive(fact[1]))),
            );
        let mut bodies = Vec::new();
        let poll = Poller::never();
        for (rule, grounding) in rules {
            let alternatives = self.premise_alternatives(grounding);
            derivations(source, rule, fact, Seg::All, &poll, &mut |bindings| {
                let body: Vec<Triple> = rule
                    .body
                    .iter()
                    .map(|atom| instantiate_head(atom, bindings))
                    .collect();
                for premises in &alternatives {
                    let mut full = body.clone();
                    full.extend_from_slice(premises);
                    bodies.push((rule.name.clone(), full));
                }
                bodies.len() >= limit
            });
            if bodies.len() >= limit {
                break;
            }
        }
        bodies
    }

    /// Whether `fact` has a one-step derivation from the facts of `source` (`Seg::All`):
    /// a bodiless instance, a rule whose head matches it, or transitivity.
    pub fn derivable<S: Source + ?Sized>(&self, source: &S, fact: Triple) -> bool {
        self.0.derivable_with(source, fact, true, NEVER)
    }
}

/// A ground program: the rule instances with a body (indexed by [`Dispatch`]), the
/// transitive predicates (closed by a module instead of joins), the facts of bodiless
/// instances, and the ground consistency rules.
///
/// It offers no enumeration of premise alternatives or producers that no poller counts:
/// during an operation that has a stop, each one read is attempted work (REAS review r1,
/// sol-loop r6/r7). Work that runs to its end reads them through `unpolled()`.
///
/// ```compile_fail
/// fn uncounted(program: &nrese_reasoner::eval::GroundProgram, fact: [u64; 3]) -> usize {
///     program.producers(fact).len()
///         + program
///             .premise_alternatives(nrese_reasoner::eval::Grounding::Bodiless(fact))
///             .len()
/// }
/// ```
#[derive(Default, Clone)]
pub struct GroundProgram {
    pub rules: Vec<Rule>,
    /// Per rule: the closed family it is an instance of, if any ([`closed_family`]).
    families: Vec<Option<u64>>,
    /// The relations closed families read and produce by name (`cax-sco`'s `rdf:type`),
    /// each with its family: the family claims the relation's closed part.
    pub family_relations: std::collections::BTreeMap<u64, u64>,
    known: hashbrown::HashMap<RuleKey, usize>,
    dispatch: Dispatch,
    pub transitive: std::collections::BTreeSet<u64>,
    /// Facts of bodiless instances, not yet handed out ([`Self::take_facts`]).
    facts: Vec<Triple>,
    /// Facts of all bodiless instances.
    pub bodiless: HashSet<Triple>,
    /// The schema facts each rule instance was grounded on (parallel to `rules`), each
    /// bodiless fact's, and each transitive predicate's: a backward proof that uses an
    /// instance must prove them too.
    premises: Vec<Vec<Triple>>,
    bodiless_premises: hashbrown::HashMap<Triple, Vec<Triple>>,
    transitive_premises: hashbrown::HashMap<u64, Vec<Triple>>,
    /// Further sets of schema facts the same ground rule, bodiless fact or transitive
    /// predicate was grounded on (a proof needs one; support graph sets need all).
    other_premises: hashbrown::HashMap<Grounding, Vec<Vec<Triple>>>,
    /// Rules by head atom, for backward derivation checks.
    heads: Dispatch,
    /// Ground consistency rules, each with the rule it came from.
    pub consistency: Vec<(Rule, Grounded)>,
    consistency_known: HashSet<(String, Vec<Option<u64>>, RuleKey)>,
    consistency_dispatch: Dispatch,
    /// The list axioms of the program's state that weren't instantiated.
    pub list_diagnostics: Vec<super::lists::ListDiagnostic>,
}

impl std::fmt::Debug for GroundProgram {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GroundProgram")
            .field("rules", &self.rules.len())
            .field("transitive", &self.transitive.len())
            .field("bodiless", &self.bodiless.len())
            .field("consistency", &self.consistency.len())
            .finish()
    }
}

impl GroundProgram {
    /// Files one ground fact rule; returns its index if it is new and has a body.
    pub fn add(&mut self, rule: Rule) -> Option<usize> {
        self.add_with(rule, Vec::new(), None)
    }

    /// [`Self::add`], with the schema facts the instance was grounded on, and the closed
    /// family it is an instance of, if any ([`closed_family`]).
    fn add_with(
        &mut self,
        rule: Rule,
        premises: Vec<Triple>,
        family: Option<u64>,
    ) -> Option<usize> {
        if let Some(p) = transitive_predicate(&rule) {
            self.transitive_with(p, premises);
            None
        } else if rule.body.is_empty() {
            if let Head::Facts(heads) = &rule.head {
                for head in heads {
                    let fact = instantiate_head(head, &[]);
                    if self.bodiless.insert(fact) {
                        self.facts.push(fact);
                        self.bodiless_premises.insert(fact, premises.clone());
                    } else if self.bodiless_premises.get(&fact) != Some(&premises) {
                        self.other(Grounding::Bodiless(fact), premises.clone());
                    }
                }
            }
            None
        } else {
            let rule = without_tautologies(rule)?;
            let key = rule_key(&rule);
            if let Some(&r) = self.known.get(&key) {
                if self.premises[r] != premises {
                    self.other(Grounding::Rule(r), premises);
                }
                return None;
            }
            self.known.insert(key, self.rules.len());
            self.dispatch.push(self.rules.len(), &rule);
            if let Head::Facts(heads) = &rule.head {
                self.heads.push_atoms(self.rules.len(), heads);
            }
            self.rules.push(rule);
            self.premises.push(premises);
            self.families.push(family);
            Some(self.rules.len() - 1)
        }
    }

    /// Makes `p` transitive, grounded on `premises`.
    fn transitive_with(&mut self, p: u64, premises: Vec<Triple>) {
        self.transitive.insert(p);
        match self.transitive_premises.get(&p) {
            None => {
                self.transitive_premises.insert(p, premises);
            }
            Some(first) if *first != premises => {
                self.other(Grounding::Transitive(p), premises);
            }
            Some(_) => {}
        }
    }

    /// Records a further set of schema facts for `grounding` (once each).
    fn other(&mut self, grounding: Grounding, premises: Vec<Triple>) {
        let others = self.other_premises.entry(grounding).or_default();
        if !others.contains(&premises) {
            others.push(premises);
        }
    }

    /// Visits every set of schema facts `grounding` was grounded on (the first, then the
    /// others), each a unit of attempted work on `poll`, counted before `body` reads it; `body`
    /// gets the poller back for its own work. `Some` with what `body` broke with; `None` once
    /// every alternative was visited, or `poll` stopped ([`Poller::stopped`] says which).
    ///
    /// The one way to enumerate them during an operation that has a stop: a read that no
    /// poller counts can't be written (REAS review r1, sol-loop r6/r7). Explanations, which
    /// run to their end, read them through [`Self::unpolled`].
    pub fn for_each_alternative<'p, B>(
        &self,
        grounding: Grounding,
        poll: &Poller<'p>,
        mut body: impl FnMut(&[Triple], &Poller<'p>) -> ControlFlow<B>,
    ) -> Option<B> {
        for premises in self.alternatives(grounding) {
            if poll.attempt(1) {
                return None;
            }
            if let ControlFlow::Break(out) = body(premises, poll) {
                return Some(out);
            }
        }
        None
    }

    /// Visits each rule (by index, ascending) whose head can give `fact`, once each, as
    /// [`Self::for_each_alternative`] visits alternatives. Discovery is attempted work as it
    /// is read: each head reference, a duplicate of the previous rule's included, is a unit
    /// counted before it is used, and a `Break` leaves the rest unread (fix check r2, R8-1).
    pub fn for_each_producer<'p, B>(
        &self,
        fact: Triple,
        poll: &Poller<'p>,
        mut body: impl FnMut(usize, &Poller<'p>) -> ControlFlow<B>,
    ) -> Option<B> {
        let mut last = None;
        for r in self.heads.rules_matching(fact) {
            if poll.attempt(1) {
                return None;
            }
            if last == Some(r) {
                continue;
            }
            debug_assert!(
                last.is_none_or(|l| l < r),
                "producers in ascending rule order"
            );
            last = Some(r);
            if let ControlFlow::Break(out) = body(r, poll) {
                return Some(out);
            }
        }
        None
    }

    /// Reads that no stop bounds, for work that runs to its end (explanations) and tests.
    pub fn unpolled(&self) -> Unpolled<'_> {
        Unpolled(self)
    }

    /// The alternatives, enumerated as they are read.
    fn alternatives(&self, grounding: Grounding) -> impl Iterator<Item = &[Triple]> + '_ {
        let first: &[Triple] = match grounding {
            Grounding::Rule(r) => &self.premises[r],
            Grounding::Bodiless(fact) => self.bodiless_premises(fact),
            Grounding::Transitive(p) => self.transitive_premises(p),
        };
        std::iter::once(first).chain(
            self.other_premises
                .get(&grounding)
                .into_iter()
                .flatten()
                .map(Vec::as_slice),
        )
    }

    /// The schema facts that made `p` transitive (empty if it isn't).
    pub fn transitive_premises(&self, p: u64) -> &[Triple] {
        self.transitive_premises.get(&p).map_or(&[], Vec::as_slice)
    }

    /// The schema facts a bodiless fact was grounded on (empty if none, or not bodiless).
    pub fn bodiless_premises(&self, fact: Triple) -> &[Triple] {
        self.bodiless_premises.get(&fact).map_or(&[], Vec::as_slice)
    }

    /// Files one ground consistency rule (from `source`); returns its index if new.
    fn add_consistency(&mut self, source: &Rule, grounded: Grounded) -> Option<usize> {
        let key = (
            source.name.clone(),
            grounded.substitution.clone(),
            rule_key(&grounded.rule),
        );
        if !self.consistency_known.insert(key) {
            return None;
        }
        self.consistency_dispatch
            .push(self.consistency.len(), &grounded.rule);
        self.consistency.push((source.clone(), grounded));
        Some(self.consistency.len() - 1)
    }

    /// Grounds `rules` (fact and consistency rules) over all of `source`.
    pub fn ground<S: Source + ?Sized>(&mut self, source: &S, schema: &Schema, rules: &[Rule]) {
        for rule in rules {
            self.ground_one(source, schema, rule, false, &[]);
        }
    }

    /// [`Self::ground`] for rules that come with premises of their own (list rules and the
    /// list facts they were instantiated from).
    pub fn ground_with_premises<S: Source + ?Sized>(
        &mut self,
        source: &S,
        schema: &Schema,
        rules: &[Rule],
        premises: &[Vec<Triple>],
    ) {
        for (rule, extra) in rules.iter().zip(premises) {
            self.ground_one(source, schema, rule, false, extra);
        }
    }

    /// Grounds `rules` through the delta of `source` only (rules without schema atoms, list
    /// rules among them, in full); returns the indices of the new fact rules and
    /// consistency rules.
    pub fn ground_delta<S: Source + ?Sized>(
        &mut self,
        source: &S,
        schema: &Schema,
        rules: &[Rule],
    ) -> (Vec<usize>, Vec<usize>) {
        let (mut facts, mut checks) = (Vec::new(), Vec::new());
        for rule in rules {
            let (f, c) = self.ground_one(source, schema, rule, true, &[]);
            facts.extend(f);
            checks.extend(c);
        }
        (facts, checks)
    }

    fn ground_one<S: Source + ?Sized>(
        &mut self,
        source: &S,
        schema: &Schema,
        rule: &Rule,
        delta: bool,
        extra: &[Triple],
    ) -> (Vec<usize>, Vec<usize>) {
        let (mut facts, mut checks) = (Vec::new(), Vec::new());
        // Source-level transitivity rules (`scm-sco`, `scm-spo`) don't depend on the
        // delta: full grounding registers them, delta grounding leaves them alone.
        if let Some(p) = transitive_predicate(rule) {
            // A property chain `p ∘ p ⊑ p` from a list axiom that holds only now (its
            // facts derived, or rewritten by equality, during a materialisation) is new
            // in delta grounding too; the vocabulary's (`scm-sco`, over schema atoms)
            // were registered by full grounding.
            let new_chain = !self.transitive.contains(&p)
                && !rule.body.iter().any(|a| schema.is_schema_atom(a));
            if !delta || new_chain {
                // With the facts it came from (a property chain `p ∘ p ⊑ p` from a list
                // axiom); vocabulary rules (`scm-sco`, `eq-trans`) come with none.
                self.transitive_with(p, extra.to_vec());
            }
            return (facts, checks);
        }
        let mut grounded = Vec::new();
        let has_schema_atoms = rule.body.iter().any(|a| schema.is_schema_atom(a));
        // Hidden unnamed classes rewrite heads (W7): the family's shape no longer holds.
        let family = closed_family(schema, rule).filter(|_| schema.hidden().is_empty());
        if let Some(family) = family
            && let Some(Term::Const(p)) = rule
                .body
                .iter()
                .find(|a| !schema.is_schema_atom(a))
                .map(|a| a.0[1])
        {
            self.family_relations.entry(p).or_insert(family);
        }
        if delta && has_schema_atoms {
            ground_delta(source, schema, rule, &mut |g| grounded.push(g));
        } else {
            ground(source, schema, rule, &mut |g| grounded.push(g));
        }
        for g in grounded {
            if rule.head == Head::Inconsistent {
                checks.extend(self.add_consistency(rule, g));
            } else {
                let premises = rule
                    .body
                    .iter()
                    .filter(|a| schema.is_schema_atom(a))
                    .map(|a| instantiate_head(a, &g.substitution))
                    .chain(extra.iter().copied())
                    .collect();
                if let Some(rule) = schema.rewrite_heads(g.rule) {
                    facts.extend(self.add_with(rule, premises, family));
                }
            }
        }
        (facts, checks)
    }

    /// Whether bodiless instances filed facts since the last [`Self::take_facts`].
    pub fn has_pending_facts(&self) -> bool {
        !self.facts.is_empty()
    }

    /// The facts of bodiless instances filed since the last call.
    pub fn take_facts(&mut self) -> Vec<Triple> {
        std::mem::take(&mut self.facts)
    }

    /// The (rule, atom) variants that can match the delta of `delta`, read on `poll`
    /// ([`Dispatch::matching`]).
    pub(crate) fn variants(
        &self,
        delta: &super::batch::Store,
        poll: &Poller<'_>,
    ) -> Vec<(usize, usize)> {
        self.dispatch.matching(delta, poll)
    }

    /// The (consistency rule, atom) variants that can match the delta of `delta`, read on
    /// `poll`.
    pub(crate) fn consistency_variants(
        &self,
        delta: &super::batch::Store,
        poll: &Poller<'_>,
    ) -> Vec<(usize, usize)> {
        self.consistency_dispatch.matching(delta, poll)
    }

    /// Every fact rule, with the transitive predicates as transitivity rules.
    pub fn with_transitivity(&self) -> Vec<Rule> {
        let mut rules = self.rules.clone();
        rules.extend(self.transitive.iter().map(|&p| transitivity(p)));
        rules
    }

    /// The facts that one-atom rule instances derive from `fact` (symmetry, inverses,
    /// subproperties, class hierarchy steps, ...).
    pub fn one_step_images(&self, fact: Triple) -> Vec<Triple> {
        let mut refs = Vec::new();
        self.dispatch.matching_fact(fact, &mut refs);
        let mut out = Vec::new();
        for (r, _) in refs {
            let rule = &self.rules[r];
            let (1, Head::Facts(heads)) = (rule.body.len(), &rule.head) else {
                continue;
            };
            let mut bindings = vec![None; rule.variables()];
            if bind(&rule.body[0], fact, &mut bindings).is_some()
                && guards_hold(&rule.guards, &bindings)
            {
                out.extend(heads.iter().map(|h| instantiate_head(h, &bindings)));
            }
        }
        out
    }

    /// The bodies of one-step derivations of `fact` over `source`: a bodiless instance's
    /// schema facts first, then up to `limit` by rules whose head matches it and
    /// transitivity (instantiated premises, plus the schema facts the instance was grounded
    /// on). Each alternative, producer and search is attempted work on `poll` (DRed's
    /// proofs run under the update's stop); stopped, the bodies found are incomplete.
    pub fn derivation_bodies<S: Source + ?Sized>(
        &self,
        source: &S,
        fact: Triple,
        limit: usize,
        poll: &Poller<'_>,
    ) -> Vec<Vec<Triple>> {
        let mut bodies: Vec<Vec<Triple>> = Vec::new();
        if self.bodiless.contains(&fact) {
            self.for_each_alternative(Grounding::Bodiless(fact), poll, |premises, _| {
                bodies.push(premises.to_vec());
                ControlFlow::<()>::Continue(())
            });
        }
        let named = self.named_derivations(source, fact, limit, poll);
        bodies.extend(named.into_iter().map(|(_, body)| body));
        bodies
    }

    /// [`Self::derivation_bodies`] by rules, each with the name of the rule it instantiates
    /// (`prp-trp` for transitivity): what explanations show. Counted on `poll` as
    /// [`Self::derivation_bodies`] is; explanations read it through [`Self::unpolled`].
    pub fn named_derivations<S: Source + ?Sized>(
        &self,
        source: &S,
        fact: Triple,
        limit: usize,
        poll: &Poller<'_>,
    ) -> Vec<(String, Vec<Triple>)> {
        let mut bodies = Vec::new();
        let mut search = |rule: &Rule, premises: &[Triple], poll: &Poller<'_>| {
            derivations(source, rule, fact, Seg::All, poll, &mut |bindings| {
                let mut body: Vec<Triple> = rule
                    .body
                    .iter()
                    .map(|atom| instantiate_head(atom, bindings))
                    .collect();
                body.extend_from_slice(premises);
                bodies.push((rule.name.clone(), body));
                bodies.len() >= limit
            });
            bodies.len() >= limit
        };
        let full = self.for_each_producer(fact, poll, |r, poll| {
            match search(&self.rules[r], &self.premises[r], poll) {
                true => ControlFlow::Break(()),
                false => ControlFlow::Continue(()),
            }
        });
        if full.is_none() && self.transitive.contains(&fact[1]) && !poll.attempt(1) {
            search(
                &transitivity(fact[1]),
                self.transitive_premises(fact[1]),
                poll,
            );
        }
        bodies
    }

    /// Whether `fact` has a one-step derivation from the facts of `source` (`Seg::All`): a
    /// bodiless instance, a rule whose head matches it, or (optionally: the caller may
    /// recompute its closure) transitivity; searching under `stop`
    /// ([`Self::derivable_through`]).
    pub fn derivable_with<S: Source + ?Sized>(
        &self,
        source: &S,
        fact: Triple,
        transitivity_too: bool,
        stop: Stop<'_>,
    ) -> bool {
        self.derivable_through(source, fact, &|_| true, true, transitivity_too, stop)
    }

    /// The one answer to whether `fact` has a one-step derivation from `source`, by the
    /// kinds the caller names: an instance of a rule `rule` admits (by index); with
    /// `bodiless_too`, a bodiless instance whose schema facts `source` holds; with
    /// `transitivity_too`, transitivity. It asks `stop` before any work, then polls it per
    /// unit attempted ([`Poller::attempt`]): each bodiless alternative, each producer, each
    /// search, each fact a search scans ([`walk_until`]; one that finds nothing can try a
    /// product of a body's matches, N10 review r1, F2). Stopped, it answers false, decided
    /// by nothing: the caller asks its (latched) stop again and discards what it decided
    /// with it.
    fn derivable_through<S: Source + ?Sized>(
        &self,
        source: &S,
        fact: Triple,
        rule: &dyn Fn(usize) -> bool,
        bodiless_too: bool,
        transitivity_too: bool,
        stop: Stop<'_>,
    ) -> bool {
        // Before the bodiless and producer prelude (review r1, F2).
        let Some(poll) = Poller::new(stop) else {
            return false;
        };
        if bodiless_too
            && self.bodiless.contains(&fact)
            && self
                .for_each_alternative(Grounding::Bodiless(fact), &poll, |body, poll| {
                    match body.iter().all(|&g| source.contains(g, poll)) {
                        true => ControlFlow::Break(()),
                        false => ControlFlow::Continue(()),
                    }
                })
                .is_some()
        {
            return true;
        }
        let mut found = false;
        let by_rule = self.for_each_producer(fact, &poll, |r, poll| {
            if rule(r) {
                derivations(source, &self.rules[r], fact, Seg::All, poll, &mut |_| {
                    found = true;
                    true
                });
            }
            match found {
                true => ControlFlow::Break(()),
                false => ControlFlow::Continue(()),
            }
        });
        if by_rule.is_some() {
            return true;
        }
        if transitivity_too && self.transitive.contains(&fact[1]) {
            derivations(
                source,
                &transitivity(fact[1]),
                fact,
                Seg::All,
                &poll,
                &mut |_| {
                    found = true;
                    true
                },
            );
        }
        found
    }
}

impl GroundProgram {
    /// The closed family rule `r` is an instance of (its schema link), if it is one and
    /// the link is transitive in this program, so that its instances are grounded on a
    /// closed hierarchy ([`closed_family`]).
    pub fn closed_family(&self, r: usize) -> Option<u64> {
        self.families
            .get(r)
            .copied()
            .flatten()
            .filter(|link| self.transitive.contains(link))
    }

    /// The rules (indexes into `rules`) whose head can give `fact`, each once, ascending.
    fn producer_list(&self, fact: Triple) -> Vec<usize> {
        let mut rules: Vec<usize> = self.heads.rules_matching(fact).collect();
        rules.dedup();
        rules
    }

    /// Every schema fact a rule instance, bodiless fact or transitive predicate was
    /// grounded on, in any of its alternatives.
    pub fn schema_premises(&self) -> HashSet<Triple> {
        self.premises
            .iter()
            .chain(self.bodiless_premises.values())
            .chain(self.transitive_premises.values())
            .chain(self.other_premises.values().flatten())
            .flatten()
            .copied()
            .collect()
    }

    /// Whether one of the rules `only` marks, or with `bodiless_too` a bodiless instance,
    /// derives `fact` from `source` in one step ([`Self::derivable_through`], without
    /// transitivity), searching under `stop`.
    pub fn derivable_by<S: Source + ?Sized>(
        &self,
        source: &S,
        fact: Triple,
        only: &[bool],
        bodiless_too: bool,
        stop: Stop<'_>,
    ) -> bool {
        self.derivable_through(
            source,
            fact,
            &|r| only.get(r).copied().unwrap_or(false),
            bodiless_too,
            false,
            stop,
        )
    }
}

/// What a ground program grounded from schema facts: a rule (by index), a bodiless fact
/// or a transitive predicate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Grounding {
    Rule(usize),
    Bodiless(Triple),
    Transitive(u64),
}

/// The transitivity rule over `p`, for evaluation where the module doesn't apply
/// (overdeletion, rederivation).
pub fn transitivity(p: u64) -> Rule {
    let (x, y, z) = (Term::Var(0), Term::Var(1), Term::Var(2));
    Rule {
        name: "prp-trp".to_owned(),
        body: vec![Atom([x, Term::Const(p), y]), Atom([y, Term::Const(p), z])],
        guards: Vec::new(),
        head: Head::Facts(vec![Atom([x, Term::Const(p), z])]),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::super::ir::{Atom, Head, Rule, Term};
    use super::*;

    /// Facts in a list; counts the scans and matches of predicate 12.
    struct Counting {
        facts: Vec<Triple>,
        scans: AtomicUsize,
        matches: AtomicUsize,
    }

    fn matches(pattern: [Option<u64>; 3], fact: Triple) -> bool {
        pattern
            .iter()
            .zip(fact)
            .all(|(want, have)| want.is_none_or(|v| v == have))
    }

    /// Each fact a scan visits is a position, the ones that don't match skipped (an
    /// estimate or probe stands for an index lookup: its call's one unit).
    impl Visits for Counting {
        fn visit(&self, pattern: [Option<u64>; 3], seg: Seg, visit: &mut Visit<'_, '_>) {
            if seg == Seg::Delta {
                return;
            }
            if pattern[1] == Some(12) {
                self.scans.fetch_add(1, Ordering::Relaxed);
            }
            for &fact in &self.facts {
                let going = match matches(pattern, fact) {
                    true => {
                        if fact[1] == 12 {
                            self.matches.fetch_add(1, Ordering::Relaxed);
                        }
                        visit.fact(fact)
                    }
                    false => visit.skip(),
                };
                if !going {
                    return;
                }
            }
        }

        fn size(&self, pattern: [Option<u64>; 3], seg: Seg, _: Probe<'_, '_>) -> usize {
            match seg {
                Seg::Delta => 0,
                _ => self.facts.iter().filter(|&&f| matches(pattern, f)).count(),
            }
        }

        fn holds(&self, fact: Triple, _: Probe<'_, '_>) -> bool {
            self.facts.contains(&fact)
        }
    }

    /// A morsel's results hold no more than `keep` left: a round whose heads are all known
    /// returns empty buffers, not ones the size of what its morsels emitted.
    #[test]
    fn a_morsel_keeps_no_room_for_what_keep_removed() {
        // (?x 11 ?y) -> (?x 12 ?y) over 10,000 pairs, every head already a fact: three
        // morsels, each emitting up to MORSEL heads and keeping none.
        let mut facts = Vec::new();
        for x in 0..100 {
            for y in 0..100 {
                facts.extend([[x, 11, 1_000 + y], [x, 12, 1_000 + y]]);
            }
        }
        let source = Counting {
            facts,
            scans: AtomicUsize::new(0),
            matches: AtomicUsize::new(0),
        };
        let rule = Rule {
            name: "copy".to_owned(),
            body: vec![Atom([Term::Var(0), Term::Const(11), Term::Var(1)])],
            guards: vec![],
            head: Head::Facts(vec![Atom([Term::Var(0), Term::Const(12), Term::Var(1)])]),
        };
        let jobs: Vec<Job<'_>> = Job::full(&source, &rule, &Poller::never())
            .into_iter()
            .collect();
        let probes = Probes::default();
        let capacities = run_jobs_by_morsel(
            &source,
            &jobs,
            &|facts: &mut Vec<Triple>, poll: &Poller<'_>| {
                facts.retain(|&f| !source.contains(f, poll))
            },
            None,
            &|| false,
            &probes,
            &|_, facts| (facts.len(), facts.capacity()),
        );
        assert!(capacities.len() > 1, "several morsels");
        assert_eq!(probes.emitted.load(Ordering::Relaxed), 10_000);
        assert!(capacities.iter().all(|&c| c == (0, 0)), "{capacities:?}");
    }

    #[test]
    fn one_derivation_asked_for_scans_one_branch() {
        // (1 11 ?x), (?x 12 ?y) -> (1 13 2): 100 x's with 100 y's each. Asking for the
        // first derivation scanned all 100 branches and their 10,000 matches before.
        let mut facts = Vec::new();
        for x in 100..200 {
            facts.push([1, 11, x]);
            facts.extend((1_000..1_100).map(|y| [x, 12, y]));
        }
        let source = Counting {
            facts,
            scans: AtomicUsize::new(0),
            matches: AtomicUsize::new(0),
        };
        let rule = Rule {
            name: "probe".to_owned(),
            body: vec![
                Atom([Term::Const(1), Term::Const(11), Term::Var(0)]),
                Atom([Term::Var(0), Term::Const(12), Term::Var(1)]),
            ],
            guards: vec![],
            head: Head::Facts(vec![Atom([1, 13, 2].map(Term::Const))]),
        };
        let mut emitted = 0;
        let poll = Poller::never();
        derivations(&source, &rule, [1, 13, 2], Seg::All, &poll, &mut |_| {
            emitted += 1;
            true
        });
        assert_eq!(emitted, 1);
        assert_eq!(
            source.scans.load(Ordering::Relaxed),
            1,
            "one branch scanned"
        );
        assert!(
            source.matches.load(Ordering::Relaxed) <= 100,
            "{} matches",
            source.matches.load(Ordering::Relaxed)
        );
        // Not stopping, every derivation.
        let mut all = 0;
        derivations(&source, &rule, [1, 13, 2], Seg::All, &poll, &mut |_| {
            all += 1;
            false
        });
        assert_eq!(all, 10_000);
    }

    /// A poller shared by several searches (one per parallel task: the support sets'
    /// one-step searches) stops one; every later search on it scans nothing, not even the
    /// first scan it would open (sol-loop r4, R4-1).
    #[test]
    fn a_search_on_a_stopped_poller_scans_nothing() {
        // (?x 12 ?y), (?y 12 ?z) -> (?x 13 ?z), asked for (1 13 2): 5,000 facts (1 12 y) and
        // 6,000 (w 12 2), no y a w, so no path. The search starts from the smaller side, the
        // 5,000 (1 12 y), scans them and finds nothing.
        let facts: Vec<Triple> = (1_000..6_000)
            .map(|y| [1, 12, y])
            .chain((10_000..16_000).map(|w| [w, 12, 2]))
            .collect();
        let source = Counting {
            facts,
            scans: AtomicUsize::new(0),
            matches: AtomicUsize::new(0),
        };
        let (x, y, z) = (Term::Var(0), Term::Var(1), Term::Var(2));
        let rule = Rule {
            name: "chain".to_owned(),
            body: vec![Atom([x, Term::Const(12), y]), Atom([y, Term::Const(12), z])],
            guards: vec![],
            head: Head::Facts(vec![Atom([x, Term::Const(13), z])]),
        };
        // Not at the poller's start; at its first poll in the walk.
        let asks = AtomicUsize::new(0);
        let stop = || asks.fetch_add(1, Ordering::Relaxed) >= 1;
        let poll = Poller::new(&stop).expect("not stopped yet");
        derivations(&source, &rule, [1, 13, 2], Seg::All, &poll, &mut |_| false);
        assert!(poll.stopped(), "the first search outlived POLL_UNITS facts");
        source.scans.store(0, Ordering::Relaxed);
        source.matches.store(0, Ordering::Relaxed);
        derivations(&source, &rule, [1, 13, 2], Seg::All, &poll, &mut |_| false);
        assert_eq!(
            (
                source.scans.load(Ordering::Relaxed),
                source.matches.load(Ordering::Relaxed)
            ),
            (0, 0),
            "scans opened and facts scanned by a search on a stopped poller"
        );
    }
}
