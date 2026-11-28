//! The upper bound U1 in a stack of its own (docs/design/owl2-dl.md §8, package 4.1).
//!
//! U1 is PAGOdA's datalog strengthening of the asserted ontology, compiled by
//! `nrese_dl::bounds` (disjunctions split, existentials c-Skolemised, `⊥` neutralised to
//! a clash fact). It is evaluated **over L ∪ data**: its input is the store's
//! materialised view (asserted and inferred statements, every graph) plus the program's
//! own facts, so U1 ⊇ L by construction. Only what it derives beyond that input is kept
//! here, in a [`TripleVersion`]: never in the engine's inferred stack, so never visible as inferred
//! data. Queries read it through a snapshot of their own (`super::query`).
//!
//! **Maintained per commit** by the reasoner's delta executor (`nrese_reasoner::delta`),
//! an ordinary incremental datalog materialisation: [`Upper::change`] (applied by [`Upper::apply`]) takes the
//! commit's changes to the materialised view (asserted and inferred) as the change of
//! U1's input. A commit that changes the ontology's schema (anything but assertions over
//! the signature U1 was compiled for) recompiles U1 and evaluates it afresh
//! ([`Upper::is_assertion`]): its rules are the TBox's.
//!
//! **Data values by value** (`Values`): U1 reads every literal as its value's
//! representative, so keys, `hasValue` and joins over data values match equal values
//! written differently, as OWL 2 DL's identity of data values has it.
//!
//! **Equality by representatives** (ADR-0011's monotone backend, the reasoner's
//! `materialise_representatives_until`): U1's `owl:sameAs` classes are kept as one
//! representative each, never as the pairs of a class (OWL2Bench DL-1: one class of about
//! 78 k terms, 6 × 10⁹ pairs). The stack holds the closure over representatives and each
//! other identity as `identity sameAs representative`, which the upper view reads expanded
//! ([`Upper::classes`]). A U1 with classes is evaluated afresh on each commit, as is one a
//! commit's equality would give classes ([`Change::merges`]): the delta executor reads
//! facts as stored, and a class that may split is never maintained.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

use nrese_dl::bounds;
use nrese_engine::{
    CountedSnapshot, GraphSelector, QuadPattern, ReadModel, Snapshot, TermId, TripleVersion,
};
use nrese_owl::{Normalised, Ontology};
use nrese_rdf::{LiteralRef, NamedNodeRef, TermRef};
use nrese_reasoner::delta::{self, Base, VisitsBase};
use nrese_reasoner::eval::{GroundProgram, Poller, Probe, Schema, Stop, Stopped, Visit};
use nrese_reasoner::ir::{self, Triple};

/// A pattern over every graph.
fn pattern([s, p, o]: [Option<u64>; 3]) -> QuadPattern {
    QuadPattern {
        subject: s.map(TermId::from_raw),
        predicate: p.map(TermId::from_raw),
        object: o.map(TermId::from_raw),
        graph: GraphSelector::Any,
    }
}

/// Data values by value (OWL 2's identity of data values, [`nrese_xsd::owl::Value`]): each
/// literal U1 meets has a representative, the first literal of its value it met, and U1
/// reads, joins and derives representatives only. So a key, a `hasValue` and every join
/// over data values match equal values written differently (`"07"` and `"7"` as
/// `xsd:integer`, `"7"^^xsd:integer` and `"7.0"^^xsd:decimal`), as OWL 2 DL does: without
/// it, U1 would miss what such an equality entails and stop being an upper bound. A
/// literal whose value can't be read (ill-typed, outside the map, beyond the value spaces)
/// is its own, and U1 then proves no consistency: an equal value written otherwise would be
/// a join it misses ([`Upper::proves_consistency`], [`Upper::reads_every_value`]).
/// Representatives are ids the store has already: nothing is interned (replicas too).
#[derive(Debug, Default)]
pub(crate) struct Values {
    /// A literal's representative, where it isn't the literal itself.
    rep: HashMap<u64, u64>,
    /// A representative's literals (itself first), for values written two or more ways.
    members: HashMap<u64, Vec<u64>>,
    by_value: HashMap<nrese_xsd::owl::Value, u64>,
    /// Literals already read.
    seen: HashSet<u64>,
    /// A compared literal's value couldn't be read.
    unread: bool,
}

impl Values {
    /// Notes `id` if it is a literal: its value's representative from now on.
    fn add(&mut self, view: &Snapshot, id: u64) {
        if !TermId::from_raw(id).kind().is_literal() || !self.seen.insert(id) {
            return;
        }
        let Some(value) = value_of(view, id) else {
            self.unread = true;
            return;
        };
        match self.by_value.entry(value) {
            std::collections::hash_map::Entry::Vacant(e) => {
                e.insert(id);
            }
            std::collections::hash_map::Entry::Occupied(e) => {
                let rep = *e.get();
                self.rep.insert(id, rep);
                self.members
                    .entry(rep)
                    .or_insert_with(|| vec![rep])
                    .push(id);
            }
        }
    }

    /// Notes the literals among the objects of `facts` whose predicate U1's rules compare
    /// values of (`compared`).
    fn add_objects<'a>(
        &mut self,
        view: &Snapshot,
        facts: impl IntoIterator<Item = &'a Triple>,
        compared: &HashSet<u64>,
    ) {
        for t in facts {
            if compared.contains(&t[1]) {
                self.add(view, t[2]);
            }
        }
    }

    /// `id`'s representative (itself for anything but a literal of a value met before).
    pub(crate) fn of(&self, id: u64) -> u64 {
        self.rep.get(&id).copied().unwrap_or(id)
    }

    /// `fact` with its object's representative (literals stand only as objects).
    fn map(&self, [s, p, o]: Triple) -> Triple {
        [s, p, self.of(o)]
    }

    /// Keeps what a commit's change noted ([`Noting`]), its commit done.
    fn absorb(&mut self, noted: Values) {
        self.rep.extend(noted.rep);
        for (rep, new) in noted.members {
            self.members
                .entry(rep)
                .or_insert_with(|| vec![rep])
                .extend(new);
        }
        self.by_value.extend(noted.by_value);
        self.seen.extend(noted.seen);
        self.unread |= noted.unread;
    }
}

/// U1's literals by value as a commit's change reads them: U1's own ([`Values`]), unchanged,
/// and those the change meets first, `added`, which U1 keeps only when the commit happens
/// ([`Upper::apply`]). `added.members` holds only the literals the change adds to a value.
struct Noting<'a> {
    base: &'a Values,
    added: Values,
}

impl Noting<'_> {
    fn add(&mut self, view: &Snapshot, id: u64) {
        if !TermId::from_raw(id).kind().is_literal()
            || self.base.seen.contains(&id)
            || !self.added.seen.insert(id)
        {
            return;
        }
        let Some(value) = value_of(view, id) else {
            self.added.unread = true;
            return;
        };
        let known = self.base.by_value.get(&value);
        match known.or_else(|| self.added.by_value.get(&value)).copied() {
            None => {
                self.added.by_value.insert(value, id);
            }
            Some(rep) => {
                self.added.rep.insert(id, rep);
                self.added.members.entry(rep).or_default().push(id);
            }
        }
    }

    fn add_objects<'t>(
        &mut self,
        view: &Snapshot,
        facts: impl IntoIterator<Item = &'t Triple>,
        compared: &HashSet<u64>,
    ) {
        for t in facts {
            if compared.contains(&t[1]) {
                self.add(view, t[2]);
            }
        }
    }

    fn of(&self, id: u64) -> u64 {
        let rep = self.added.rep.get(&id).or_else(|| self.base.rep.get(&id));
        rep.copied().unwrap_or(id)
    }

    fn map(&self, [s, p, o]: Triple) -> Triple {
        [s, p, self.of(o)]
    }

    /// Every term `rep` stands for: the literals with its value (`rep` first), or `rep`
    /// alone, and those of its value the change met.
    fn members(&self, rep: u64) -> impl Iterator<Item = u64> + '_ {
        let base = self.base.members.get(&rep);
        base.into_iter()
            .flatten()
            .copied()
            .chain(base.is_none().then_some(rep))
            .chain(self.added.members.get(&rep).into_iter().flatten().copied())
    }
}

/// The OWL 2 data value of the literal `id`; `None` for anything else, or a literal whose
/// value can't be read.
fn value_of(view: &Snapshot, id: u64) -> Option<nrese_xsd::owl::Value> {
    let nrese_rdf::Term::Literal(literal) = view.decode(TermId::from_raw(id))? else {
        return None;
    };
    let datatype = nrese_xsd::owl::Datatype::from_iri(literal.datatype().as_str())?;
    nrese_xsd::owl::Value::parse(literal.value(), datatype, literal.language()).ok()
}

/// U1's input and its stack as the delta executor reads them: the store's materialised
/// view after the change and the program's facts count as asserted, the stack as
/// inferred. The view's literals are read as their representatives ([`Values`]).
struct UpperBase<'a> {
    /// Read only counted: the engine's work below each read on the reading task's poller
    /// (REAS-3, sweeps SW020, SW021).
    view: CountedSnapshot,
    facts: &'a HashSet<Triple>,
    stack: &'a TripleVersion,
    values: &'a Noting<'a>,
}

impl UpperBase<'_> {
    /// Whether the view has `s p o`, `o` standing for every literal of its value: the call
    /// counted before its value's members are looked up (one probe), each member's lookup
    /// counted before it is made (a value can be written in many ways).
    /// [`Stopped`] once `poll` has stopped before a member was found, never a "not in the
    /// view" that would keep the fact.
    fn in_view(&self, [s, p, o]: Triple, poll: &Poller<'_>) -> Result<bool, Stopped> {
        #[cfg(test)]
        crate::witness::add(crate::witness::Site::InView, 1);
        if poll.tick() {
            return Err(Stopped);
        }
        // Each member's lookup counted in the engine too (REAS-3, SW020).
        let reads = |n| poll.visit(n);
        for o in poll.counted(self.values.members(o)) {
            let first = self
                .view
                .quads(
                    ReadModel::Materialised,
                    &pattern([Some(s), Some(p), Some(o)]),
                    &reads,
                )
                .next()
                .transpose()?;
            if first.is_some() {
                return Ok(true);
            }
        }
        poll.ended()?;
        Ok(false)
    }
}

/// Reads count as `Base` says: the call, each object member it looks up, each quad and
/// program fact a scan visits, and each stack fact it delivers.
impl VisitsBase for UpperBase<'_> {
    fn visit(&self, bound: [Option<u64>; 3], visit: &mut Visit<'_, '_>) {
        let [s, p, o] = bound;
        // A bound object's members read as they are visited, never copied first (a value can be written in many
        // ways).
        let objects = o
            .map(|o| self.values.members(o))
            .into_iter()
            .flatten()
            .map(Some)
            .chain(o.is_none().then_some(None));
        let poll = visit.poller();
        let reads = |n| poll.visit(n);
        for o in objects {
            // Each member's lookup is a unit.
            if !visit.skip() {
                return;
            }
            for q in self
                .view
                .quads(ReadModel::Materialised, &pattern([s, p, o]), &reads)
            {
                // Stopped (SW021): the scan's caller reads its poller as stopped.
                let Ok(q) = q else {
                    return;
                };
                let fact = self
                    .values
                    .map([q.subject.raw(), q.predicate.raw(), q.object.raw()]);
                if !visit.fact(fact) {
                    return;
                }
            }
        }
        for t in self.facts {
            let going = match bound.iter().zip(t).all(|(b, v)| b.is_none_or(|b| b == *v)) {
                true => visit.fact(*t),
                false => visit.skip(),
            };
            if !going {
                return;
            }
        }
        let Ok(facts) = self.stack.scan_counted(bound, &reads) else {
            return;
        };
        for fact in facts {
            let Ok(fact) = fact else {
                return;
            };
            if !visit.fact(fact) {
                return;
            }
        }
    }

    /// The view's count's work counted on `probe` as it goes; [`Stopped`]
    /// where that, or the stack's estimate, stopped.
    fn size(&self, bound: [Option<u64>; 3], probe: Probe<'_, '_>) -> Result<usize, Stopped> {
        let view = self
            .view
            .count(ReadModel::Materialised, &pattern(bound), &|n| {
                probe.visited(n)
            })?
            .at_least();
        let stack = estimate(self.stack, bound, &|n| probe.visited(n))?;
        probe.ended()?;
        // An expanded count past `u64` reads as its maximum (`Snapshot::count_in`): the sum
        // saturates too, never wrapping to a smaller estimate.
        Ok(usize::try_from(view)
            .unwrap_or(usize::MAX)
            .saturating_add(stack)
            .saturating_add(self.facts.len()))
    }

    fn holds(&self, fact: Triple, probe: Probe<'_, '_>) -> Result<bool, Stopped> {
        Ok(self
            .stack
            .any_counted(fact.map(Some), &|n| probe.visited(n))?
            || self.asserts(fact, probe)?)
    }

    /// Each literal member the view is asked about is a unit, counted as it is asked.
    fn asserts(&self, fact: Triple, probe: Probe<'_, '_>) -> Result<bool, Stopped> {
        Ok(self.facts.contains(&fact) || self.in_view(fact, probe.poller())?)
    }
}

/// Upper bound on the matching facts. A narrower shape reports zero only after a
/// counted probe establishes that it is empty.
fn estimate(
    stack: &TripleVersion,
    bound @ [s, p, o]: [Option<u64>; 3],
    visit: nrese_engine::Visitor<'_>,
) -> Result<usize, Stopped> {
    let count = p.map_or_else(|| stack.len(), |p| stack.count_predicate(p));
    if count != 0 && (s.is_some() || o.is_some()) && !stack.any_counted(bound, visit)? {
        return Ok(0);
    }
    Ok(count)
}

/// Gives IRIs and literals their ids in the store's dictionary (U1's own terms: fresh
/// classes, Skolem constants, the clash): interned on a primary, looked up on a replica
/// ([`super::source::resolve`]), where a term no record brought yet is `missing`.
pub(crate) struct Interner<'a> {
    pub resolve: &'a dyn Fn(TermRef<'_>) -> Option<TermId>,
    pub missing: bool,
}

impl Interner<'_> {
    fn id(&mut self, term: TermRef<'_>) -> u64 {
        match (self.resolve)(term) {
            Some(id) => id.raw(),
            None => {
                self.missing = true;
                0
            }
        }
    }
}

impl ir::Vocabulary for Interner<'_> {
    fn iri(&mut self, iri: &str) -> u64 {
        self.id(NamedNodeRef::new_unchecked(iri).into())
    }

    fn literal(&mut self, lexical: &str, datatype: &str) -> u64 {
        self.id(
            LiteralRef::new_typed_literal(lexical, NamedNodeRef::new_unchecked(datatype)).into(),
        )
    }

    fn language_literal(&mut self, lexical: &str, language: &str) -> u64 {
        self.id(LiteralRef::new_language_tagged_literal_unchecked(lexical, language).into())
    }
}

/// A commit's change to the stack ([`Upper::change`]).
#[derive(Default)]
pub struct Change {
    moved: Vec<Triple>,
    insert: Vec<Triple>,
    remove: Vec<Triple>,
    program: Option<GroundProgram>,
    /// The literals the change read by value first ([`Noting`]).
    noted: Values,
    pub elapsed: Duration,
    /// The change derives an equality between two terms: U1 then has classes, which it
    /// keeps by representatives, so it is evaluated afresh instead.
    merges: bool,
}

impl Change {
    /// Whether the change equates two terms ([`Self::merges`] field).
    pub fn merges(&self) -> bool {
        self.merges
    }
}

/// Why U1 isn't available: its evaluation ran out of its budget, or (`refused`) memory
/// couldn't hold what it reads or keeps, or the process's memory stopped it. A refused U1 is
/// never kept: once memory is free again the next read builds it (SYN-61; REAS-3, C17: kept,
/// every later reader of the revision would be answered by the lower bound alone,
/// dl/query/mod.rs); one that gave up stays so for its revision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GaveUp {
    pub why: String,
    pub refused: bool,
}

impl GaveUp {
    /// Gave up within its budgets (time, a missing term).
    pub fn new(why: impl Into<String>) -> Self {
        Self {
            why: why.into(),
            refused: false,
        }
    }

    /// A budget refused what U1 reads or keeps, or the process's memory stopped it.
    pub fn refused(why: impl Into<String>) -> Self {
        Self {
            why: why.into(),
            refused: true,
        }
    }

    /// `self`, refused where `memory` says the process's memory stopped it (REAS-3, C17): a
    /// store stop that polls the memory watch remembers whether the watch fired.
    pub(crate) fn by_memory(self, memory: &std::sync::atomic::AtomicBool) -> Self {
        match self.refused || !memory.load(std::sync::atomic::Ordering::Relaxed) {
            true => self,
            false => Self::refused(format!(
                "{}: the process's memory is past its limit",
                self.why
            )),
        }
    }
}

/// U1's give-up for an evaluation the reasoner interrupted: refused where the process's
/// memory stopped it (`Interrupted::memory`, the reasoner's own memory fallback), never kept;
/// otherwise as `why` says.
fn gave_up(interrupted: &delta::Interrupted, why: &str) -> GaveUp {
    match interrupted.memory {
        Some(_) => GaveUp::refused(format!("{why}: the process's memory is past its limit")),
        None => GaveUp::new(why),
    }
}

/// Poll the caller's store watch as well as the process owner's safety fallback; the latter
/// is the process's memory, so a U1 it stops is refused, never kept (REAS-3, C17).
fn check_stop(stop: Stop<'_>) -> Result<(), GaveUp> {
    if nrese_reasoner::eval::over_memory_limit() {
        return Err(GaveUp::refused(
            "the upper bound's evaluation was stopped: the process's memory is past its limit",
        ));
    }
    if stop() {
        Err(GaveUp::new("the upper bound's evaluation was stopped"))
    } else {
        Ok(())
    }
}

/// The immutable part of U1 a read can keep while a commit updates the stack.
pub(super) struct ProgramFacts {
    pub(super) triples: HashSet<Triple>,
    pub(super) classes: nrese_reasoner::representatives::EqualityClasses,
}

/// Immutable evidence held together by a read of one publication.
#[derive(Clone)]
pub struct UpperVersion {
    pub program: Arc<bounds::Program>,
    pub stack: TripleVersion,
    pub eligible: TripleVersion,
    facts: Arc<ProgramFacts>,
    values_unread: bool,
    pub literals_by_value: usize,
}

/// U1: its published read evidence and the writer's state for maintaining it.
pub struct Upper {
    pub(super) version: UpperVersion,
    rules: Vec<ir::Rule>,
    schema: Schema,
    /// The `owl:sameAs` id `rules` hold the equality rules over, if they do, asked once with
    /// them ([`nrese_reasoner::delta::Rules::equality`]).
    equality: Option<u64>,
    /// The classes U1 was compiled for: memberships in others change it.
    signature: HashSet<u64>,
    /// Its object and data properties: an assertion that uses one as the other kind, or
    /// another property, changes it.
    object_properties: HashSet<u64>,
    data_properties: HashSet<u64>,
    /// The ground program of the delta executor, for the next commit.
    ground: Option<GroundProgram>,
    /// The literals U1 compares and their values' representatives: the objects of the
    /// predicates [`bounds::Program::compared_predicates`] names. Other literals are never
    /// compared, so they needn't be read by value (LUBM's names, e-mail addresses and
    /// telephone numbers: 107,410 literals at LUBM(10)). A commit's new literals are noted
    /// as its change is computed and kept with the change ([`Self::apply`]): U1 is read,
    /// never changed, while the change is computed.
    values: Values,
    rdf_type: u64,
    same_as: u64,
    /// How long the last evaluation or maintenance took.
    pub elapsed: Duration,
}

impl std::ops::Deref for Upper {
    type Target = UpperVersion;

    fn deref(&self) -> &Self::Target {
        &self.version
    }
}

impl std::fmt::Debug for Upper {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Upper")
            .field("rules", &self.rules.len())
            .field("facts", &self.facts.triples.len())
            .field("stack", &self.stack.len())
            .finish_non_exhaustive()
    }
}

fn slot(s: bounds::Slot) -> ir::Term {
    match s {
        bounds::Slot::Var(v) => ir::Term::Var(v),
        bounds::Slot::Const(t) => ir::Term::Const(t),
    }
}

/// U1's rules as the reasoner's (the atoms copied one to one, each constant literal as
/// its value's representative).
fn reasoner_rules(program: &bounds::Program, values: &Values) -> Vec<ir::Rule> {
    let slot = |s: bounds::Slot| match slot(s) {
        ir::Term::Const(t) => ir::Term::Const(values.of(t)),
        var => var,
    };
    let atom = |a: &bounds::Atom| ir::Atom(a.0.map(slot));
    program
        .rules
        .iter()
        .map(|r| ir::Rule {
            name: r.name.clone(),
            body: r.body.iter().map(atom).collect(),
            guards: r
                .distinct
                .iter()
                .map(|&(a, b)| ir::Guard::NotEqual(slot(a), slot(b)))
                .collect(),
            head: ir::Head::Facts(r.head.iter().map(atom).collect()),
        })
        .collect()
}

/// The materialised view's facts (every graph, each once).
/// The materialised view's facts (every graph, each once), charged to `budget` as they are
/// collected (SYN-61: the whole view, U1's build's largest scratch).
fn view_facts(
    view: &Snapshot,
    stop: Stop<'_>,
    budget: &std::sync::Arc<nrese_exec::Budget>,
) -> Result<nrese_exec::ChargedVec<Triple>, GaveUp> {
    let mut facts = nrese_exec::ChargedVec::new(std::sync::Arc::clone(budget));
    for q in view.quads_for_pattern_in(ReadModel::Materialised, &pattern([None, None, None])) {
        if facts.len() % 1024 == 0 {
            check_stop(stop)?;
        }
        facts
            .push([q.subject.raw(), q.predicate.raw(), q.object.raw()])
            .map_err(|_| GaveUp::refused(SCRATCH_REFUSED))?;
    }
    sort_dedup(&mut facts);
    Ok(facts)
}

/// Why U1 isn't available when its build's scratch doesn't fit.
const SCRATCH_REFUSED: &str = "its evaluation's input doesn't fit the queries' memory";

fn facts_refused(_: nrese_exec::BudgetExceeded) -> GaveUp {
    GaveUp::refused("its facts don't fit the queries' memory")
}

/// `facts` sorted, each once; in place, allocating nothing.
pub(crate) fn sort_dedup(facts: &mut nrese_exec::ChargedVec<Triple>) {
    facts.sort_unstable();
    let mut kept = 0;
    for i in 0..facts.len() {
        if i == 0 || facts[i] != facts[kept - 1] {
            facts[kept] = facts[i];
            kept += 1;
        }
    }
    facts.truncate(kept);
}

impl Upper {
    /// Compiles U1 for `ontology` and evaluates it over `view` (the materialised view the
    /// ontology was read from). `resolve` gives U1's own terms their ids (`None`: not in
    /// a replica's dictionary yet, and U1 isn't available); `stop` ends the evaluation (U1
    /// is then not available). O(the closure).
    pub fn build(
        ontology: &Ontology,
        normalised: &Normalised,
        view: &Snapshot,
        resolve: &dyn Fn(TermRef<'_>) -> Option<TermId>,
        stop: Stop<'_>,
        kept: &std::sync::Arc<nrese_exec::Budget>,
    ) -> Result<Self, GaveUp> {
        check_stop(stop)?;
        let started = Instant::now();
        let mut vocabulary = Interner {
            resolve,
            missing: false,
        };
        let mut program = bounds::compile(ontology, normalised, &mut |iri| {
            ir::Vocabulary::iri(&mut vocabulary, iri)
        });
        let schema = Schema::owl(&mut vocabulary);
        if vocabulary.missing {
            return Err(GaveUp::new(
                "its terms haven't reached this replica yet: the primary interns them in a commit",
            ));
        }
        // The literals U1 compares, by value: of the view, the program's facts, its rules.
        let compared = program.compared_predicates();
        let mut values = Values::default();
        let mut input = view_facts(view, stop, kept)?;
        values.add_objects(view, input.iter(), compared);
        let program_facts: Vec<Triple> = program.facts.iter().map(|(f, _)| *f).collect();
        values.add_objects(view, &program_facts, compared);
        for rule in &program.rules {
            for atom in rule.body.iter().chain(&rule.head) {
                for slot in atom.0 {
                    if let bounds::Slot::Const(t) = slot {
                        values.add(view, t);
                    }
                }
            }
        }
        let mut rules = reasoner_rules(&program, &values);
        // A rule longer than the reasoner numbers (`ir::Rule::fits`: a key of more than 127
        // properties) can't be evaluated: it is left out, and U1 doesn't cover its axioms.
        for (rule, ours) in program.rules.iter().zip(&rules) {
            if !ours.fits() {
                let axiom = rule.provenance.sources.iter().flatten().next().copied();
                let why = "a rule longer than the reasoner holds";
                program.incomplete.push((axiom.unwrap_or(usize::MAX), why));
            }
        }
        rules.retain(ir::Rule::fits);
        let facts: HashSet<Triple> = program_facts.iter().map(|&f| values.map(f)).collect();
        for t in input.iter_mut() {
            *t = values.map(*t);
        }
        for &fact in &facts {
            input
                .push(fact)
                .map_err(|_| GaveUp::refused(SCRATCH_REFUSED))?;
        }
        sort_dedup(&mut input);
        // The input's charge is held until the evaluation that consumes it returns.
        let (input, input_held) = input.into_parts();
        // Equality by representatives: the closure over one term per class, the input's
        // facts that mention a class rewritten beside it, each other identity placed in
        // its class.
        let m = nrese_reasoner::batch::materialise_representatives_until(
            nrese_reasoner::batch::Input::Facts(input),
            &rules,
            None,
            &schema,
            nrese_reasoner::batch::Listing::Stored,
            stop,
        )
        .map_err(|stopped| gave_up(&stopped, "the upper bound's evaluation was stopped"))?;
        let same_as = program.names.same_as;
        // What U1 keeps of the evaluation, charged to `kept` before it is made (SYN-61); the
        // evaluation's own working set is watched (`stop`), as the support sets' is.
        let mut facts_derived = nrese_exec::ChargedVec::new(Arc::clone(kept));
        for (i, t) in m.derived.into_iter().enumerate() {
            if i % 1024 == 0 {
                check_stop(stop)?;
            }
            facts_derived.push(t).map_err(facts_refused)?;
        }
        // The input is consumed and the derived facts (which may have reused it) freed.
        drop(input_held);
        for (representative, members) in m.classes.classes() {
            for (i, &member) in members.iter().filter(|&&m| m != representative).enumerate() {
                if i % 1024 == 0 {
                    check_stop(stop)?;
                }
                facts_derived
                    .push([member, same_as, representative])
                    .map_err(facts_refused)?;
            }
        }
        let stack =
            TripleVersion::build(facts_derived.iter().copied(), kept).map_err(facts_refused)?;
        drop(facts_derived);
        let eligible = TripleVersion::build(
            facts.iter().copied().filter(|t| {
                !t.iter().any(|&id| m.classes.class_of(id).is_some())
                    && !super::bounds::in_view(view, *t)
                    && !stack.contains(*t)
            }),
            kept,
        )
        .map_err(facts_refused)?;
        let s = &program.signature;
        let signature = s.classes.iter().copied().collect();
        let object_properties = s.object_properties.iter().copied().collect();
        let data_properties = s.data_properties.iter().copied().collect();
        Ok(Self {
            rdf_type: program.names.rdf_type,
            same_as: program.names.same_as,
            equality: nrese_reasoner::representatives::same_as(&rules, schema.same_as()),
            rules,
            schema,
            version: UpperVersion {
                program: Arc::new(program),
                facts: Arc::new(ProgramFacts {
                    triples: facts,
                    classes: m.classes,
                }),
                stack,
                eligible,
                values_unread: values.unread,
                literals_by_value: values.seen.len(),
            },
            signature,
            object_properties,
            data_properties,
            ground: None,
            values,
            elapsed: started.elapsed(),
        })
    }

    /// Whether a changed statement only asserts something about individuals over the
    /// signature U1 was compiled for, so U1's rules stay as they are: a class membership
    /// in a known class, an object property U1 knows as one between named individuals, a
    /// data property it knows as one with a literal, or an equality. Anything else
    /// (schema, blank nodes, OWL's vocabulary, new vocabulary, a property used as the
    /// other kind or as both, whose kind the reading then gives by its uses) means
    /// recompiling.
    pub fn is_assertion(&self, [s, p, o]: Triple) -> bool {
        let named = |t: u64| TermId::from_raw(t).kind() == nrese_engine::TermKind::Iri;
        if !named(s) {
            return false;
        }
        if p == self.rdf_type {
            return named(o) && self.signature.contains(&o);
        }
        if p == self.same_as {
            return named(o);
        }
        let object = self.object_properties.contains(&p);
        let data = self.data_properties.contains(&p);
        (named(o) && object && !data)
            || (TermId::from_raw(o).kind().is_literal() && data && !object)
    }

    /// The stack's change for a commit, computed before it and applied after it
    /// ([`Self::apply`]), so a commit that doesn't happen leaves U1 as it was: `view` is
    /// the materialised view after the commit, `inserted` the facts new to the view,
    /// `deleted` those gone from it (each checked here against the view and the
    /// program's facts). U1 is only read, so it is shared while this runs. Cost: the
    /// change's.
    pub fn change(
        &self,
        view: &Snapshot,
        inserted: &[Triple],
        deleted: &[Triple],
        stop: Stop<'_>,
    ) -> Result<Change, GaveUp> {
        check_stop(stop)?;
        let started = Instant::now();
        let mut values = Noting {
            base: &self.values,
            added: Values::default(),
        };
        values.add_objects(view, inserted, self.program.compared_predicates());
        let mut inserted: Vec<Triple> = inserted
            .iter()
            .map(|&t| values.map(t))
            .filter(|t| !self.facts.triples.contains(t))
            .collect();
        inserted.sort_unstable();
        inserted.dedup();
        // A fact U1 derived that is now in its input: it leaves the stack, and its
        // consequences are already there.
        let moved: Vec<Triple> = inserted
            .iter()
            .filter(|&&t| self.stack.contains(t))
            .copied()
            .collect();
        inserted.retain(|&t| !self.stack.contains(t));
        let mut deleted: Vec<Triple> = deleted
            .iter()
            .map(|&t| values.map(t))
            .filter(|t| !self.facts.triples.contains(t))
            .collect();
        deleted.sort_unstable();
        deleted.dedup();
        let base = UpperBase {
            view: view.counted(),
            facts: &self.facts.triples,
            stack: &self.stack,
            values: &values,
        };
        // Each deleted fact's lookups in the view, counted on the commit's stop;
        // the first that finds it stopped ends the change (read as "not
        // in the view", a stopped lookup kept every fact after it, each looked up).
        let stopped = || GaveUp::new("the upper bound's maintenance was stopped");
        let prep = Poller::new(stop).ok_or_else(stopped)?;
        let mut kept = 0;
        for i in 0..deleted.len() {
            let fact = deleted[i];
            if !base.in_view(fact, &prep).map_err(|Stopped| stopped())? {
                deleted[kept] = fact;
                kept += 1;
            }
        }
        deleted.truncate(kept);
        let mut change = Change {
            moved,
            ..Change::default()
        };
        if inserted.is_empty() && deleted.is_empty() {
            change.noted = values.added;
            return Ok(change);
        }
        let rules = delta::Rules {
            rules: &self.rules,
            lists: None,
            schema: &self.schema,
            equality: self.equality,
        };
        let update = delta::update_until(
            &base,
            &inserted,
            &deleted,
            rules,
            self.ground.as_ref(),
            stop,
        )
        .map_err(|stopped| gave_up(&stopped, "the upper bound's maintenance was stopped"))?;
        change.program = update.program;
        // An equality the commit brings (the RL closure's, say: a functional property is
        // OWL 2 RL) or U1 derives: U1 then has classes.
        let same_as = self.same_as;
        let equates = |t: &Triple| t[1] == same_as && t[0] != t[2];
        change.merges = inserted.iter().any(equates) || update.insert.iter().any(equates);
        change.remove = update.remove;
        // A probe per inserted fact, on the commit's stop as the update's reads are; the first
        // that finds it stopped ends the change.
        let poll = Poller::new(stop).ok_or_else(stopped)?;
        for t in update.insert {
            if !base.is_asserted(t, &poll).map_err(|Stopped| stopped())? {
                change.insert.push(t);
            }
        }
        change.noted = values.added;
        change.elapsed = started.elapsed();
        Ok(change)
    }

    /// Stages both exact deltas before the commit can publish either.
    pub(super) fn stage(
        &self,
        mut change: Change,
        changed: impl Iterator<Item = Triple>,
        view: &Snapshot,
        budget: &Arc<nrese_exec::Budget>,
        compacted: Option<(TripleVersion, TripleVersion)>,
    ) -> Result<(UpperVersion, Change), GaveUp> {
        let mut candidates = nrese_exec::ChargedVec::new(Arc::clone(budget));
        for t in changed.filter(|t| self.facts.triples.contains(t)) {
            candidates.push(t).map_err(facts_refused)?;
        }
        sort_dedup(&mut candidates);
        let mut inserts = nrese_exec::ChargedVec::new(Arc::clone(budget));
        let mut deletes = nrese_exec::ChargedVec::new(Arc::clone(budget));
        for &t in candidates.iter() {
            let was = self.eligible.contains(t);
            let now = !super::bounds::in_view(view, t);
            match (was, now) {
                (false, true) => inserts.push(t).map_err(facts_refused)?,
                (true, false) => deletes.push(t).map_err(facts_refused)?,
                _ => {}
            }
        }
        let (stack, eligible) =
            compacted.unwrap_or_else(|| (self.stack.clone(), self.eligible.clone()));
        let eligible = advance(&eligible, &inserts, &deletes)?;

        change.moved.append(&mut change.remove);
        change.moved.sort_unstable();
        change.moved.dedup();
        change.insert.sort_unstable();
        change.insert.dedup();
        change
            .moved
            .retain(|&t| stack.contains(t) && change.insert.binary_search(&t).is_err());
        change.insert.retain(|&t| !stack.contains(t));
        let stack = advance(&stack, &change.insert, &change.moved)?;
        let version = UpperVersion {
            program: Arc::clone(&self.program),
            facts: Arc::clone(&self.facts),
            values_unread: self.values.unread || change.noted.unread,
            literals_by_value: self.values.seen.len() + change.noted.seen.len(),
            stack,
            eligible,
        };
        Ok((version, change))
    }

    /// Installs the writer's accompanying state after its evidence was published.
    pub(super) fn apply(&mut self, change: Change, version: UpperVersion) {
        if let Some(program) = change.program {
            self.ground = Some(program);
        }
        self.values.absorb(change.noted);
        self.elapsed = change.elapsed;
        self.version = version;
    }
}

fn advance(
    previous: &TripleVersion,
    inserts: &[Triple],
    deletes: &[Triple],
) -> Result<TripleVersion, GaveUp> {
    let version = previous
        .successor(inserts, deletes)
        .map_err(facts_refused)?;
    if let Some(splice) = version
        .compact(nrese_engine::CompactionPolicy::default(), true)
        .map_err(facts_refused)?
    {
        return Ok(splice(&version)
            .map_err(facts_refused)?
            .expect("compaction against its own version"));
    }
    Ok(version)
}

impl UpperVersion {
    /// The terms a clash was derived at (`⊥` fired in U1): the ontology may be
    /// inconsistent.
    pub fn clashes(&self) -> usize {
        self.stack.count_predicate(self.program.names.clash)
    }

    /// Whether U1 proves the ontology consistent (PAGOdA, Theorem 5.5 (i)): no clash, and
    /// U1 checks every `⊥` ([`bounds::Program::proves_consistency`]).
    pub fn proves_consistency(&self) -> bool {
        self.program.proves_consistency() && self.clashes() == 0 && !self.values_unread
    }

    /// U1's `owl:sameAs` classes: its stack is over their representatives, each other
    /// identity stored as `identity sameAs representative`; empty where nothing is
    /// equated. A U1 with classes is evaluated afresh on each commit.
    pub fn classes(&self) -> &nrese_reasoner::representatives::EqualityClasses {
        &self.facts.classes
    }

    /// `owl:sameAs`, as U1's program names it.
    pub fn same_as(&self) -> u64 {
        self.program.names.same_as
    }

    /// Whether `term` is one of U1's own (a Skolem constant, a fresh class, the clash).
    pub fn is_internal(&self, term: u64) -> bool {
        self.program.is_internal(term)
    }
}

#[cfg(test)]
mod tests {
    use super::{Triple, TripleVersion, estimate};

    fn stack(facts: impl IntoIterator<Item = Triple>) -> TripleVersion {
        TripleVersion::build(facts, &std::sync::Arc::new(nrese_exec::Budget::unlimited())).unwrap()
    }

    /// REAS-2 (a counted test per leaf that filters): U1's base counts
    /// the program facts its scan passes over, not only those it delivers. Over three poll
    /// intervals of facts, none matching, a poller whose stop has fired polls within the scan
    /// and is stopped by it; uncounted, it would never poll.
    #[test]
    fn the_upper_base_counts_the_facts_it_passes_over() {
        use nrese_reasoner::delta::Base;
        use std::sync::atomic::{AtomicBool, Ordering};
        let n = 3 * u64::from(nrese_exec::POLL_UNITS);
        let engine = nrese_engine::Engine::new(nrese_engine::EngineConfig {
            background_maintenance: false,
            ..nrese_engine::EngineConfig::default()
        })
        .unwrap();
        let view = engine.snapshot();
        let facts: std::collections::HashSet<[u64; 3]> = (0..n).map(|i| [i, 5, i]).collect();
        let (stack, values) = (stack([]), super::Values::default());
        let noting = super::Noting {
            base: &values,
            added: super::Values::default(),
        };
        let base = super::UpperBase {
            view: view.counted(),
            facts: &facts,
            stack: &stack,
            values: &noting,
        };
        let fired = AtomicBool::new(false);
        let stop = || fired.load(Ordering::Relaxed);
        let poll = nrese_reasoner::eval::Poller::new(&stop).expect("not stopped yet");
        fired.store(true, Ordering::Relaxed);
        let mut delivered = 0;
        let scanned = base.scan([None, Some(6), None], &poll, &mut |_| delivered += 1);
        assert!(scanned.is_err(), "a scan its poller stopped says so");
        assert_eq!(delivered, 0, "no fact matches");
        assert!(
            poll.stopped(),
            "{n} facts passed over without the poller polling"
        );
    }

    /// Contract §4.3c R6. A bound scan polls while it cancels the deleted prefix
    /// and reports the stop before the final live fact.
    #[test]
    fn the_upper_base_counts_the_stack_facts_it_passes_over() {
        use nrese_reasoner::delta::Base;
        use std::sync::atomic::{AtomicBool, Ordering};
        let n = 3 * u64::from(nrese_exec::POLL_UNITS);
        let engine = nrese_engine::Engine::new(nrese_engine::EngineConfig {
            background_maintenance: false,
            ..nrese_engine::EngineConfig::default()
        })
        .unwrap();
        let view = engine.snapshot();
        let facts = std::collections::HashSet::new();
        let deleted: Vec<_> = (0..n).map(|c| [1, 2, 1_000 + c]).collect();
        let tail = [1, 2, 1_000 + n];
        let stack = stack(deleted.iter().copied().chain([tail]))
            .successor(&[], &deleted)
            .unwrap();
        let values = super::Values::default();
        let noting = super::Noting {
            base: &values,
            added: super::Values::default(),
        };
        let base = super::UpperBase {
            view: view.counted(),
            facts: &facts,
            stack: &stack,
            values: &noting,
        };
        let fired = AtomicBool::new(false);
        let stop = || fired.load(Ordering::Relaxed);
        let poll = nrese_reasoner::eval::Poller::new(&stop).expect("not stopped yet");
        fired.store(true, Ordering::Relaxed);
        let mut delivered = 0;
        let scanned = base.scan([Some(1), Some(2), None], &poll, &mut |_| delivered += 1);
        assert!(scanned.is_err(), "a scan its poller stopped says so");
        assert_eq!(delivered, 0, "the last fact is beyond the stopped prefix");
        assert_eq!(
            stack.scan([Some(1), Some(2), None]).collect::<Vec<_>>(),
            [tail]
        );
        assert!(
            poll.stopped(),
            "{n} stack facts passed over without the poller polling"
        );
    }

    /// U1's base over one literal value written `n` ways (`Values`: one representative, its
    /// lexical members): an empty view, no program facts, an empty stack.
    fn one_value_written(n: u64) -> (nrese_engine::Engine, super::Values, u64) {
        let engine = nrese_engine::Engine::new(nrese_engine::EngineConfig {
            background_maintenance: false,
            ..nrese_engine::EngineConfig::default()
        })
        .unwrap();
        let rep = 1_000_000;
        let mut values = super::Values::default();
        values.members.insert(rep, (rep..rep + n).collect());
        (engine, values, rep)
    }

    /// U1's base reads a value's lexical members as it visits them, each
    /// counted: a scan bound to a value written 64 × `POLL_UNITS` ways, its stop fired since
    /// its poller was made, stops within a poll interval of members and holds no copy of the
    /// member list (at most a quarter of its bytes at any time). Copied first, the whole list
    /// was copied before the first member was counted.
    #[test]
    fn the_upper_base_reads_a_value_s_members_as_it_visits_them() {
        use nrese_reasoner::delta::Base;
        use std::sync::atomic::{AtomicBool, Ordering};
        if !nrese_exec::measure::alone() {
            return;
        }
        let n = 64 * u64::from(nrese_exec::POLL_UNITS);
        let (engine, values, rep) = one_value_written(n);
        let view = engine.snapshot();
        let (facts, stack) = (std::collections::HashSet::new(), stack([]));
        let noting = super::Noting {
            base: &values,
            added: super::Values::default(),
        };
        let base = super::UpperBase {
            view: view.counted(),
            facts: &facts,
            stack: &stack,
            values: &noting,
        };
        let fired = AtomicBool::new(false);
        let stop = || fired.load(Ordering::Relaxed);
        let poll = nrese_reasoner::eval::Poller::new(&stop).expect("not stopped yet");
        fired.store(true, Ordering::Relaxed);
        let (delivered, held) = nrese_exec::measure::measure(|| {
            let mut delivered = 0;
            let scanned = base.scan([None, Some(5), Some(rep)], &poll, &mut |_| delivered += 1);
            assert!(scanned.is_err(), "a scan its poller stopped says so");
            delivered
        });
        assert_eq!(delivered, 0, "the view holds nothing");
        assert!(
            poll.stopped(),
            "{n} members visited without the poller polling"
        );
        let list = n as usize * size_of::<u64>();
        assert!(
            held <= list / 4,
            "{held} bytes held by a stopped scan: the {list}-byte member list was copied"
        );
    }

    /// A negative lookup of a fact whose object is a value written
    /// 8 × `POLL_UNITS` ways asks the view about each member, each counted as it is asked:
    /// the stop at least once every `POLL_UNITS` plus one run (the longest run counted at
    /// once, `POLL_UNITS`) of members. Counted all at once, the whole list was one ask.
    #[test]
    fn a_negative_upper_base_lookup_asks_its_stop_within_a_run() {
        use nrese_reasoner::delta::Base;
        use std::sync::atomic::{AtomicUsize, Ordering};
        let interval = nrese_exec::POLL_UNITS as usize;
        let n = 8 * interval as u64;
        let (engine, values, rep) = one_value_written(n);
        let view = engine.snapshot();
        let (facts, stack) = (std::collections::HashSet::new(), stack([]));
        let noting = super::Noting {
            base: &values,
            added: super::Values::default(),
        };
        let base = super::UpperBase {
            view: view.counted(),
            facts: &facts,
            stack: &stack,
            values: &noting,
        };
        let asks = AtomicUsize::new(0);
        let stop = || {
            asks.fetch_add(1, Ordering::Relaxed);
            false
        };
        let poll = nrese_reasoner::eval::Poller::new(&stop).expect("never stopped");
        assert!(
            !base.contains([1, 5, rep], &poll).expect("never stopped"),
            "the view asserts no member"
        );
        let run = interval;
        let asks = asks.load(Ordering::Relaxed);
        assert!(
            asks >= n as usize / (interval + run),
            "{asks} asks of the stop for {n} members asked about: more than {interval} plus one \
             run of {run} between two of them"
        );
    }

    /// The delta executor reads estimates per job: they must be upper bounds, and zero
    /// only where nothing matches (a zero skips the job), and cost an index probe, not a
    /// count of the matches.
    #[test]
    fn stack_estimates_are_bounds_and_zero_only_without_a_match() {
        let stack = stack((0..50u64).flat_map(|s| [[s, 1, s + 100], [s, 2, 7]]))
            .successor(&[], &[[0, 1, 100]])
            .unwrap();
        let patterns = [
            [None, None, None],
            [Some(3), None, None],
            [Some(3), Some(1), None],
            [Some(3), Some(1), Some(103)],
            [Some(3), Some(1), Some(104)],
            [None, Some(2), Some(7)],
            [None, Some(2), Some(8)],
            [None, None, Some(7)],
            [None, Some(9), None],
            [Some(0), Some(1), None],
        ];
        for pattern in patterns {
            let matches = stack.scan(pattern).count();
            let estimate = estimate(&stack, pattern, &|_| true).unwrap();
            assert!(estimate >= matches, "{pattern:?}: {estimate} < {matches}");
            assert_eq!(estimate == 0, matches == 0, "{pattern:?}");
        }
        // Bounded by the predicate's facts, not the stack's.
        assert_eq!(
            estimate(&stack, [None, Some(2), Some(7)], &|_| true).unwrap(),
            50
        );
        assert_eq!(
            estimate(&stack, [Some(3), Some(1), None], &|_| true).unwrap(),
            49
        );
    }
}
