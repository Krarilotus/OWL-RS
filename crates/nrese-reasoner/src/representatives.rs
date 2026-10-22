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
use super::ir::{Atom, Guard, Head, Rule, SharedListIndex, Term};
use super::ir::{Triple, Violation};
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

/// A guard over the slots of [`guards_hold_over`].
enum Check<'g> {
    Differ(usize, usize),
    Outside(usize, u64, u64),
    Listed(usize, usize, &'g SharedListIndex),
}

impl Check<'_> {
    /// Whether it holds for `chosen`, the identities of the first slots; `true` while a
    /// slot of it has none yet.
    fn holds(&self, chosen: &[u64]) -> bool {
        let at = |slot: usize| chosen.get(slot).copied();
        match *self {
            Check::Differ(a, b) => match (at(a), at(b)) {
                (Some(a), Some(b)) => a != b,
                _ => true,
            },
            Check::Outside(slot, low, high) => at(slot).is_none_or(|v| v < low || v > high),
            Check::Listed(a, b, index) => match (at(a), at(b)) {
                (Some(a), Some(b)) => a < b && index.together(a, b),
                _ => true,
            },
        }
    }
}

/// Whether some choice of one identity of each [`Bound::Class`] satisfies all of `guards`:
/// the guards of a rule evaluated over representatives, as replication evaluates them over
/// the identities (a fact of the replicated closure holds of every identity of its terms,
/// so only the guards tell the identities apart). `value` gives what each term stands
/// for; a guard over a term it leaves unbound (`None`) is decided later.
///
/// The representatives themselves are one choice, tried first. Otherwise a search over the
/// identities: a term that only `!=` guards constrain needs no more of its class than one
/// identity per such guard plus one (whatever the others chose, one of them differs), so a
/// class of any size costs that many candidates; a term a list guard asks about takes the
/// identities that guard's lists hold.
pub fn guards_hold_over(
    guards: &[Guard],
    value: impl Fn(Term) -> Option<Bound>,
    classes: &EqualityClasses,
) -> bool {
    let mut slots: Vec<(Term, Bound)> = Vec::new();
    let mut slot = |term: Term| -> Option<usize> {
        if let Some(i) = slots.iter().position(|&(t, _)| t == term) {
            return Some(i);
        }
        slots.push((term, value(term)?));
        Some(slots.len() - 1)
    };
    let mut checks = Vec::new();
    for guard in guards {
        let check = match guard {
            Guard::NotEqual(a, b) => slot(*a).zip(slot(*b)).map(|(a, b)| Check::Differ(a, b)),
            Guard::NotIn(term, low, high) => slot(*term).map(|t| Check::Outside(t, *low, *high)),
            Guard::SameList(a, b, index) => slot(*a)
                .zip(slot(*b))
                .map(|(a, b)| Check::Listed(a, b, index)),
            Guard::Classes(_) => unreachable!("class guards aren't nested"),
        };
        checks.extend(check);
    }
    let representatives: Vec<u64> = slots.iter().map(|&(_, bound)| bound.id()).collect();
    if checks.iter().all(|check| check.holds(&representatives)) {
        return true;
    }
    let candidates: Vec<Vec<u64>> = (0..slots.len())
        .map(|i| {
            let id = match slots[i].1 {
                Bound::Term(id) => return vec![id],
                Bound::Class(id) => id,
            };
            let members = classes.class_of(id).unwrap_or(std::slice::from_ref(&id));
            let lists: Vec<&SharedListIndex> = checks
                .iter()
                .filter_map(|check| match *check {
                    Check::Listed(a, b, index) if a == i || b == i => Some(index),
                    _ => None,
                })
                .collect();
            let allowed = |member: &&u64| {
                checks.iter().all(|check| match *check {
                    Check::Outside(slot, low, high) if slot == i => !(low..=high).contains(*member),
                    _ => true,
                }) && lists.iter().all(|index| index.contains(**member))
            };
            let differs = checks
                .iter()
                .filter(|check| matches!(**check, Check::Differ(a, b) if a == i || b == i))
                .count();
            let take = if lists.is_empty() {
                differs + 1
            } else {
                usize::MAX
            };
            members.iter().filter(allowed).take(take).copied().collect()
        })
        .collect();
    fn search(candidates: &[Vec<u64>], checks: &[Check<'_>], chosen: &mut Vec<u64>) -> bool {
        let Some(options) = candidates.get(chosen.len()) else {
            return true;
        };
        for &option in options {
            chosen.push(option);
            if checks.iter().all(|check| check.holds(chosen)) && search(candidates, checks, chosen)
            {
                return true;
            }
            chosen.pop();
        }
        false
    }
    search(&candidates, &checks, &mut Vec::with_capacity(slots.len()))
}

/// The classes a materialisation has merged so far, shared with the guards that read them
/// ([`ClassGuards`]): replaced between its rounds, never while rules are evaluated.
pub type SharedClasses = Arc<RwLock<Arc<EqualityClasses>>>;

/// A rule's term guards (`!=`, `NotIn`) as equality by representatives decides them
/// ([`guards_hold_over`]): its variables stand for classes, its constants for themselves.
/// Compared by the guards and the values grounding gave them, not by the classes.
#[derive(Debug, Clone)]
pub struct ClassGuards {
    guards: Arc<[Guard]>,
    /// The values grounding gave the guards' schema variables (no longer in the body).
    grounded: Vec<(u8, u64)>,
    classes: SharedClasses,
}

impl ClassGuards {
    /// Whether some choice of identities satisfies the guards (`value`: a variable's value,
    /// a representative; `None` while unbound). The representatives are checked first
    /// without the classes; only a binding they fail reads them.
    pub fn holds(&self, value: &dyn Fn(Term) -> Option<u64>) -> bool {
        let bound = |term: Term| match term {
            Term::Const(id) => Some(Bound::Term(id)),
            Term::Var(v) => self
                .grounded
                .iter()
                .find(|&&(g, _)| g == v)
                .map(|&(_, id)| id)
                .or_else(|| value(term))
                .map(Bound::Class),
        };
        if self
            .guards
            .iter()
            .all(|guard| guard.holds(|term| bound(term).map(Bound::id)))
        {
            return true;
        }
        let classes = self.classes.read().unwrap_or_else(PoisonError::into_inner);
        !classes.is_empty() && guards_hold_over(&self.guards, bound, &classes)
    }

    /// The guards with the schema variables' values of `substitution` (grounding).
    pub fn ground(&self, substitution: &[Option<u64>]) -> Self {
        let mut grounded = self.grounded.clone();
        for guard in self.guards.iter() {
            let terms = match guard {
                Guard::NotEqual(a, b) => [Some(*a), Some(*b)],
                Guard::NotIn(term, _, _) => [Some(*term), None],
                Guard::SameList(..) | Guard::Classes(_) => [None, None],
            };
            for term in terms.into_iter().flatten() {
                if let Term::Var(v) = term
                    && let Some(Some(id)) = substitution.get(usize::from(v))
                    && !grounded.iter().any(|&(g, _)| g == v)
                {
                    grounded.push((v, *id));
                }
            }
        }
        Self {
            grounded,
            ..self.clone()
        }
    }
}

impl PartialEq for ClassGuards {
    fn eq(&self, other: &Self) -> bool {
        self.guards == other.guards && self.grounded == other.grounded
    }
}

impl Eq for ClassGuards {}

impl std::hash::Hash for ClassGuards {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        (&self.guards, &self.grounded).hash(state);
    }
}

/// `rule` as equality by representatives evaluates it: its term guards (`!=`, `NotIn`)
/// decided over `classes` ([`ClassGuards`]). Unchanged without term guards, and where a
/// guard failing over one class changes nothing: only `!=` guards, each on the pair every
/// head atom equates (`prp-key`'s `x != y` for `x sameAs y`: a class equated with itself).
/// List guards stay as they are: the lists are read over representatives, with their
/// positions ([`super::lists`]).
pub fn over_classes(rule: Rule, same_as: u64, classes: &SharedClasses) -> Rule {
    let (terms, lists): (Vec<Guard>, Vec<Guard>) = rule
        .guards
        .iter()
        .cloned()
        .partition(|guard| matches!(guard, Guard::NotEqual(..) | Guard::NotIn(..)));
    let equates = |atom: &Atom, guard: &Guard| match *guard {
        Guard::NotEqual(a, b) => {
            atom.0 == [a, Term::Const(same_as), b] || atom.0 == [b, Term::Const(same_as), a]
        }
        _ => false,
    };
    let reflexive = match &rule.head {
        Head::Facts(atoms) => {
            !atoms.is_empty()
                && terms
                    .iter()
                    .all(|guard| atoms.iter().all(|atom| equates(atom, guard)))
        }
        Head::Inconsistent => false,
    };
    if terms.is_empty() || reflexive {
        return rule;
    }
    let mut guards = lists;
    guards.push(Guard::Classes(ClassGuards {
        guards: terms.into(),
        grounded: Vec::new(),
        classes: Arc::clone(classes),
    }));
    Rule { guards, ..rule }
}

/// A closure over representatives.
#[derive(Debug, Default)]
pub struct RepresentativeClosure {
    /// Every fact of the closure over representatives, the rewritten input included,
    /// sorted, without duplicates.
    pub facts: Vec<Triple>,
    pub classes: EqualityClasses,
    /// Violations over representatives, sorted.
    pub violations: Vec<Violation>,
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
    let rule = rules.iter().find(|r| r.name == "eq-rep-s")?;
    match rule.body.first()?.0[1] {
        super::ir::Term::Const(id) => Some(id),
        super::ir::Term::Var(_) => None,
    }
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
