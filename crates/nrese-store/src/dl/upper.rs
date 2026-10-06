//! The upper bound U1 in a stack of its own (docs/design/owl2-dl.md §8, package 4.1).
//!
//! U1 is PAGOdA's datalog strengthening of the asserted ontology, compiled by
//! `nrese_dl::bounds` (disjunctions split, existentials c-Skolemised, `⊥` neutralised to
//! a clash fact). It is evaluated **over L ∪ data**: its input is the store's
//! materialised view (asserted and inferred statements, every graph) plus the program's
//! own facts, so U1 ⊇ L by construction. Only what it derives beyond that input is kept
//! here, in [`Stack`]: never in the engine's inferred stack, so never visible as inferred
//! data. Queries read it through a snapshot of their own ([`super::query`]).
//!
//! **Maintained per commit** by the reasoner's delta executor (`nrese_reasoner::delta`),
//! an ordinary incremental datalog materialisation: [`Upper::maintain`] takes the
//! commit's changes to the materialised view (asserted and inferred) as the change of
//! U1's input. A commit that changes the ontology's schema (anything but assertions over
//! the signature U1 was compiled for) recompiles U1 and evaluates it afresh
//! ([`is_assertion`]): its rules are the TBox's.

use std::collections::{BTreeSet, HashSet};
use std::time::{Duration, Instant};

use nrese_dl::bounds;
use nrese_engine::{GraphSelector, QuadPattern, ReadModel, Snapshot, TermId};
use nrese_owl::{Normalised, Ontology};
use nrese_rdf::{LiteralRef, NamedNodeRef, TermRef};
use nrese_reasoner::delta::{self, Base};
use nrese_reasoner::eval::{GroundProgram, Schema, Stop};
use nrese_reasoner::ir::{self, Triple};

/// U1's own facts beyond its input, indexed for every bound pattern.
#[derive(Debug, Default, Clone)]
pub struct Stack {
    spo: BTreeSet<[u64; 3]>,
    pos: BTreeSet<[u64; 3]>,
    osp: BTreeSet<[u64; 3]>,
    /// Facts per predicate, for the delta executor's estimates.
    per_predicate: std::collections::HashMap<u64, usize>,
}

impl Stack {
    pub fn len(&self) -> usize {
        self.spo.len()
    }

    pub fn is_empty(&self) -> bool {
        self.spo.is_empty()
    }

    pub fn contains(&self, t: &Triple) -> bool {
        self.spo.contains(t)
    }

    pub fn insert(&mut self, [s, p, o]: Triple) -> bool {
        if !self.spo.insert([s, p, o]) {
            return false;
        }
        self.pos.insert([p, o, s]);
        self.osp.insert([o, s, p]);
        *self.per_predicate.entry(p).or_default() += 1;
        true
    }

    pub fn remove(&mut self, [s, p, o]: Triple) -> bool {
        if !self.spo.remove(&[s, p, o]) {
            return false;
        }
        self.pos.remove(&[p, o, s]);
        self.osp.remove(&[o, s, p]);
        if let Some(n) = self.per_predicate.get_mut(&p) {
            *n -= 1;
        }
        true
    }

    pub fn iter(&self) -> impl Iterator<Item = Triple> + '_ {
        self.spo.iter().copied()
    }

    /// An upper bound on the facts matching `pattern`, zero only if none does. O(log n).
    pub fn estimate(&self, pattern: [Option<u64>; 3]) -> usize {
        let bound = match pattern[1] {
            Some(p) => self.per_predicate.get(&p).copied().unwrap_or(0),
            None => self.len(),
        };
        if bound == 0 {
            return 0;
        }
        let mut any = false;
        self.first(pattern, &mut any);
        if any { bound } else { 0 }
    }

    /// Whether a fact matches `pattern`, by the index whose prefix it binds.
    fn first(&self, pattern: [Option<u64>; 3], any: &mut bool) {
        let hit = |set: &BTreeSet<[u64; 3]>, prefix: &[u64], test: &dyn Fn(&[u64; 3]) -> bool| {
            let mut low = [0u64; 3];
            let mut high = [u64::MAX; 3];
            low[..prefix.len()].copy_from_slice(prefix);
            high[..prefix.len()].copy_from_slice(prefix);
            set.range(low..=high).any(test)
        };
        *any = match pattern {
            [Some(s), Some(p), o] => hit(&self.spo, &[s, p], &|t| o.is_none_or(|o| t[2] == o)),
            [Some(s), None, o] => hit(&self.spo, &[s], &|t| o.is_none_or(|o| t[2] == o)),
            [None, Some(p), Some(o)] => hit(&self.pos, &[p, o], &|_| true),
            [None, Some(p), None] => hit(&self.pos, &[p], &|_| true),
            [None, None, Some(o)] => hit(&self.osp, &[o], &|_| true),
            [None, None, None] => !self.is_empty(),
        };
    }

    /// The facts matching `pattern`, by the index whose prefix it binds. O(log n + k).
    pub fn scan(&self, pattern: [Option<u64>; 3], f: &mut dyn FnMut(Triple)) {
        let range = |set: &BTreeSet<[u64; 3]>, prefix: &[u64]| {
            let mut low = [0u64; 3];
            let mut high = [u64::MAX; 3];
            low[..prefix.len()].copy_from_slice(prefix);
            high[..prefix.len()].copy_from_slice(prefix);
            set.range(low..=high).copied().collect::<Vec<_>>()
        };
        let matches = |t: &Triple| {
            pattern
                .iter()
                .zip(t)
                .all(|(bound, value)| bound.is_none_or(|b| b == *value))
        };
        let found: Vec<Triple> = match pattern {
            [Some(s), Some(p), _] => range(&self.spo, &[s, p]),
            [Some(s), None, _] => range(&self.spo, &[s]),
            [None, Some(p), Some(o)] => range(&self.pos, &[p, o])
                .into_iter()
                .map(|[p, o, s]| [s, p, o])
                .collect(),
            [None, Some(p), None] => range(&self.pos, &[p])
                .into_iter()
                .map(|[p, o, s]| [s, p, o])
                .collect(),
            [None, None, Some(o)] => range(&self.osp, &[o])
                .into_iter()
                .map(|[o, s, p]| [s, p, o])
                .collect(),
            [None, None, None] => self.spo.iter().copied().collect(),
        };
        for t in found.into_iter().filter(matches) {
            f(t);
        }
    }
}

/// A pattern over every graph.
fn pattern([s, p, o]: [Option<u64>; 3]) -> QuadPattern {
    QuadPattern {
        subject: s.map(TermId::from_raw),
        predicate: p.map(TermId::from_raw),
        object: o.map(TermId::from_raw),
        graph: GraphSelector::Any,
    }
}

/// U1's input and its stack as the delta executor reads them: the store's materialised
/// view after the change and the program's facts count as asserted, the stack as
/// inferred.
struct UpperBase<'a> {
    view: &'a Snapshot,
    facts: &'a HashSet<Triple>,
    stack: &'a Stack,
}

impl UpperBase<'_> {
    fn in_view(&self, [s, p, o]: Triple) -> bool {
        self.view
            .quads_for_pattern_in(
                ReadModel::Materialised,
                &pattern([Some(s), Some(p), Some(o)]),
            )
            .next()
            .is_some()
    }
}

impl Base for UpperBase<'_> {
    fn scan(&self, bound: [Option<u64>; 3], f: &mut dyn FnMut(Triple)) {
        for q in self
            .view
            .quads_for_pattern_in(ReadModel::Materialised, &pattern(bound))
        {
            f([q.subject.raw(), q.predicate.raw(), q.object.raw()]);
        }
        for t in self.facts {
            if bound.iter().zip(t).all(|(b, v)| b.is_none_or(|b| b == *v)) {
                f(*t);
            }
        }
        self.stack.scan(bound, f);
    }

    fn estimate(&self, bound: [Option<u64>; 3]) -> usize {
        let view = self.view.count_in(ReadModel::Materialised, &pattern(bound));
        usize::try_from(view).unwrap_or(usize::MAX) + self.stack.estimate(bound) + self.facts.len()
    }

    fn contains(&self, fact: Triple) -> bool {
        self.stack.contains(&fact) || self.is_asserted(fact)
    }

    fn is_asserted(&self, fact: Triple) -> bool {
        self.facts.contains(&fact) || self.in_view(fact)
    }
}

/// Interns IRIs and literals into the store's dictionary (U1's own terms: fresh classes,
/// Skolem constants, the clash).
pub(crate) struct Interner<'a>(pub &'a dyn Fn(TermRef<'_>) -> TermId);

impl ir::Vocabulary for Interner<'_> {
    fn iri(&mut self, iri: &str) -> u64 {
        (self.0)(NamedNodeRef::new_unchecked(iri).into()).raw()
    }

    fn literal(&mut self, lexical: &str, datatype: &str) -> u64 {
        (self.0)(
            LiteralRef::new_typed_literal(lexical, NamedNodeRef::new_unchecked(datatype)).into(),
        )
        .raw()
    }

    fn language_literal(&mut self, lexical: &str, language: &str) -> u64 {
        (self.0)(LiteralRef::new_language_tagged_literal_unchecked(lexical, language).into()).raw()
    }
}

/// A commit's change to the stack ([`Upper::change`]).
#[derive(Default)]
pub struct Change {
    moved: Vec<Triple>,
    insert: Vec<Triple>,
    remove: Vec<Triple>,
    program: Option<GroundProgram>,
    pub elapsed: Duration,
}

/// Why U1 isn't available: its evaluation ran out of its budget.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GaveUp(pub String);

/// U1: the compiled program, its rules as the reasoner's, and its stack.
pub struct Upper {
    pub program: bounds::Program,
    rules: Vec<ir::Rule>,
    schema: Schema,
    /// The program's facts.
    facts: HashSet<Triple>,
    pub stack: Stack,
    /// The classes and properties U1 was compiled for: assertions over others change it.
    signature: HashSet<u64>,
    /// The ground program of the delta executor, for the next commit.
    ground: Option<GroundProgram>,
    rdf_type: u64,
    same_as: u64,
    /// How long the last evaluation or maintenance took.
    pub elapsed: Duration,
}

impl std::fmt::Debug for Upper {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Upper")
            .field("rules", &self.rules.len())
            .field("facts", &self.facts.len())
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

/// U1's rules as the reasoner's (the atoms copied one to one).
fn reasoner_rules(program: &bounds::Program) -> Vec<ir::Rule> {
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
fn view_facts(view: &Snapshot) -> Vec<Triple> {
    let mut facts: Vec<Triple> = view
        .quads_for_pattern_in(ReadModel::Materialised, &pattern([None, None, None]))
        .map(|q| [q.subject.raw(), q.predicate.raw(), q.object.raw()])
        .collect();
    facts.sort_unstable();
    facts.dedup();
    facts
}

impl Upper {
    /// Compiles U1 for `ontology` and evaluates it over `view` (the materialised view the
    /// ontology was read from). `intern` adds U1's own terms to the dictionary; `stop`
    /// ends the evaluation (U1 is then not available). O(the closure).
    pub fn build(
        ontology: &Ontology,
        normalised: &Normalised,
        view: &Snapshot,
        intern: &dyn Fn(TermRef<'_>) -> TermId,
        stop: Stop<'_>,
    ) -> Result<Self, GaveUp> {
        let started = Instant::now();
        let mut vocabulary = Interner(intern);
        let program = bounds::compile(ontology, normalised, &mut |iri| {
            ir::Vocabulary::iri(&mut vocabulary, iri)
        });
        let schema = Schema::owl(&mut vocabulary);
        let rules = reasoner_rules(&program);
        let facts: HashSet<Triple> = program.facts.iter().map(|(f, _)| *f).collect();
        let mut input = view_facts(view);
        input.extend(facts.iter().copied());
        input.sort_unstable();
        input.dedup();
        let m = nrese_reasoner::batch::materialise_owned_until(input, &rules, None, &schema, stop)
            .map_err(|_| GaveUp("the upper bound's evaluation was stopped".to_owned()))?;
        let mut stack = Stack::default();
        for t in m.derived {
            stack.insert(t);
        }
        let s = &program.signature;
        let signature = s
            .classes
            .iter()
            .chain(&s.object_properties)
            .chain(&s.data_properties)
            .copied()
            .collect();
        Ok(Self {
            rdf_type: program.names.rdf_type,
            same_as: program.names.same_as,
            program,
            rules,
            schema,
            facts,
            stack,
            signature,
            ground: None,
            elapsed: started.elapsed(),
        })
    }

    /// Whether a changed statement only asserts something about individuals over the
    /// signature U1 was compiled for, so U1's rules stay as they are: a class membership
    /// in a known class, a known property between named individuals (or with a literal),
    /// or an equality. Anything else (schema, blank nodes, OWL's vocabulary, new
    /// vocabulary) means recompiling.
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
        self.signature.contains(&p) && (named(o) || is_literal(o))
    }

    /// The stack's change for a commit, computed before it and applied after it
    /// ([`Self::apply`]), so a commit that doesn't happen leaves U1 as it was: `view` is
    /// the materialised view after the commit, `inserted` the facts new to the view,
    /// `deleted` those gone from it (each checked here against the view and the
    /// program's facts). Cost: the change's.
    pub fn change(
        &self,
        view: &Snapshot,
        inserted: &[Triple],
        deleted: &[Triple],
        stop: Stop<'_>,
    ) -> Result<Change, GaveUp> {
        let started = Instant::now();
        let mut inserted: Vec<Triple> = inserted
            .iter()
            .filter(|t| !self.facts.contains(*t))
            .copied()
            .collect();
        inserted.sort_unstable();
        inserted.dedup();
        // A fact U1 derived that is now in its input: it leaves the stack, and its
        // consequences are already there.
        let moved: Vec<Triple> = inserted
            .iter()
            .filter(|t| self.stack.contains(t))
            .copied()
            .collect();
        inserted.retain(|t| !self.stack.contains(t));
        let mut deleted: Vec<Triple> = deleted
            .iter()
            .filter(|t| !self.facts.contains(*t))
            .copied()
            .collect();
        deleted.sort_unstable();
        deleted.dedup();
        let base = UpperBase {
            view,
            facts: &self.facts,
            stack: &self.stack,
        };
        deleted.retain(|t| !base.in_view(*t));
        let mut change = Change {
            moved,
            ..Change::default()
        };
        if inserted.is_empty() && deleted.is_empty() {
            return Ok(change);
        }
        let rules = delta::Rules {
            rules: &self.rules,
            lists: None,
            schema: &self.schema,
        };
        let update = delta::update_until(
            &base,
            &inserted,
            &deleted,
            rules,
            self.ground.as_ref(),
            stop,
        )
        .map_err(|_| GaveUp("the upper bound's maintenance was stopped".to_owned()))?;
        change.program = update.program;
        change.remove = update.remove;
        change.insert = update
            .insert
            .into_iter()
            .filter(|t| !base_has(view, &self.facts, *t))
            .collect();
        change.elapsed = started.elapsed();
        Ok(change)
    }

    /// Applies a commit's change ([`Self::change`]) once the commit is done.
    pub fn apply(&mut self, change: Change) {
        for t in change.moved.into_iter().chain(change.remove) {
            self.stack.remove(t);
        }
        for t in change.insert {
            self.stack.insert(t);
        }
        if let Some(program) = change.program {
            self.ground = Some(program);
        }
        self.elapsed = change.elapsed;
    }

    /// The clashes the stack would have after `change`.
    pub fn clashes_after(&self, change: &Change) -> usize {
        let clash = self.program.names.clash;
        let is_clash = |t: &Triple| t[1] == clash && t[2] == clash;
        let gone = change
            .moved
            .iter()
            .chain(&change.remove)
            .filter(|t| is_clash(t) && self.stack.contains(t))
            .count();
        let new = change
            .insert
            .iter()
            .filter(|t| is_clash(t) && !self.stack.contains(t))
            .count();
        (self.clashes() + new).saturating_sub(gone)
    }

    /// The terms a clash was derived at (`⊥` fired in U1): the ontology may be
    /// inconsistent.
    pub fn clashes(&self) -> usize {
        let clash = self.program.names.clash;
        let mut n = 0;
        self.stack
            .scan([None, Some(clash), Some(clash)], &mut |_| n += 1);
        n
    }

    /// Whether U1 proves the ontology consistent (PAGOdA, Theorem 5.5 (i)): no clash, and
    /// U1 checks every `⊥` ([`bounds::Program::proves_consistency`]).
    pub fn proves_consistency(&self) -> bool {
        self.program.proves_consistency() && self.clashes() == 0
    }

    /// Whether `term` is one of U1's own (a Skolem constant, a fresh class, the clash).
    pub fn is_internal(&self, term: u64) -> bool {
        self.program.is_internal(term)
    }
}

/// Whether a term is a literal (of any of the engine's literal kinds).
fn is_literal(t: u64) -> bool {
    use nrese_engine::TermKind;
    !matches!(
        TermId::from_raw(t).kind(),
        TermKind::Iri | TermKind::BlankNode | TermKind::DefaultGraph
    )
}

fn base_has(view: &Snapshot, facts: &HashSet<Triple>, [s, p, o]: Triple) -> bool {
    facts.contains(&[s, p, o])
        || view
            .quads_for_pattern_in(
                ReadModel::Materialised,
                &pattern([Some(s), Some(p), Some(o)]),
            )
            .next()
            .is_some()
}

#[cfg(test)]
mod tests {
    use super::Stack;

    /// The delta executor reads estimates per job: they must be upper bounds, and zero
    /// only where nothing matches (a zero skips the job), and cost an index probe, not a
    /// count of the matches.
    #[test]
    fn stack_estimates_are_bounds_and_zero_only_without_a_match() {
        let mut stack = Stack::default();
        for s in 0..50u64 {
            stack.insert([s, 1, s + 100]);
            stack.insert([s, 2, 7]);
        }
        stack.remove([0, 1, 100]);
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
            let mut matches = 0;
            stack.scan(pattern, &mut |_| matches += 1);
            let estimate = stack.estimate(pattern);
            assert!(estimate >= matches, "{pattern:?}: {estimate} < {matches}");
            assert_eq!(estimate == 0, matches == 0, "{pattern:?}");
        }
        // Bounded by the predicate's facts, not the stack's.
        assert_eq!(stack.estimate([None, Some(2), Some(7)]), 50);
        assert_eq!(stack.estimate([Some(3), Some(1), None]), 49);
    }
}
