//! The batch executor's joins: a rule variant's atoms after its driver, compiled into
//! steps that read the working set's runs as slices ([`Relation::<R>::matching`]) and bind
//! numbers in an array, a guard checked as soon as its terms are bound, and the head facts
//! packed straight into the morsel's candidates (`eval::Candidate` for `u128`).
//!
//! The generic walk (`eval::walk`, the delta executor's) reads any [`Source`] through a
//! callback per fact, looks the relation up by predicate per lookup, binds through
//! `Option`s, checks guards only once every atom is bound, and instantiates each head by
//! matching its terms per binding: on the httpd subset of CSPA its enumeration was 79 % of
//! the joins' time (13 October 2026).

use super::*;
use crate::eval::{Candidate, Job};
use crate::ir::{Atom, Guard};

/// Variables a rule may have here; a rule with more takes the generic walk.
const MAX_VARIABLES: usize = 32;

/// How one position of an atom is read at its step.
#[derive(Clone, Copy)]
enum Slot {
    /// A constant (bound in the lookup).
    Constant(u64),
    /// A variable an earlier step bound (bound in the lookup).
    Bound(u8),
    /// A variable this step binds.
    Free(u8),
    /// The variable this step binds at its subject, again: the two must agree.
    Again,
}

/// One atom after the driver.
struct Step<'s, R: Representation> {
    relation: &'s Relation<R>,
    seg: Seg,
    s: Slot,
    o: Slot,
    /// The guards whose terms are all bound once this step has bound its variables.
    guards: Vec<&'s Guard>,
}

/// A head position: a constant or a variable's value.
#[derive(Clone, Copy)]
enum Out {
    Constant(u64),
    Variable(u8),
}

/// A job compiled for the working set ([`WorkingStore<R>`] with dense numbers).
pub(super) struct Plan<'s, R: Representation> {
    driver: &'s Atom,
    /// The guards whose terms the driver binds.
    driver_guards: Vec<&'s Guard>,
    steps: Vec<Step<'s, R>>,
    heads: Vec<[Out; 3]>,
}

impl<'s, R: Representation> Plan<'s, R> {
    /// `job` over `store`; `None` where the generic walk runs it: an atom without a
    /// constant predicate, a lookup by object of a relation without that order, more
    /// variables than [`MAX_VARIABLES`], or a head that isn't facts.
    pub(super) fn of(store: &'s WorkingStore<R>, job: &'s Job<'_>) -> Option<Self> {
        let rule: &'s Rule = job.rule;
        if rule.variables() > MAX_VARIABLES {
            return None;
        }
        let Head::Facts(heads) = &rule.head else {
            return None;
        };
        let order = job.order();
        let driver = &rule.body[order[0].0];
        let mut bound = [false; MAX_VARIABLES];
        let bind_atom = |atom: &Atom, bound: &mut [bool; MAX_VARIABLES]| {
            for term in atom.0 {
                if let Term::Var(v) = term {
                    bound[usize::from(v)] = true;
                }
            }
        };
        bind_atom(driver, &mut bound);
        let ready = |guard: &Guard, bound: &[bool; MAX_VARIABLES]| {
            let terms: &[Term] = match guard {
                Guard::NotEqual(a, b) | Guard::SameList(a, b, _) => &[*a, *b],
                Guard::NotIn(t, _, _) => std::slice::from_ref(t),
            };
            terms.iter().all(|t| {
                matches!(t, Term::Const(_)) || matches!(t, Term::Var(v) if bound[usize::from(*v)])
            })
        };
        let mut pending: Vec<&'s Guard> = rule.guards.iter().collect();
        let mut take_ready = |bound: &[bool; MAX_VARIABLES]| -> Vec<&'s Guard> {
            let (now, later): (Vec<&Guard>, Vec<&Guard>) =
                pending.iter().partition(|guard| ready(guard, bound));
            pending = later;
            now
        };
        let driver_guards = take_ready(&bound);
        let mut steps = Vec::with_capacity(order.len() - 1);
        for &(index, seg) in &order[1..] {
            let atom = &rule.body[index];
            let [s, p, o] = atom.0;
            let Term::Const(p) = p else {
                return None;
            };
            let relation = store.relation(p)?;
            let slot = |term: Term, bound: &[bool; MAX_VARIABLES]| match term {
                Term::Const(c) => Slot::Constant(c),
                Term::Var(v) if bound[usize::from(v)] => Slot::Bound(v),
                Term::Var(v) => Slot::Free(v),
            };
            let s_slot = slot(s, &bound);
            let mut o_slot = slot(o, &bound);
            if let (Slot::Free(a), Slot::Free(b)) = (s_slot, o_slot)
                && a == b
            {
                o_slot = Slot::Again;
            }
            // A lookup by object alone needs the object order.
            if matches!(s_slot, Slot::Free(_))
                && !matches!(o_slot, Slot::Free(_) | Slot::Again)
                && relation.by_subject_only
            {
                return None;
            }
            bind_atom(atom, &mut bound);
            steps.push(Step {
                relation,
                seg,
                s: s_slot,
                o: o_slot,
                guards: take_ready(&bound),
            });
        }
        let out = |term: Term| match term {
            Term::Const(c) => Out::Constant(c),
            Term::Var(v) => Out::Variable(v),
        };
        Some(Self {
            driver,
            driver_guards,
            steps,
            heads: heads.iter().map(|atom| atom.0.map(out)).collect(),
        })
    }

    /// The head facts of the bindings of drivers `range` of `job` over `store`, packed
    /// into `out`; returns the bindings enumerated.
    pub(super) fn run(
        &self,
        store: &WorkingStore<R>,
        job: &Job<'_>,
        range: std::ops::Range<usize>,
        out: &mut Vec<R::Candidate>,
    ) -> u64 {
        let mut bindings = [0u64; MAX_VARIABLES];
        let mut count = 0u64;
        // A finger per step and run: a step's keys mostly ascend ([`Pairs::<R>::range_from`]).
        let mut fingers = vec![[RangeFinger::default(); RUNS]; self.steps.len()];
        job.drivers_in(store, range, &mut |fact| {
            if bind(self.driver, fact, &mut bindings) && holds(&self.driver_guards, &bindings) {
                self.descend(0, &mut bindings, &mut fingers, out, &mut count);
            }
        });
        count
    }

    fn descend(
        &self,
        depth: usize,
        bindings: &mut [u64; MAX_VARIABLES],
        fingers: &mut [[RangeFinger; RUNS]],
        out: &mut Vec<R::Candidate>,
        count: &mut u64,
    ) {
        let Some(step) = self.steps.get(depth) else {
            *count += 1;
            for head in &self.heads {
                let value = |position: Out| match position {
                    Out::Constant(c) => c,
                    Out::Variable(v) => bindings[usize::from(v)],
                };
                out.push(R::Candidate::of([
                    value(head[0]),
                    value(head[1]),
                    value(head[2]),
                ]));
            }
            return;
        };
        let key = |slot: Slot, bindings: &[u64; MAX_VARIABLES]| match slot {
            Slot::Constant(c) => Some(c),
            Slot::Bound(v) => Some(bindings[usize::from(v)]),
            Slot::Free(_) | Slot::Again => None,
        };
        let (s, o) = (key(step.s, bindings), key(step.o, bindings));
        let relation = step.relation;
        let (mine, deeper) = fingers.split_first_mut().expect("a finger array per step");
        let mut visit = |a: u64, b: u64, bindings: &mut [u64; MAX_VARIABLES]| {
            if let Slot::Free(v) = step.s {
                bindings[usize::from(v)] = a;
            }
            match step.o {
                Slot::Free(v) => bindings[usize::from(v)] = b,
                Slot::Again if a != b => return,
                _ => {}
            }
            if holds(&step.guards, bindings) {
                self.descend(depth + 1, bindings, deeper, out, count);
            }
        };
        let mut each = |a: u64, b: u64| visit(a, b, bindings);
        if !relation.each(s, o, step.seg, mine, &mut each) {
            relation.scan(s, o, step.seg, &mut each);
        }
    }
}

/// Binds `atom`'s variables to `fact`; `false` where a constant or a repeated variable
/// disagrees.
fn bind(atom: &Atom, fact: Triple, bindings: &mut [u64; MAX_VARIABLES]) -> bool {
    let mut set = 0u32;
    for (term, value) in atom.0.into_iter().zip(fact) {
        match term {
            Term::Const(c) if c != value => return false,
            Term::Const(_) => {}
            Term::Var(v) => {
                let v = usize::from(v);
                if set & (1 << v) != 0 && bindings[v] != value {
                    return false;
                }
                set |= 1 << v;
                bindings[v] = value;
            }
        }
    }
    true
}

/// Whether `guards` hold over `bindings` (their terms bound).
#[inline]
fn holds(guards: &[&Guard], bindings: &[u64; MAX_VARIABLES]) -> bool {
    guards.iter().all(|guard| {
        guard.holds(|term| match term {
            Term::Const(c) => Some(c),
            Term::Var(v) => Some(bindings[usize::from(v)]),
        })
    })
}
