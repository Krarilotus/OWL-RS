//! The normalisation into DL-clauses checked against the semantics on finite
//! interpretations (docs/design/owl2-dl.md §3, the gate of work package 2.2): for random
//! small ontologies and random interpretations of their signature over domains of one to
//! three elements, "the interpretation is a model of the ontology" equals "it extends to
//! a model of the clauses and facts".
//!
//! - **The extension** of the fresh names is the canonical one: a subexpression's name is
//!   the subexpression (it occurs with its polarity only); an automaton's state names are
//!   the greatest fixpoint of its clauses. Where the domain and the fresh names are few,
//!   every extension is tried as well: none may satisfy the clauses of a non-model
//!   (soundness without the polarity argument).
//! - **Role chains and transitivity** are encoded by automata, exact on interpretations
//!   closed under the role inclusions: such ontologies get closed interpretations.

use std::collections::HashMap;

use nrese_owl::{
    Axiom, BodyAtom, Characteristic, ClassExpr, Clause, Concept, EntityKind, ExprId, Filler,
    FreshOf, HeadAtom, Interner, Normalised, ObjProp, Ontology, Options, Term, Var, normalise_with,
};

/// Picks a role from the random source.
type PickRole = dyn Fn(&mut dyn FnMut(u64) -> u64) -> ObjProp;

/// A finite interpretation: domain `0..n`, concepts and successor sets as bitmasks.
#[derive(Debug, Clone)]
struct Interp {
    n: u32,
    concepts: HashMap<Term, u64>,
    /// Per role, per element, its successors.
    roles: HashMap<Term, Vec<u64>>,
    individuals: HashMap<Term, u32>,
    fresh: Vec<u64>,
}

impl Interp {
    fn all(&self) -> u64 {
        (1u64 << self.n) - 1
    }

    fn succ(&self, role: ObjProp, d: u32) -> u64 {
        match role {
            ObjProp::Named(p) => self.roles.get(&p).map_or(0, |r| r[d as usize]),
            ObjProp::Inverse(p) => {
                let Some(r) = self.roles.get(&p) else {
                    return 0;
                };
                (0..self.n)
                    .filter(|&e| r[e as usize] & (1 << d) != 0)
                    .fold(0, |m, e| m | (1 << e))
            }
        }
    }

    fn edge(&self, role: Term, a: u32, b: u32) -> bool {
        self.roles
            .get(&role)
            .is_some_and(|r| r[a as usize] & (1 << b) != 0)
    }

    fn concept(&self, c: Concept) -> u64 {
        match c {
            Concept::Named(t) => self.concepts.get(&t).copied().unwrap_or(0),
            Concept::Fresh(q) => self.fresh[q as usize],
        }
    }
}

fn eval(classes: &Interner<ClassExpr>, e: ExprId, i: &Interp) -> u64 {
    let all = i.all();
    let each = |test: &dyn Fn(u32) -> bool| {
        (0..i.n)
            .filter(|&d| test(d))
            .fold(0u64, |m, d| m | (1 << d))
    };
    match classes.get(e.0) {
        ClassExpr::Class(t) => i.concepts.get(t).copied().unwrap_or(0),
        ClassExpr::Thing => all,
        ClassExpr::Nothing => 0,
        ClassExpr::And(xs) => xs.iter().fold(all, |m, &x| m & eval(classes, x, i)),
        ClassExpr::Or(xs) => xs.iter().fold(0, |m, &x| m | eval(classes, x, i)),
        ClassExpr::Not(x) => all & !eval(classes, *x, i),
        ClassExpr::OneOf(xs) => xs.iter().fold(0, |m, a| m | (1 << i.individuals[a])),
        ClassExpr::Some(r, x) => {
            let c = eval(classes, *x, i);
            each(&|d| i.succ(*r, d) & c != 0)
        }
        ClassExpr::All(r, x) => {
            let c = eval(classes, *x, i);
            each(&|d| i.succ(*r, d) & !c == 0)
        }
        ClassExpr::HasValue(r, a) => each(&|d| i.succ(*r, d) & (1 << i.individuals[a]) != 0),
        ClassExpr::HasSelf(r) => each(&|d| i.succ(*r, d) & (1 << d) != 0),
        ClassExpr::Min(n, r, x) => {
            let c = eval(classes, *x, i);
            each(&|d| (i.succ(*r, d) & c).count_ones() >= *n)
        }
        ClassExpr::Max(n, r, x) => {
            let c = eval(classes, *x, i);
            each(&|d| (i.succ(*r, d) & c).count_ones() <= *n)
        }
        ClassExpr::Exact(n, r, x) => {
            let c = eval(classes, *x, i);
            each(&|d| (i.succ(*r, d) & c).count_ones() == *n)
        }
        other => panic!("no data in these ontologies: {other:?}"),
    }
}

/// The pairs `(a, b)` of a chain of roles.
fn compose(i: &Interp, chain: &[ObjProp]) -> Vec<u64> {
    let mut reach: Vec<u64> = (0..i.n).map(|d| 1u64 << d).collect();
    for &r in chain {
        reach = reach
            .iter()
            .map(|&from| {
                (0..i.n)
                    .filter(|&d| from & (1 << d) != 0)
                    .fold(0, |m, d| m | i.succ(r, d))
            })
            .collect();
    }
    reach
}

fn holds(o: &Ontology, axiom: &Axiom, i: &Interp) -> bool {
    let c = |e: ExprId| eval(&o.classes, e, i);
    let all = i.all();
    let rel = |r: ObjProp| (0..i.n).map(|d| i.succ(r, d)).collect::<Vec<u64>>();
    match axiom {
        Axiom::Declaration(..) => true,
        Axiom::SubClassOf(a, b) => c(*a) & !c(*b) == 0,
        Axiom::EquivalentClasses(xs) => xs.windows(2).all(|w| c(w[0]) == c(w[1])),
        Axiom::DisjointClasses(xs) => xs
            .iter()
            .enumerate()
            .all(|(k, &a)| xs[k + 1..].iter().all(|&b| c(a) & c(b) == 0)),
        Axiom::SubObjectPropertyOf(chain, sup) => {
            let (got, have) = (compose(i, chain), rel(*sup));
            got.iter().zip(&have).all(|(g, h)| g & !h == 0)
        }
        Axiom::InverseObjectProperties(a, b) => rel(*a) == rel(b.inverse()),
        Axiom::EquivalentObjectProperties(ps) => ps.windows(2).all(|w| rel(w[0]) == rel(w[1])),
        Axiom::DisjointUnion(class, xs) => {
            let named = i.concepts.get(class).copied().unwrap_or(0);
            named == xs.iter().fold(0, |m, &x| m | c(x))
                && xs
                    .iter()
                    .enumerate()
                    .all(|(k, &a)| xs[k + 1..].iter().all(|&b| c(a) & c(b) == 0))
        }
        Axiom::ObjectPropertyDomain(r, x) => {
            let d = (0..i.n)
                .filter(|&d| i.succ(*r, d) != 0)
                .fold(0u64, |m, d| m | (1 << d));
            d & !c(*x) == 0
        }
        Axiom::ObjectPropertyRange(r, x) => (0..i.n).all(|d| i.succ(*r, d) & !c(*x) == 0),
        Axiom::ObjectCharacteristic(kind, r) => {
            (0..i.n).all(|d| {
                let s = i.succ(*r, d);
                match kind {
                    Characteristic::Functional => s.count_ones() <= 1,
                    Characteristic::InverseFunctional => i.succ(r.inverse(), d).count_ones() <= 1,
                    Characteristic::Reflexive => s & (1 << d) != 0,
                    Characteristic::Irreflexive => s & (1 << d) == 0,
                    Characteristic::Symmetric => s == i.succ(r.inverse(), d),
                    Characteristic::Asymmetric => s & i.succ(r.inverse(), d) == 0,
                    Characteristic::Transitive => compose(i, &[*r, *r])[d as usize] & !s == 0,
                }
            }) && all != 0
        }
        Axiom::DisjointObjectProperties(ps) => ps.iter().enumerate().all(|(k, &a)| {
            ps[k + 1..]
                .iter()
                .all(|&b| (0..i.n).all(|d| i.succ(a, d) & i.succ(b, d) == 0))
        }),
        Axiom::ClassAssertion(x, a) => c(*x) & (1 << i.individuals[a]) != 0,
        Axiom::ObjectPropertyAssertion(p, a, b) => i.edge(*p, i.individuals[a], i.individuals[b]),
        Axiom::NegativeObjectPropertyAssertion(p, a, b) => {
            !i.edge(*p, i.individuals[a], i.individuals[b])
        }
        Axiom::SameIndividual(xs) => xs
            .windows(2)
            .all(|w| i.individuals[&w[0]] == i.individuals[&w[1]]),
        Axiom::DifferentIndividuals(xs) => xs.iter().enumerate().all(|(k, a)| {
            xs[k + 1..]
                .iter()
                .all(|b| i.individuals[a] != i.individuals[b])
        }),
        other => panic!("not generated: {other:?}"),
    }
}

/// Every assignment of `vars` over the domain.
fn assignments(vars: &[Var], n: u32, f: &mut dyn FnMut(&HashMap<Var, u32>) -> bool) -> bool {
    let mut values = vec![0u32; vars.len()];
    loop {
        let map: HashMap<Var, u32> = vars.iter().copied().zip(values.iter().copied()).collect();
        if !f(&map) {
            return false;
        }
        let mut k = 0;
        loop {
            if k == values.len() {
                return true;
            }
            values[k] += 1;
            if values[k] < n {
                break;
            }
            values[k] = 0;
            k += 1;
        }
    }
}

fn vars_of(clause: &Clause) -> Vec<Var> {
    let mut vars = vec![Var::X];
    let mut add = |v: Var| {
        if !vars.contains(&v) {
            vars.push(v);
        }
    };
    for b in &clause.body {
        match b {
            BodyAtom::Concept(_, v) | BodyAtom::Nominal(_, v) => add(*v),
            BodyAtom::Role(_, a, b) => {
                add(*a);
                add(*b);
            }
            BodyAtom::Data(..) => panic!("no data"),
        }
    }
    for h in &clause.head {
        match h {
            HeadAtom::Concept(_, v) | HeadAtom::Nominal(_, v) => add(*v),
            HeadAtom::Role(_, a, b) | HeadAtom::Equal(a, b) => {
                add(*a);
                add(*b);
            }
            HeadAtom::AtLeast { var, .. } | HeadAtom::AtMost { var, .. } => add(*var),
            _ => panic!("no data"),
        }
    }
    vars
}

fn body_true(b: &BodyAtom, a: &HashMap<Var, u32>, i: &Interp) -> bool {
    match b {
        BodyAtom::Concept(c, v) => i.concept(*c) & (1 << a[v]) != 0,
        BodyAtom::Role(p, x, y) => i.edge(*p, a[x], a[y]),
        BodyAtom::Nominal(t, v) => i.individuals[t] == a[v],
        BodyAtom::Data(..) => unreachable!(),
    }
}

fn mask(filler: &Filler, i: &Interp) -> u64 {
    match filler {
        Filler::Top => i.all(),
        Filler::Is(c) => i.concept(*c),
        Filler::Not(c) => i.all() & !i.concept(*c),
    }
}

fn head_true(h: &HeadAtom, a: &HashMap<Var, u32>, i: &Interp) -> bool {
    match h {
        HeadAtom::Concept(c, v) => i.concept(*c) & (1 << a[v]) != 0,
        HeadAtom::Role(p, x, y) => i.edge(*p, a[x], a[y]),
        HeadAtom::Nominal(t, v) => i.individuals[t] == a[v],
        HeadAtom::Equal(x, y) => a[x] == a[y],
        HeadAtom::AtLeast {
            n,
            role,
            filler,
            var,
        } => (i.succ(*role, a[var]) & mask(filler, i)).count_ones() >= *n,
        HeadAtom::AtMost {
            n,
            role,
            filler,
            var,
        } => (i.succ(*role, a[var]) & mask(filler, i)).count_ones() <= *n,
        _ => unreachable!(),
    }
}

/// Whether `clause` holds in `i`; else a violating assignment.
fn violation(clause: &Clause, i: &Interp) -> Option<HashMap<Var, u32>> {
    let vars = vars_of(clause);
    let mut found = None;
    assignments(&vars, i.n, &mut |a| {
        let violated = clause.body.iter().all(|b| body_true(b, a, i))
            && !clause.head.iter().any(|h| head_true(h, a, i));
        if violated {
            found = Some(a.clone());
        }
        !violated
    });
    found
}

fn clauses_hold(n: &Normalised, i: &Interp) -> bool {
    n.clauses.iter().all(|c| violation(c, i).is_none())
        && n.facts
            .concepts
            .iter()
            .all(|(c, a, _)| i.concept(*c) & (1 << i.individuals[a]) != 0)
        && n.facts
            .roles
            .iter()
            .all(|(p, a, b, _)| i.edge(*p, i.individuals[a], i.individuals[b]))
        && n.facts
            .not_roles
            .iter()
            .all(|(p, a, b, _)| !i.edge(*p, i.individuals[a], i.individuals[b]))
        && n.facts
            .same
            .iter()
            .all(|(a, b, _)| i.individuals[a] == i.individuals[b])
        && n.facts
            .different
            .iter()
            .all(|(a, b, _)| i.individuals[a] != i.individuals[b])
}

/// The canonical extension of the fresh names: subexpressions their value, automaton
/// states the greatest fixpoint.
fn extend(n: &Normalised, i: &mut Interp) {
    i.fresh = vec![0; n.fresh.len()];
    // Subexpressions refer to the original signature only.
    for (q, of) in n.fresh.iter().enumerate() {
        if let FreshOf::Expr { expr, .. } = of {
            i.fresh[q] = eval(&n.classes, *expr, i);
        }
    }
    let states: Vec<usize> = (0..n.fresh.len())
        .filter(|&q| matches!(n.fresh[q], FreshOf::State { .. }))
        .collect();
    for &q in &states {
        i.fresh[q] = i.all();
    }
    loop {
        let mut changed = false;
        for clause in &n.clauses {
            let state_atoms: Vec<(u32, Var)> = clause
                .body
                .iter()
                .filter_map(|b| match b {
                    BodyAtom::Concept(Concept::Fresh(q), v) if states.contains(&(*q as usize)) => {
                        Some((*q, *v))
                    }
                    _ => None,
                })
                .collect();
            if state_atoms.is_empty() {
                continue;
            }
            while let Some(a) = violation(clause, i) {
                for &(q, v) in &state_atoms {
                    i.fresh[q as usize] &= !(1 << a[&v]);
                }
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
}

fn rng(seed: u64) -> impl FnMut(u64) -> u64 {
    let mut state = seed;
    move |n: u64| {
        state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        (z ^ (z >> 31)) % n.max(1)
    }
}

/// A domain over a non-simple role goes through the role's automaton, as a range does:
/// engines don't add the edges a chain or transitivity implies, so a raw
/// `R(x, y) → C(x)` would miss them. The models test below can't see that, since its
/// interpretations are closed under the role inclusions. (Found by the context core's EL
/// gate on 3 October 2026: `C4 ≡ ∃p1.C1`, `Range(p1) = ∃p2.C3`, `p1 ∘ p2 ⊑ p2`,
/// `Domain(p2) = C3 ⊓ C5` lost `C4 ⊑ C3`.)
#[test]
fn a_domain_over_a_non_simple_role_goes_through_its_automaton() {
    let (class, role) = (0, 10);
    let mut o = Ontology::default();
    let c = o.classes.intern(ClassExpr::Class(class));
    o.axioms = vec![
        Axiom::Declaration(EntityKind::Class, class),
        Axiom::ObjectCharacteristic(Characteristic::Transitive, ObjProp::Named(role)),
        Axiom::ObjectPropertyDomain(ObjProp::Named(role), ExprId(c)),
    ];
    o.sources = vec![Vec::new(); o.axioms.len()];
    let normalised = normalise_with(&o, Options::default());
    let raw = normalised.clauses.iter().any(|clause| {
        clause
            .body
            .iter()
            .any(|b| matches!(b, BodyAtom::Role(r, ..) if *r == role))
            && clause
                .head
                .iter()
                .any(|h| matches!(h, HeadAtom::Concept(Concept::Named(x), _) if *x == class))
    });
    assert!(
        !raw,
        "a raw domain clause on a transitive role: {:#?}",
        normalised.clauses
    );
    assert!(
        normalised.clauses.iter().any(|clause| clause
            .head
            .iter()
            .any(|h| matches!(h, HeadAtom::Concept(Concept::Fresh(_), _)))),
        "the domain isn't routed through a fresh name or automaton state: {:#?}",
        normalised.clauses
    );
    // And Horn: a domain is a universal over the inverse, no disjunction (the U1 bound
    // turned a disjunctive domain into typing every element).
    assert!(
        normalised
            .clauses
            .iter()
            .all(|clause| clause.head.len() <= 1),
        "a disjunctive clause from a domain: {:#?}",
        normalised.clauses
    );
}

/// A transition of a role's automaton names the role inclusions of its edge with the
/// universal it encodes: a justification through a role hierarchy needs them. (Found by
/// the context core's proof test on 3 October 2026: `C1 ⊑ ∃p0.(C0 ⊓ C2)`, `p0 ⊑ p2`,
/// `Transitive(p2)`, `Domain(p2) = C2` gave `C1 ⊑ C2` a justification without `p0 ⊑ p2`.)
#[test]
fn automaton_transitions_need_their_role_inclusions() {
    let (c0, c1, c2, p0, p2) = (0, 1, 2, 10, 12);
    let mut o = Ontology::default();
    let class = |o: &mut Ontology, c| ExprId(o.classes.intern(ClassExpr::Class(c)));
    let (e0, e1, e2) = (class(&mut o, c0), class(&mut o, c1), class(&mut o, c2));
    let both = ExprId(o.classes.intern(ClassExpr::And(vec![e0, e2])));
    let some = ExprId(o.classes.intern(ClassExpr::Some(ObjProp::Named(p0), both)));
    o.axioms = vec![
        Axiom::SubClassOf(e1, some),
        Axiom::SubObjectPropertyOf(vec![ObjProp::Named(p0)], ObjProp::Named(p2)),
        Axiom::ObjectCharacteristic(Characteristic::Transitive, ObjProp::Named(p2)),
        Axiom::ObjectPropertyDomain(ObjProp::Named(p2), e2),
    ];
    o.sources = vec![Vec::new(); o.axioms.len()];
    let inclusion = 1;
    let normalised = normalise_with(&o, Options::default());
    let p0_transitions: Vec<&Clause> = normalised
        .clauses
        .iter()
        .filter(|clause| {
            clause
                .body
                .iter()
                .any(|b| matches!(b, BodyAtom::Role(r, ..) if *r == p0))
                && clause
                    .head
                    .iter()
                    .all(|h| matches!(h, HeadAtom::Concept(Concept::Fresh(_), _)))
        })
        .collect();
    // A p0-edge reaches p2's automaton through a transition on p0 or, where the inclusion
    // clause makes it a p2-edge too, through that clause: either way p0 ⊑ p2 is named.
    let through_inclusion = normalised.clauses.iter().any(|clause| {
        matches!(clause.body[..], [BodyAtom::Role(r, ..)] if r == p0)
            && matches!(clause.head[..], [HeadAtom::Role(r, ..)] if r == p2)
            && clause.sources.iter().all(|set| set.contains(&inclusion))
    });
    assert!(
        !p0_transitions.is_empty() || through_inclusion,
        "{:#?}",
        normalised.clauses
    );
    for clause in p0_transitions {
        assert!(
            clause.sources.iter().all(|set| set.contains(&inclusion)),
            "a p0-transition of p2's automaton without p0 ⊑ p2: {clause:?}"
        );
    }
}

#[test]
fn clauses_and_ontologies_have_the_same_models() {
    let env = |name: &str| std::env::var(name).ok().and_then(|v| v.parse::<u64>().ok());
    let cases = env("NRESE_FUZZ_CASES").unwrap_or(150);
    let mut next = rng(env("NRESE_FUZZ_SEED").unwrap_or(0x2026_1003_0222));
    let (mut compared, mut models) = (0u64, 0u64);
    for case in 0..cases {
        // Terms: classes 0..3, roles 10 (may be transitive) and 11 (simple), individuals 20, 21.
        let classes: Vec<Term> = vec![0, 1, 2];
        let (chained, simple) = (10, 11);
        let individuals: Vec<Term> = vec![20, 21];
        let mut o = Ontology::default();
        let transitive = next(2) == 0;
        let mut axioms: Vec<Axiom> = Vec::new();
        if transitive {
            axioms.push(Axiom::ObjectCharacteristic(
                Characteristic::Transitive,
                ObjProp::Named(chained),
            ));
            if next(2) == 0 {
                axioms.push(Axiom::SubObjectPropertyOf(
                    vec![ObjProp::Named(simple)],
                    ObjProp::Named(chained),
                ));
            }
            if next(3) == 0 {
                axioms.push(Axiom::SubObjectPropertyOf(
                    vec![ObjProp::Named(chained), ObjProp::Named(simple)],
                    ObjProp::Named(chained),
                ));
            }
        }
        // Any role for ∃ and ∀, the simple one for numbers and Self.
        let any_role = move |next: &mut dyn FnMut(u64) -> u64| {
            let p = if next(2) == 0 { chained } else { simple };
            if next(4) == 0 {
                ObjProp::Inverse(p)
            } else {
                ObjProp::Named(p)
            }
        };
        let simple_role = move |next: &mut dyn FnMut(u64) -> u64| {
            let p = if transitive {
                simple
            } else if next(2) == 0 {
                chained
            } else {
                simple
            };
            if next(4) == 0 {
                ObjProp::Inverse(p)
            } else {
                ObjProp::Named(p)
            }
        };
        fn class(
            o: &mut Ontology,
            next: &mut dyn FnMut(u64) -> u64,
            depth: u32,
            classes: &[Term],
            individuals: &[Term],
            any_role: &PickRole,
            simple_role: &PickRole,
        ) -> ExprId {
            let pick = if depth == 0 { next(3) } else { next(14) };
            let sub = |o: &mut Ontology, next: &mut dyn FnMut(u64) -> u64| {
                class(
                    o,
                    next,
                    depth.saturating_sub(1),
                    classes,
                    individuals,
                    any_role,
                    simple_role,
                )
            };
            let e = match pick {
                0 | 1 => ClassExpr::Class(classes[next(3) as usize]),
                2 => {
                    if next(2) == 0 {
                        ClassExpr::Thing
                    } else {
                        ClassExpr::Nothing
                    }
                }
                3 => {
                    let (a, b) = (sub(o, next), sub(o, next));
                    let mut v = vec![a, b];
                    v.sort();
                    v.dedup();
                    ClassExpr::And(v)
                }
                4 => {
                    let (a, b) = (sub(o, next), sub(o, next));
                    let mut v = vec![a, b];
                    v.sort();
                    v.dedup();
                    ClassExpr::Or(v)
                }
                5 => ClassExpr::Not(sub(o, next)),
                6 => ClassExpr::Some(any_role(next), sub(o, next)),
                7 => ClassExpr::All(any_role(next), sub(o, next)),
                8 => ClassExpr::Min(next(3) as u32, simple_role(next), sub(o, next)),
                9 => ClassExpr::Max(next(3) as u32, simple_role(next), sub(o, next)),
                10 => ClassExpr::Exact(next(2) as u32, simple_role(next), sub(o, next)),
                11 => ClassExpr::HasSelf(simple_role(next)),
                12 => ClassExpr::HasValue(any_role(next), individuals[next(2) as usize]),
                _ => {
                    let mut v = vec![individuals[next(2) as usize], individuals[next(2) as usize]];
                    v.sort();
                    v.dedup();
                    ClassExpr::OneOf(v)
                }
            };
            ExprId(o.classes.intern(e))
        }
        for _ in 0..1 + next(4) {
            let c = |o: &mut Ontology, next: &mut dyn FnMut(u64) -> u64, depth: u32| {
                class(
                    o,
                    next,
                    depth,
                    &classes,
                    &individuals,
                    &any_role,
                    &simple_role,
                )
            };
            let axiom = match next(16) {
                0..=3 => {
                    let (a, b) = (c(&mut o, &mut next, 2), c(&mut o, &mut next, 2));
                    Axiom::SubClassOf(a, b)
                }
                4 => {
                    let (a, b) = (c(&mut o, &mut next, 1), c(&mut o, &mut next, 1));
                    let mut v = vec![a, b];
                    v.sort();
                    v.dedup();
                    Axiom::EquivalentClasses(v)
                }
                5 => {
                    let (a, b) = (c(&mut o, &mut next, 1), c(&mut o, &mut next, 1));
                    let mut v = vec![a, b];
                    v.sort();
                    v.dedup();
                    Axiom::DisjointClasses(v)
                }
                6 => Axiom::ObjectPropertyDomain(any_role(&mut next), c(&mut o, &mut next, 1)),
                7 => Axiom::ObjectPropertyRange(any_role(&mut next), c(&mut o, &mut next, 1)),
                8 => Axiom::ObjectCharacteristic(
                    [
                        Characteristic::Functional,
                        Characteristic::InverseFunctional,
                        Characteristic::Reflexive,
                        Characteristic::Irreflexive,
                        Characteristic::Symmetric,
                        Characteristic::Asymmetric,
                    ][next(6) as usize],
                    simple_role(&mut next),
                ),
                9 => Axiom::ClassAssertion(c(&mut o, &mut next, 2), individuals[next(2) as usize]),
                10 => Axiom::ObjectPropertyAssertion(
                    if next(2) == 0 { chained } else { simple },
                    individuals[next(2) as usize],
                    individuals[next(2) as usize],
                ),
                11 => Axiom::NegativeObjectPropertyAssertion(
                    if transitive { simple } else { chained },
                    individuals[next(2) as usize],
                    individuals[next(2) as usize],
                ),
                13 => {
                    let union = vec![c(&mut o, &mut next, 1), c(&mut o, &mut next, 1)];
                    Axiom::DisjointUnion(classes[next(3) as usize], union)
                }
                // Role axioms: into the transitive role from the simple one, else any.
                14 if transitive => Axiom::SubObjectPropertyOf(
                    vec![if next(2) == 0 {
                        ObjProp::Named(simple)
                    } else {
                        ObjProp::Inverse(simple)
                    }],
                    if next(2) == 0 {
                        ObjProp::Named(chained)
                    } else {
                        ObjProp::Inverse(chained)
                    },
                ),
                14 => match next(4) {
                    0 => Axiom::InverseObjectProperties(
                        ObjProp::Named(chained),
                        ObjProp::Named(simple),
                    ),
                    1 => Axiom::EquivalentObjectProperties(vec![
                        ObjProp::Named(chained),
                        ObjProp::Named(simple),
                    ]),
                    2 => Axiom::DisjointObjectProperties(vec![
                        ObjProp::Named(chained),
                        simple_role(&mut next),
                    ]),
                    _ => Axiom::SubObjectPropertyOf(vec![any_role(&mut next)], any_role(&mut next)),
                },
                _ => {
                    if next(2) == 0 {
                        Axiom::SameIndividual(vec![20, 21])
                    } else {
                        Axiom::DifferentIndividuals(vec![20, 21])
                    }
                }
            };
            axioms.push(axiom);
        }
        // Symmetry on the transitive role would make it non-simple-symmetric: fine (an
        // inclusion); asymmetry and irreflexivity only on simple roles (generated so).
        axioms.sort();
        axioms.dedup();
        for &t in &classes {
            axioms.push(Axiom::Declaration(EntityKind::Class, t));
        }
        o.sources = vec![Vec::new(); axioms.len()];
        o.axioms = axioms;
        // Both encodings of `≤ n`: spelled out, and as at-most atoms.
        let normalised = normalise_with(
            &o,
            Options {
                expand_at_most_up_to: if case % 2 == 0 { 2 } else { 0 },
                // The clauses' models are the ontology's: not so when read one way.
                lazy_definitions: false,
                // Minimal automata accept the same words: the same models.
                exact_provenance: case % 3 != 0,
            },
        );
        assert!(
            normalised.unsupported.is_empty(),
            "{:?}",
            normalised.unsupported
        );
        // Provenance: every clause comes from an axiom that can produce it.
        for clause in &normalised.clauses {
            assert!(!clause.sources.is_empty(), "{clause:?}");
            for &s in clause.sources.iter().flatten() {
                assert!(
                    !matches!(o.axioms[s], Axiom::Declaration(..)),
                    "a clause from a declaration: {clause:?}"
                );
            }
        }
        let rias: Vec<(Vec<ObjProp>, ObjProp)> = o
            .axioms
            .iter()
            .filter_map(|a| match a {
                Axiom::SubObjectPropertyOf(chain, sup) => Some((chain.clone(), *sup)),
                Axiom::ObjectCharacteristic(Characteristic::Transitive, r) => {
                    Some((vec![*r, *r], *r))
                }
                Axiom::ObjectCharacteristic(Characteristic::Symmetric, r) => {
                    Some((vec![r.inverse()], *r))
                }
                _ => None,
            })
            .collect();
        for _ in 0..60 {
            let n = 1 + next(3) as u32;
            let mut i = Interp {
                n,
                concepts: classes.iter().map(|&t| (t, next(1 << n))).collect(),
                roles: [chained, simple]
                    .iter()
                    .map(|&p| (p, (0..n).map(|_| next(1 << n)).collect()))
                    .collect(),
                individuals: individuals
                    .iter()
                    .map(|&a| (a, next(n as u64) as u32))
                    .collect(),
                fresh: Vec::new(),
            };
            if transitive {
                // Closed under the role inclusions: where the automata are exact.
                loop {
                    let mut changed = false;
                    for (chain, sup) in &rias {
                        let got = compose(&i, chain);
                        for d in 0..n {
                            for e in 0..n {
                                if got[d as usize] & (1 << e) != 0 {
                                    let (a, b, p) = match sup {
                                        ObjProp::Named(p) => (d, e, *p),
                                        ObjProp::Inverse(p) => (e, d, *p),
                                    };
                                    let row = &mut i.roles.get_mut(&p).unwrap()[a as usize];
                                    if *row & (1 << b) == 0 {
                                        *row |= 1 << b;
                                        changed = true;
                                    }
                                }
                            }
                        }
                    }
                    if !changed {
                        break;
                    }
                }
            }
            let model = o.axioms.iter().all(|a| holds(&o, a, &i));
            extend(&normalised, &mut i);
            let extended = clauses_hold(&normalised, &i);
            compared += 1;
            models += u64::from(model);
            if model != extended {
                let name = |t: Term| format!("t{t}");
                let axioms: Vec<String> = o.axioms.iter().map(|a| o.functional(a, &name)).collect();
                let broken: Vec<String> = normalised
                    .clauses
                    .iter()
                    .filter_map(|c| violation(c, &i).map(|a| format!("{c:?} at {a:?}")))
                    .collect();
                panic!(
                    "case {case}: model of the ontology {model}, of the clauses {extended}\n\
                     ontology:\n{}\ninterpretation: {i:?}\nfresh: {:?}\nviolated: {broken:#?}\nclauses: {:?}\nfacts: {:?}",
                    axioms.join("\n"),
                    normalised.fresh,
                    normalised.clauses,
                    normalised.facts
                );
            }
            // Soundness without the polarity argument: no extension of a non-model.
            let bits = n as usize * normalised.fresh.len();
            if !model && bits <= 10 {
                for code in 0u64..(1 << bits) {
                    for q in 0..normalised.fresh.len() {
                        i.fresh[q] = (code >> (q * n as usize)) & i.all();
                    }
                    if clauses_hold(&normalised, &i) {
                        let name = |t: Term| format!("t{t}");
                        let axioms: Vec<String> =
                            o.axioms.iter().map(|a| o.functional(a, &name)).collect();
                        let failing: Vec<String> = o
                            .axioms
                            .iter()
                            .filter(|a| !holds(&o, a, &i))
                            .map(|a| o.functional(a, &name))
                            .collect();
                        panic!(
                            "case {case}: a non-model extends to a model of the clauses: {i:?}\n\
                             ontology:\n{}\nfailing: {failing:?}\nfresh: {:?}\nclauses: {:?}\nfacts: {:?}",
                            axioms.join("\n"),
                            normalised.fresh,
                            normalised.clauses,
                            normalised.facts
                        );
                    }
                }
            }
        }
    }
    eprintln!("{compared} interpretations compared, {models} models");
    assert!(models > 0 && models < compared, "{models} of {compared}");
}

/// A transitive role over forty transitive subroles, under thirty universals (the shape of
/// ore_ont_1066: one `DisjointClasses(A, ∃part_of.B)` made 4,451 clauses). Minimal automata
/// and transitions the role inclusions imply keep the clauses near a handful per universal
/// (docs/design/performance.md §0, P3); this fails at once if that is lost.
#[test]
fn universals_over_a_large_role_hierarchy_stay_small() {
    let mut o = Ontology::default();
    let r = 1000;
    let mut axioms = vec![Axiom::ObjectCharacteristic(
        Characteristic::Transitive,
        ObjProp::Named(r),
    )];
    for i in 1..=40 {
        let s = r + i;
        axioms.push(Axiom::ObjectCharacteristic(
            Characteristic::Transitive,
            ObjProp::Named(s),
        ));
        axioms.push(Axiom::SubObjectPropertyOf(
            vec![ObjProp::Named(s)],
            ObjProp::Named(r),
        ));
    }
    for j in 0..30u64 {
        let a = ExprId(o.classes.intern(ClassExpr::Class(2 * j)));
        let b = ExprId(o.classes.intern(ClassExpr::Class(2 * j + 1)));
        let some = ExprId(o.classes.intern(ClassExpr::Some(ObjProp::Named(r), b)));
        axioms.push(Axiom::DisjointClasses(vec![a, some]));
    }
    o.sources = vec![Vec::new(); axioms.len()];
    o.axioms = axioms;
    let count = |exact_provenance| {
        normalise_with(
            &o,
            Options {
                exact_provenance,
                ..Options::default()
            },
        )
        .clauses
        .len()
    };
    let (minimal, exact) = (count(false), count(true));
    // 220 and 4,990 on 5 October.
    assert!(minimal <= 400, "{minimal} clauses with minimal automata");
    assert!(exact <= 6000, "{exact} clauses with exact provenance");
}
