//! Lazy unfolding of definitions (`Options::lazy_definitions`; Horrocks and Tobies, KR
//! 2000): for `A ≡ D`, `A`'s only definition and not cyclic, the clauses keep `A ⊑ D`, and
//! every negative occurrence of `A` is read as `¬D` in its place, instead of `D ⊑ A`
//! holding of every element. That direction is the costly one where `¬D` is a
//! disjunction: `A ≡ ¬B ⊓ ¬C` makes `B ⊔ C ⊔ A` hold everywhere, where unfolded the
//! disjunction only arises where `¬A` is asked for.
//!
//! Only Boolean combinations of names whose reverse `D ⊑ A` isn't Horn are unfolded:
//! there that direction is a disjunction over every element, and `¬D` in each place of
//! `¬A` holds only where the place does. A Horn reverse (`B ⊓ C ⊑ A` is
//! `B(x) ∧ C(x) → A(x)`) is cheaper kept: unfolded, `¬A` would branch over `¬B ⊔ ¬C`.
//! Definitions by restrictions stay as they are: unfolding them too made k_branch (W3C
//! DL-661) eight times slower, while k_grz (DL-204) only needs the Boolean ones.
//!
//! Equisatisfiable; no model of the left-out direction, so a model read off the clauses
//! under-approximates such an `A`: callers that read models keep this off.
//!
//! A class isn't unfolded where the normalisation reads its occurrences into clause bodies
//! without negating them: the class of a key, and anything inside a cardinality's filler.

use std::collections::{HashMap, HashSet};

use crate::mapping::Ontology;
use crate::model::{Axiom, ClassExpr, ExprId, Term};
use crate::walk::{self, Holds};

/// The unfolded definitions: `A ↦ D`, and by axiom index `(A, D)` as expressions.
#[derive(Debug, Default)]
pub(crate) struct Unfolded {
    pub(crate) by_class: HashMap<Term, ExprId>,
    pub(crate) by_axiom: HashMap<usize, (ExprId, ExprId)>,
}

pub(crate) fn unfolded(ontology: &Ontology) -> Unfolded {
    let class = |e: ExprId| match ontology.classes.get(e.0) {
        ClassExpr::Class(a) => Some(*a),
        _ => None,
    };
    let mut definitions: HashMap<Term, usize> = HashMap::new();
    let mut candidates: HashMap<Term, (usize, ExprId, ExprId)> = HashMap::new();
    let mut excluded: HashSet<Term> = HashSet::new();
    for (index, axiom) in ontology.axioms.iter().enumerate() {
        match axiom {
            Axiom::EquivalentClasses(xs) => {
                for &x in xs {
                    if let Some(a) = class(x) {
                        *definitions.entry(a).or_default() += 1;
                    }
                }
                if let [x, y] = xs[..] {
                    match (class(x), class(y)) {
                        (Some(a), None) => {
                            candidates.insert(a, (index, x, y));
                        }
                        (None, Some(a)) => {
                            candidates.insert(a, (index, y, x));
                        }
                        _ => {}
                    }
                }
            }
            Axiom::DisjointUnion(a, _) => {
                if let ClassExpr::Class(a) = ontology.class(*a) {
                    excluded.insert(*a);
                }
            }
            Axiom::HasKey(x, _, _) => names(ontology, *x, &mut |a| {
                excluded.insert(a);
            }),
            _ => {}
        }
        for e in expressions(axiom) {
            in_fillers(ontology, e, &mut excluded);
        }
    }
    candidates.retain(|a, &mut (_, _, d)| {
        definitions.get(a) == Some(&1)
            && !excluded.contains(a)
            && propositional(ontology, d)
            && !horn_reverse(ontology, d)
    });
    // Not cyclic: peel the definitions whose `D` mentions no remaining candidate.
    let mut uses: HashMap<Term, HashSet<Term>> = HashMap::new();
    for (&a, &(_, _, d)) in &candidates {
        let mut set = HashSet::new();
        names(ontology, d, &mut |b| {
            set.insert(b);
        });
        uses.insert(a, set);
    }
    let mut acyclic: HashSet<Term> = HashSet::new();
    loop {
        let ready: Vec<Term> = uses
            .iter()
            .filter(|(a, used)| {
                !acyclic.contains(*a)
                    && used
                        .iter()
                        .all(|b| !candidates.contains_key(b) || acyclic.contains(b))
            })
            .map(|(&a, _)| a)
            .collect();
        if ready.is_empty() {
            break;
        }
        acyclic.extend(ready);
    }
    let mut out = Unfolded::default();
    for (a, (index, x, d)) in candidates {
        if acyclic.contains(&a) {
            out.by_class.insert(a, d);
            out.by_axiom.insert(index, (x, d));
        }
    }
    out
}

/// Whether `D ⊑ A` is a Horn clause: `D` holds only of what body atoms say.
fn horn_reverse(ontology: &Ontology, d: ExprId) -> bool {
    walk::holds(d, (), |e, ()| match ontology.classes.get(e.0) {
        ClassExpr::Class(_) | ClassExpr::Thing | ClassExpr::HasValue(..) => Holds::Is(true),
        ClassExpr::OneOf(xs) => Holds::Is(xs.len() == 1),
        ClassExpr::And(xs) => Holds::All(xs, ()),
        ClassExpr::Some(_, x) => Holds::All(std::slice::from_ref(x), ()),
        _ => Holds::Is(false),
    })
}

/// Whether `d` is a Boolean combination of names.
fn propositional(ontology: &Ontology, d: ExprId) -> bool {
    walk::holds(d, (), |e, ()| match ontology.classes.get(e.0) {
        ClassExpr::Class(_) | ClassExpr::Thing | ClassExpr::Nothing => Holds::Is(true),
        ClassExpr::Not(x) => Holds::All(std::slice::from_ref(x), ()),
        ClassExpr::And(xs) | ClassExpr::Or(xs) => Holds::All(xs, ()),
        _ => Holds::Is(false),
    })
}

/// The class expressions an axiom holds directly.
fn expressions(axiom: &Axiom) -> Vec<ExprId> {
    match axiom {
        Axiom::SubClassOf(a, b) => vec![*a, *b],
        // A disjoint union's class is no filler, and a named one is excluded above.
        Axiom::EquivalentClasses(xs) | Axiom::DisjointClasses(xs) | Axiom::DisjointUnion(_, xs) => {
            xs.clone()
        }
        Axiom::ObjectPropertyDomain(_, x)
        | Axiom::ObjectPropertyRange(_, x)
        | Axiom::DataPropertyDomain(_, x)
        | Axiom::HasKey(x, _, _)
        | Axiom::ClassAssertion(x, _) => vec![*x],
        _ => Vec::new(),
    }
}

/// Every named class in `e`, from the left, with a stack of its own (the nesting depth is
/// no call depth).
fn names(ontology: &Ontology, e: ExprId, f: &mut dyn FnMut(Term)) {
    let mut stack = vec![e];
    while let Some(e) = stack.pop() {
        match ontology.classes.get(e.0) {
            ClassExpr::Class(a) => f(*a),
            ClassExpr::Not(x)
            | ClassExpr::Some(_, x)
            | ClassExpr::All(_, x)
            | ClassExpr::Min(_, _, x)
            | ClassExpr::Max(_, _, x)
            | ClassExpr::Exact(_, _, x) => stack.push(*x),
            ClassExpr::And(xs) | ClassExpr::Or(xs) => stack.extend(xs.iter().rev()),
            _ => {}
        }
    }
}

/// Adds every named class inside a cardinality's filler in `e` to `out`, with a stack of
/// its own (the nesting depth is no call depth).
fn in_fillers(ontology: &Ontology, e: ExprId, out: &mut HashSet<Term>) {
    // Each expression with whether it is inside a cardinality's filler.
    let mut stack = vec![(e, false)];
    while let Some((e, inside)) = stack.pop() {
        match ontology.classes.get(e.0) {
            ClassExpr::Class(a) if inside => {
                out.insert(*a);
            }
            ClassExpr::Min(_, _, x) | ClassExpr::Max(_, _, x) | ClassExpr::Exact(_, _, x) => {
                stack.push((*x, true))
            }
            ClassExpr::Not(x) | ClassExpr::Some(_, x) | ClassExpr::All(_, x) => {
                stack.push((*x, inside))
            }
            ClassExpr::And(xs) | ClassExpr::Or(xs) => stack.extend(xs.iter().map(|&x| (x, inside))),
            _ => {}
        }
    }
}
