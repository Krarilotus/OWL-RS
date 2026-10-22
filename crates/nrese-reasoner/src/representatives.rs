//! Equality by representatives (work package W4, stage A).
//!
//! OWL 2 RL's equality rules (`eq-rep-s/p/o`) make every fact true of every identity of
//! its terms: a class of k identities turns one fact into up to k³ copies, and the store,
//! the joins and the answers carry all of them. Here each `owl:sameAs` class has one
//! representative (its smallest id), every fact is rewritten to representatives, and the
//! rules run on the rewritten facts without the replacement rules.
//!
//! The batch executor does it within its semi-naive rounds (egglog's rebuild; Zhang et
//! al., POPL 2023): a round's new `sameAs` between two representatives merges their
//! classes, and only the facts that mention the representative that lost its place are
//! rewritten, into the next round's delta ([`super::batch`]). So equality costs one
//! materialisation plus the facts its merges touch, however long a cascade of merges is;
//! before 6 October 2026 every merge re-materialised the whole closure, until a pass
//! merged nothing.
//!
//! The replicated closure is the representative closure expanded: a fact holds iff the
//! fact of its terms' representatives is in the representative closure
//! ([`EqualityClasses::expand`]). The property test checks exactly that, and that the
//! same consistency rules fire.
//!
//! A rule's guards compare terms (N3's `log:notEqualTo`, PIE's constraints), and a value
//! over representatives stands for every identity of its class: a guard holds if some
//! choice of identities satisfies the rule's guards ([`guards_hold_over`]), as one binding
//! of the replicated closure does. Compared over representatives, `?x log:notEqualTo ?y`
//! over two identities of one class read `x != x` and lost the heads replication derives.

use std::sync::{Arc, PoisonError, RwLock};

use super::batch::{self, Schema};
use super::delta::Interrupted;
use super::eval::Poller;
use super::ir::{Atom, Guard, Head, Rule, SharedListIndex, Term};
use super::ir::{Triple, Violations};
use super::lists::ListVocabulary;

/// The `owl:sameAs` classes of a closure: each term's representative, and each
/// representative's members (only for classes of two or more). The union kernel the
/// engine shares ([`nrese_exec::classes`]).
pub use nrese_exec::classes::Classes as EqualityClasses;

/// What a guard's term stands for over representatives.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Bound {
    /// This term: a constant of the rule.
    Term(u64),
    /// Any identity of the class of this id: a variable's value.
    Class(u64),
}

impl Bound {
    fn id(self) -> u64 {
        match self {
            Bound::Term(id) | Bound::Class(id) => id,
        }
    }
}

/// A check over original Term slots. Static structure never contains class choices.
#[derive(Debug)]
enum Check {
    Differ(usize, usize),
    Outside(usize, u64, u64),
    Listed(usize, usize, SharedListIndex),
}

impl Check {
    fn slots(&self) -> (usize, Option<usize>) {
        match *self {
            Self::Differ(a, b) | Self::Listed(a, b, _) => (a, Some(b)),
            Self::Outside(a, ..) => (a, None),
        }
    }

    fn active(&self, bound: &[Option<Bound>]) -> bool {
        let (a, b) = self.slots();
        bound[a].is_some() && b.is_none_or(|b| bound[b].is_some())
    }

    fn holds(&self, chosen: &[u64]) -> bool {
        let at = |i: usize| chosen.get(i).copied();
        match self {
            Self::Differ(a, b) => match (at(*a), at(*b)) {
                (Some(a), Some(b)) => a != b,
                _ => true,
            },
            Self::Outside(a, low, high) => at(*a).is_none_or(|v| v < *low || v > *high),
            Self::Listed(a, b, index) => match (at(*a), at(*b)) {
                (Some(a), Some(b)) => a < b && index.together(a, b),
                _ => true,
            },
        }
    }
}

/// Prepared once at the original ClassGuards owner; grounded copies share it.
/// Original guards remain the semantic identity. Physical slots/incidence do not.
#[derive(Debug)]
struct ClassGuardPlan {
    guards: Box<[Guard]>,
    slots: Box<[Term]>,
    checks: Box<[Check]>,
    offsets: Box<[usize]>,
    incidence: Box<[usize]>,
    limits: Box<[usize]>,
    candidate_capacity: usize,
}

impl ClassGuardPlan {
    fn prepare_polled(guards: Vec<Guard>, poll: &mut Poller<'_>) -> Result<Self, Interrupted> {
        let mut slots = Vec::new();
        let mut checks = Vec::with_capacity(guards.len());
        let mut slot = |term: Term| -> Result<usize, Interrupted> {
            poll.work()?;
            for (i, &old) in slots.iter().enumerate() {
                poll.work()?;
                if old == term {
                    return Ok(i);
                }
            }
            slots.push(term);
            Ok(slots.len() - 1)
        };
        for guard in &guards {
            let check = match guard {
                Guard::NotEqual(a, b) => Check::Differ(slot(*a)?, slot(*b)?),
                Guard::NotIn(t, lo, hi) => Check::Outside(slot(*t)?, *lo, *hi),
                Guard::SameList(a, b, index) => Check::Listed(slot(*a)?, slot(*b)?, index.clone()),
                Guard::Classes(_) => unreachable!("class guards aren't nested"),
            };
            checks.push(check);
        }
        // Standard contiguous incidence lists. This is cold preparation; every scan
        // participates in the operation's interruption contract.
        let mut offsets = Vec::with_capacity(slots.len() + 1);
        let mut incidence = Vec::with_capacity(checks.len() * 2);
        let mut limits = Vec::with_capacity(slots.len());
        for (i, _) in slots.iter().enumerate() {
            poll.work()?;
            offsets.push(incidence.len());
            let mut differs = 0usize;
            let mut listed = false;
            for (j, check) in checks.iter().enumerate() {
                poll.work()?;
                let (a, b) = check.slots();
                if a == i || b == Some(i) {
                    incidence.push(j);
                    match check {
                        Check::Differ(..) => differs += 1,
                        Check::Listed(..) => listed = true,
                        _ => {}
                    }
                }
            }
            limits.push(if listed { usize::MAX } else { differs + 1 });
        }
        offsets.push(incidence.len());
        let candidate_capacity = slots
            .iter()
            .zip(&limits)
            .map(|(term, &n)| {
                if matches!(term, Term::Const(_)) {
                    1
                } else if n != usize::MAX {
                    n
                } else {
                    0
                }
            })
            .sum();
        Ok(Self {
            guards: guards.into_boxed_slice(),
            slots: slots.into_boxed_slice(),
            checks: checks.into_boxed_slice(),
            offsets: offsets.into_boxed_slice(),
            incidence: incidence.into_boxed_slice(),
            limits: limits.into_boxed_slice(),
            candidate_capacity,
        })
    }

    fn incident(&self, slot: usize) -> &[usize] {
        &self.incidence[self.offsets[slot]..self.offsets[slot + 1]]
    }

    fn holds_polled(
        &self,
        value: impl Fn(Term) -> Option<Bound>,
        classes: &EqualityClasses,
        poll: &mut Poller<'_>,
        scratch: &mut GuardWorkspace,
    ) -> Result<bool, Interrupted> {
        scratch.bound.clear();
        scratch.rows.clear();
        scratch.candidates.clear();
        scratch.chosen.clear();
        for &term in &self.slots {
            poll.work()?;
            scratch.bound.push(value(term));
        }
        for bound in &scratch.bound {
            poll.work()?;
            scratch.chosen.push(bound.map_or(0, Bound::id));
        }
        let mut direct = true;
        for check in &self.checks {
            poll.work()?;
            if check.active(&scratch.bound) && !check.holds(&scratch.chosen) {
                direct = false;
                break;
            }
        }
        if direct {
            return Ok(true);
        }
        scratch.chosen.clear();
        for (i, bound) in scratch.bound.iter().copied().enumerate() {
            poll.work()?;
            let start = scratch.candidates.len();
            match bound {
                None => scratch.candidates.push(0), // inactive checks defer this Term.
                Some(Bound::Term(id)) => scratch.candidates.push(id),
                Some(Bound::Class(id)) => {
                    let members = classes.class_of(id).unwrap_or(std::slice::from_ref(&id));
                    'members: for &member in members {
                        poll.work()?;
                        for &j in self.incident(i) {
                            poll.work()?;
                            let check = &self.checks[j];
                            if !check.active(&scratch.bound) {
                                continue;
                            }
                            match check {
                                Check::Outside(_, lo, hi) if (*lo..=*hi).contains(&member) => {
                                    continue 'members;
                                }
                                Check::Listed(_, _, index) if !index.contains(member) => {
                                    continue 'members;
                                }
                                _ => {}
                            }
                        }
                        scratch.candidates.push(member);
                        if scratch.candidates.len() - start == self.limits[i] {
                            break;
                        }
                    }
                }
            }
            scratch.rows.push(start..scratch.candidates.len());
        }
        self.search(scratch, poll)
    }

    fn search(
        &self,
        scratch: &mut GuardWorkspace,
        poll: &mut Poller<'_>,
    ) -> Result<bool, Interrupted> {
        let slot = scratch.chosen.len();
        let Some(row) = scratch.rows.get(slot).cloned() else {
            return Ok(true);
        };
        for position in row {
            poll.work()?;
            scratch.chosen.push(scratch.candidates[position]);
            let mut valid = true;
            for &j in self.incident(slot) {
                poll.work()?;
                let check = &self.checks[j];
                if check.active(&scratch.bound) && !check.holds(&scratch.chosen) {
                    valid = false;
                    break;
                }
            }
            if valid && self.search(scratch, poll)? {
                return Ok(true);
            }
            scratch.chosen.pop();
        }
        Ok(false)
    }
}

/// One sequential workspace per affected task/grounding/backward operation. It never
/// stores class choices between checks and is absent from rule Eq/Hash/fingerprints.
#[derive(Default)]
pub(crate) struct GuardWorkspace {
    bound: Vec<Option<Bound>>,
    rows: Vec<std::ops::Range<usize>>,
    candidates: Vec<u64>,
    chosen: Vec<u64>,
    // Maximum static dimensions of this task's plans, not class assignments.
    planned: (usize, usize),
}

impl GuardWorkspace {
    fn clear_binding(&mut self) {
        self.bound.clear();
        self.rows.clear();
        self.candidates.clear();
        self.chosen.clear();
    }
    fn reserve_plan(
        &mut self,
        plan: &ClassGuardPlan,
        poll: &mut Poller<'_>,
    ) -> Result<(), Interrupted> {
        poll.work()?;
        // Lists use the same kernel but have no static candidate bound. ClassGuards
        // plans contain term guards only, so task-local term search is fully reserved.
        self.planned.0 = self.planned.0.max(plan.slots.len());
        self.planned.1 = self.planned.1.max(plan.candidate_capacity);
        self.initialize(poll)
    }

    fn covers(&self, plan: &ClassGuardPlan) -> bool {
        self.bound.capacity() >= plan.slots.len()
            && self.candidates.capacity() >= plan.candidate_capacity
    }

    #[cold]
    #[inline(never)]
    fn initialize(&mut self, poll: &mut Poller<'_>) -> Result<(), Interrupted> {
        // Opaque heap requests use the SAME poller, including immediate admission.
        // Nothing reaches this helper on the representative-only success path.
        let (slots, candidates) = self.planned;
        poll.check()?;
        self.bound
            .reserve_exact(slots.saturating_sub(self.bound.len()));
        poll.check()?;
        self.rows
            .reserve_exact(slots.saturating_sub(self.rows.len()));
        poll.check()?;
        self.chosen
            .reserve_exact(slots.saturating_sub(self.chosen.len()));
        poll.check()?;
        self.candidates
            .reserve_exact(candidates.saturating_sub(self.candidates.len()));
        poll.check()?;
        Ok(())
    }

    fn dimensions(
        guards: &[Guard],
        poll: &mut Poller<'_>,
        slots: &mut usize,
        candidates: &mut usize,
    ) -> Result<(), Interrupted> {
        for guard in guards {
            poll.work()?;
            if let Guard::Classes(g) = guard {
                *slots = (*slots).max(g.plan.slots.len());
                *candidates = (*candidates).max(g.plan.candidate_capacity);
            }
        }
        Ok(())
    }

    pub(crate) fn prepare_rule(
        &mut self,
        rule: &Rule,
        poll: &mut Poller<'_>,
    ) -> Result<(), Interrupted> {
        let (mut slots, mut candidates) = (0, 0);
        Self::dimensions(&rule.guards, poll, &mut slots, &mut candidates)?;
        if let Head::Grouped(groups) = &rule.head {
            for group in groups.iter() {
                Self::dimensions(&group.guards, poll, &mut slots, &mut candidates)?;
            }
        }
        self.planned.0 = self.planned.0.max(slots);
        self.planned.1 = self.planned.1.max(candidates);
        Ok(())
    }
}

pub fn guards_hold_over(
    guards: &[Guard],
    value: impl Fn(Term) -> Option<Bound>,
    classes: &EqualityClasses,
) -> bool {
    guards_hold_over_polled(guards, value, classes, &mut Poller::never())
        .expect("uncancelled guard check")
}

pub(crate) fn guards_hold_over_polled(
    guards: &[Guard],
    value: impl Fn(Term) -> Option<Bound>,
    classes: &EqualityClasses,
    poll: &mut Poller<'_>,
) -> Result<bool, Interrupted> {
    let plan = ClassGuardPlan::prepare_polled(guards.to_vec(), poll)?;
    let mut scratch = GuardWorkspace::default();
    scratch.reserve_plan(&plan, poll)?;
    plan.holds_polled(value, classes, poll, &mut scratch)
}

/// The classes a materialisation has merged so far, shared with the guards that read them
/// ([`ClassGuards`]): replaced between its rounds, never while rules are evaluated.
pub type SharedClasses = Arc<RwLock<Arc<EqualityClasses>>>;

/// A rule's term guards (`!=`, `NotIn`) as equality by representatives decides them
/// ([`guards_hold_over`]): its variables stand for classes, its constants for themselves.
/// Compared by the guards and the values grounding gave them, not by the classes.
#[derive(Debug, Clone)]
pub struct ClassGuards {
    plan: Arc<ClassGuardPlan>,
    /// The values grounding gave the guards' schema variables (no longer in the body).
    grounded: Vec<(u8, u64)>,
    classes: SharedClasses,
}

impl ClassGuards {
    /// Whether some choice of identities satisfies the guards (`value`: a variable's value,
    /// a representative; `None` while unbound). The representatives are checked first
    /// without the classes; only a binding they fail reads them.
    pub fn holds(&self, value: &dyn Fn(Term) -> Option<u64>) -> bool {
        self.holds_polled(value, &mut Poller::never())
            .expect("the uncancelled guard check runs to completion")
    }

    pub(crate) fn holds_polled(
        &self,
        value: &dyn Fn(Term) -> Option<u64>,
        poll: &mut Poller<'_>,
    ) -> Result<bool, Interrupted> {
        self.holds_prepared_polled(value, poll, &mut GuardWorkspace::default())
    }

    pub(crate) fn holds_prepared_polled(
        &self,
        value: &dyn Fn(Term) -> Option<u64>,
        poll: &mut Poller<'_>,
        scratch: &mut GuardWorkspace,
    ) -> Result<bool, Interrupted> {
        let bound = |term| self.bound(term, value);
        let mut direct = true;
        for guard in self.plan.guards.iter() {
            poll.work()?;
            if !guard.holds(|term| bound(term).map(Bound::id)) {
                direct = false;
                break;
            }
        }
        if direct {
            return Ok(true);
        }
        self.fallback_polled(value, poll, scratch)
    }

    #[inline]
    fn bound(&self, term: Term, value: &dyn Fn(Term) -> Option<u64>) -> Option<Bound> {
        match term {
            Term::Const(id) => Some(Bound::Term(id)),
            Term::Var(v) => self
                .grounded
                .iter()
                .find(|&&(g, _)| g == v)
                .map(|&(_, id)| id)
                .or_else(|| value(term))
                .map(Bound::Class),
        }
    }

    #[cold]
    #[inline(never)]
    fn fallback_polled(
        &self,
        value: &dyn Fn(Term) -> Option<u64>,
        poll: &mut Poller<'_>,
        scratch: &mut GuardWorkspace,
    ) -> Result<bool, Interrupted> {
        let classes = self.classes.read().unwrap_or_else(PoisonError::into_inner);
        if classes.is_empty() {
            return Ok(false);
        }
        if !scratch.covers(&self.plan) {
            scratch.reserve_plan(&self.plan, poll)?;
        }
        let result =
            self.plan
                .holds_polled(|term| self.bound(term, value), &classes, poll, scratch);
        scratch.clear_binding();
        result
    }

    /// The guards with the schema variables' values of `substitution` (grounding).
    pub fn ground(&self, substitution: &[Option<u64>]) -> Self {
        self.ground_polled(substitution, &mut Poller::never())
            .expect("uncancelled guard grounding")
    }

    pub(crate) fn ground_polled(
        &self,
        substitution: &[Option<u64>],
        poll: &mut Poller<'_>,
    ) -> Result<Self, Interrupted> {
        let mut grounded = self.grounded.clone();
        for &term in self.plan.slots.iter() {
            poll.work()?;
            if let Term::Var(v) = term
                && let Some(Some(id)) = substitution.get(usize::from(v))
                && !grounded.iter().any(|&(g, _)| g == v)
            {
                grounded.push((v, *id));
            }
        }
        Ok(Self {
            grounded,
            ..self.clone()
        })
    }
}

impl PartialEq for ClassGuards {
    fn eq(&self, other: &Self) -> bool {
        self.plan.guards == other.plan.guards && self.grounded == other.grounded
    }
}

impl Eq for ClassGuards {}

impl std::hash::Hash for ClassGuards {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        (&self.plan.guards, &self.grounded).hash(state);
    }
}

/// `rule` as equality by representatives evaluates it: its term guards (`!=`, `NotIn`)
/// decided over `classes` ([`ClassGuards`]). Unchanged without term guards, and where a
/// guard failing over one class changes nothing: only `!=` guards, each on the pair every
/// head atom equates (`prp-key`'s `x != y` for `x sameAs y`: a class equated with itself).
/// List guards stay as they are: the lists are read over representatives, with their
/// positions ([`super::lists`]).
pub fn over_classes(rule: Rule, same_as: u64, classes: &SharedClasses) -> Rule {
    over_classes_polled(rule, same_as, classes, &mut Poller::never())
        .expect("uncancelled class preparation")
}

pub(crate) fn over_classes_polled(
    mut rule: Rule,
    same_as: u64,
    classes: &SharedClasses,
    poll: &mut Poller<'_>,
) -> Result<Rule, Interrupted> {
    if let Head::Grouped(groups) = &mut rule.head {
        let mut common_terms = Vec::new();
        let mut common_other = Vec::new();
        for guard in std::mem::take(&mut rule.guards) {
            poll.work()?;
            if matches!(guard, Guard::NotEqual(..) | Guard::NotIn(..)) {
                common_terms.push(guard);
            } else {
                common_other.push(guard);
            }
        }
        rule.guards = common_other;
        for group in groups.iter_mut() {
            poll.work()?;
            let mut effective = Vec::with_capacity(common_terms.len() + group.guards.len());
            for guard in &common_terms {
                poll.work()?;
                effective.push(guard.clone());
            }
            effective.append(&mut group.guards);
            group.guards = class_plan(effective, &group.heads, same_as, classes, poll)?;
        }
        rule.factor_head_guards();
        return Ok(rule);
    }
    rule.guards = class_plan(
        rule.guards,
        match &rule.head {
            Head::Facts(atoms) => atoms,
            _ => &[],
        },
        same_as,
        classes,
        poll,
    )?;
    Ok(rule)
}

fn class_plan(
    original: Vec<Guard>,
    heads: &[Atom],
    same_as: u64,
    classes: &SharedClasses,
    poll: &mut Poller<'_>,
) -> Result<Vec<Guard>, Interrupted> {
    let mut terms = Vec::new();
    let mut lists = Vec::new();
    for guard in &original {
        poll.work()?;
        if matches!(guard, Guard::NotEqual(..) | Guard::NotIn(..)) {
            terms.push(guard.clone());
        } else {
            lists.push(guard.clone());
        }
    }
    let equates = |atom: &Atom, guard: &Guard| match *guard {
        Guard::NotEqual(a, b) => {
            atom.0 == [a, Term::Const(same_as), b] || atom.0 == [b, Term::Const(same_as), a]
        }
        _ => false,
    };
    let mut reflexive = !heads.is_empty();
    'guards: for guard in &terms {
        for atom in heads {
            poll.work()?;
            if !equates(atom, guard) {
                reflexive = false;
                break 'guards;
            }
        }
    }
    if terms.is_empty() || reflexive {
        return Ok(original);
    }
    let mut guards = lists;
    guards.push(Guard::Classes(ClassGuards {
        plan: Arc::new(ClassGuardPlan::prepare_polled(terms, poll)?),
        grounded: Vec::new(),
        classes: Arc::clone(classes),
    }));
    Ok(guards)
}

/// A closure over representatives.
#[derive(Debug, Default)]
pub struct RepresentativeClosure {
    /// Every fact of the closure over representatives, the rewritten input included,
    /// sorted, without duplicates.
    pub facts: Vec<Triple>,
    pub classes: EqualityClasses,
    /// Violations over representatives, sorted.
    pub violations: Violations,
    pub diagnostics: Vec<super::lists::ListDiagnostic>,
    /// Rounds of the closure, and in how many of them classes merged.
    pub rounds: usize,
    pub merges: usize,
    /// The batch materialisations run: one (merges are rebuilt within its rounds).
    pub passes: usize,
    /// The times and work of every pass, summed (`passes` counts them).
    pub phases: batch::Phases,
}

/// The `owl:sameAs` id of rules that do equality reasoning (`eq-rep-s`), if they do.
pub fn same_as(rules: &[Rule]) -> Option<u64> {
    batch::same_as_of(rules)
}

/// The rules without the replacement rules, which rewriting stands in for.
pub fn without_replacement(rules: &[Rule]) -> Vec<Rule> {
    rules
        .iter()
        .filter(|r| !batch::is_replacement_rule(r))
        .cloned()
        .collect()
}

/// The closure of `input` under `rules` over representatives of `owl:sameAs` classes.
/// Rules without equality reasoning give the plain closure (no classes).
pub fn materialise(
    input: &[Triple],
    rules: &[Rule],
    lists: Option<&ListVocabulary>,
    schema: &Schema,
) -> RepresentativeClosure {
    materialise_until(input, rules, lists, schema, super::eval::NEVER).expect("never stopped")
}

/// [`materialise`], polling `stop` in every round.
pub fn materialise_until(
    input: &[Triple],
    rules: &[Rule],
    lists: Option<&ListVocabulary>,
    schema: &Schema,
    stop: super::eval::Stop<'_>,
) -> Result<RepresentativeClosure, super::delta::Interrupted> {
    let result = batch::materialise_representatives_until(
        batch::Input::Facts(input.to_vec()),
        rules,
        lists,
        schema,
        batch::Listing::Representatives,
        stop,
    )?;
    Ok(RepresentativeClosure {
        facts: result.derived,
        classes: result.classes,
        violations: result.violations,
        diagnostics: result.diagnostics,
        rounds: result.rounds,
        merges: result.merges,
        passes: 1,
        phases: result.phases,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};

    #[test]
    fn joint_plan_is_lazy_reused_and_reads_current_classes() {
        let mut classes = EqualityClasses::default();
        classes.union(1, 2);
        let shared = Arc::new(RwLock::new(Arc::new(classes)));
        let x = Term::Var(0);
        let y = Term::Var(1);
        let head = |p| Atom([x, Term::Const(p), y]);
        let original = Rule {
            name: "joint".into(),
            body: vec![Atom([x, Term::Const(7), y])],
            guards: vec![Guard::NotEqual(x, Term::Const(1))],
            head: Head::Grouped(
                vec![
                    super::super::ir::HeadGroup {
                        guards: vec![],
                        heads: vec![head(8)],
                    },
                    super::super::ir::HeadGroup {
                        guards: vec![Guard::NotEqual(x, Term::Const(2))],
                        heads: vec![head(9)],
                    },
                ]
                .into_boxed_slice(),
            ),
        };
        let rule = over_classes(original, 99, &shared);
        let Head::Grouped(groups) = &rule.head else {
            panic!("two joint conjunctions")
        };
        let get = |i: usize| match &groups[i].guards[0] {
            Guard::Classes(g) => g,
            _ => panic!("class plan"),
        };
        let mut scratch = GuardWorkspace::default();
        let mut poll = Poller::never();
        scratch.prepare_rule(&rule, &mut poll).unwrap();
        let capacities = |s: &GuardWorkspace| {
            (
                s.bound.capacity(),
                s.rows.capacity(),
                s.candidates.capacity(),
                s.chosen.capacity(),
            )
        };
        assert_eq!(capacities(&scratch), (0, 0, 0, 0));
        assert!(
            get(0)
                .holds_prepared_polled(&|_| Some(3), &mut poll, &mut scratch)
                .unwrap()
        );
        assert_eq!(capacities(&scratch), (0, 0, 0, 0));
        assert!(
            get(0)
                .holds_prepared_polled(&|_| Some(1), &mut poll, &mut scratch)
                .unwrap()
        );
        let reserved = capacities(&scratch);
        assert!(reserved.0 > 0);
        for _ in 0..32 {
            assert!(
                !get(1)
                    .holds_prepared_polled(&|_| Some(1), &mut poll, &mut scratch)
                    .unwrap()
            );
            assert_eq!(capacities(&scratch), reserved);
            assert!(
                scratch.bound.is_empty()
                    && scratch.rows.is_empty()
                    && scratch.candidates.is_empty()
                    && scratch.chosen.is_empty()
            );
        }
        let key = super::super::eval::rule_key(&rule);
        let mut grown = (**shared.read().unwrap()).clone();
        grown.union(1, 3);
        *shared.write().unwrap() = Arc::new(grown);
        assert!(
            get(1)
                .holds_prepared_polled(&|_| Some(1), &mut poll, &mut scratch)
                .unwrap()
        );
        assert_eq!(super::super::eval::rule_key(&rule), key);
        let stopped = AtomicBool::new(false);
        let stop = || stopped.load(Ordering::Relaxed);
        let mut poll = Poller::new(&stop).unwrap();
        let mut empty = GuardWorkspace::default();
        empty.prepare_rule(&rule, &mut poll).unwrap();
        stopped.store(true, Ordering::Relaxed);
        assert_eq!(
            get(0).holds_prepared_polled(&|_| Some(1), &mut poll, &mut empty),
            Err(Interrupted)
        );
        assert_eq!(capacities(&empty), (0, 0, 0, 0));
        assert!(poll.stopped());
        assert!(shared.try_write().is_ok());
    }
}
