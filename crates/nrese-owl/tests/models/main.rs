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
    Axiom, BodyAtom, Characteristic, ClassExpr, Clause, Concept, DataRange, EntityKind, ExprId,
    Filler, FreshOf, HeadAtom, Interner, Normalised, ObjProp, Ontology, Options, RangeId, Term,
    Var, normalise_with,
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
    let covered = |models: u64, compared: u64| models > 0 && models < compared;
    // Draw on (up to ten times as many cases) until the run covers what the assertion
    // below asks for, so that coverage never depends on the seed (the NI fuzz test's
    // fix, a901e34; seed 66810 of the SPARQL differential tests, 12 October 2026).
    for case in 0..cases * 10 {
        if case >= cases && covered(models, compared) {
            break;
        }
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
                complements: false,
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
    assert!(covered(models, compared), "{models} of {compared}");
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

/// `C ≡ ∃R.D` over a transitive R: the reverse direction `∃R.D ⊑ C` is read as
/// `D ⊑ ∀R⁻.C`, Horn through R⁻'s automaton. Read as `∀R.¬D ⊔ C` it put a disjunction with
/// an empty body on every node, and the model grew without end (ore_ont_10212: gave up at
/// 60 s; 0.24 s after; docs/design/performance.md §0).
#[test]
fn existentials_over_transitive_roles_on_the_left_stay_horn() {
    let mut o = Ontology::default();
    let (r, s) = (1000, 1001);
    let class = |o: &mut Ontology, t| ExprId(o.classes.intern(ClassExpr::Class(t)));
    let (c, a, b) = (class(&mut o, 1), class(&mut o, 2), class(&mut o, 3));
    // D = A ⊓ ∃S.B: not a literal, as in the real definitions (∃bearer_of.Q ⊓ ∃part_of.…).
    let s_b = ExprId(o.classes.intern(ClassExpr::Some(ObjProp::Named(s), b)));
    let d = ExprId(o.classes.intern(ClassExpr::And(vec![a, s_b])));
    let some = ExprId(o.classes.intern(ClassExpr::Some(ObjProp::Named(r), d)));
    o.axioms = vec![
        Axiom::ObjectCharacteristic(Characteristic::Transitive, ObjProp::Named(r)),
        Axiom::EquivalentClasses(vec![c, some]),
    ];
    o.sources = vec![Vec::new(); o.axioms.len()];
    let normalised = normalise_with(&o, Options::default());
    let universal: Vec<&Clause> = normalised
        .clauses
        .iter()
        .filter(|clause| clause.body.is_empty() && clause.head.len() > 1)
        .collect();
    assert!(universal.is_empty(), "{universal:#?}");
}

/// An inverse pair with a transitive role is a regular role hierarchy: universals over
/// both go through their automata and nothing is unsupported (ore_ont_15971: `after`
/// inverse of `before`, `before` transitive, was "unsupported: an irregular role
/// hierarchy"; the hypertableau's answers on it are `nrese-dl`'s
/// `transitive_roles_through_automata`).
#[test]
fn an_inverse_pair_with_a_transitive_role_is_regular() {
    let (before, after) = (ObjProp::Named(200), ObjProp::Named(201));
    let mut o = Ontology::default();
    let c = ExprId(o.classes.intern(ClassExpr::Class(1)));
    let not_c = ExprId(o.classes.intern(ClassExpr::Not(c)));
    let all_before = ExprId(o.classes.intern(ClassExpr::All(before, not_c)));
    let all_after = ExprId(o.classes.intern(ClassExpr::All(after, not_c)));
    o.axioms = vec![
        Axiom::InverseObjectProperties(before, after),
        Axiom::ObjectCharacteristic(Characteristic::Transitive, before),
        Axiom::ClassAssertion(all_before, 100),
        Axiom::ClassAssertion(all_after, 102),
        Axiom::ObjectPropertyAssertion(200, 100, 101),
        Axiom::ObjectPropertyAssertion(201, 102, 101),
    ];
    o.sources = vec![Vec::new(); o.axioms.len()];
    let normalised = normalise_with(&o, Options::default());
    assert!(
        normalised.unsupported.is_empty(),
        "{:?}",
        normalised.unsupported
    );
}

/// A cardinality whose negation is past `u32` (review A-1201): `a : ¬(≤ 4294967295 r)`,
/// `a : ¬(= 4294967295 r)` and their data forms need `≥ 4294967296`. Over a two-valued
/// datatype the data one has no model; wrapped to `≥ 0` it would hold everywhere. The
/// normalisation leaves those axioms out; one below, their negation `≥ 4294967295` stays.
/// Unnegated, `≤ 4294967295` and `= 4294967295` need no `+ 1`: they stay, their at-most
/// atoms counting 4294967295 exactly (PR #40 review, R1).
#[test]
fn only_a_negation_past_u32_is_left_out() {
    let mut wrong = Vec::new();
    for (n, negated, left_out) in [
        (u32::MAX - 1, true, 0),
        (u32::MAX, true, 4),
        (u32::MAX, false, 0),
    ] {
        let mut o = Ontology::default();
        let range = RangeId(o.ranges.intern(DataRange::Datatype(30)));
        let mut e = |x| ExprId(o.classes.intern(x));
        let thing = e(ClassExpr::Thing);
        let restrictions = [
            ClassExpr::Max(n, ObjProp::Named(10), thing),
            ClassExpr::Exact(n, ObjProp::Named(10), thing),
            ClassExpr::DataMax(n, 20, range),
            ClassExpr::DataExact(n, 20, range),
        ]
        .map(|x| {
            let x = e(x);
            if negated { e(ClassExpr::Not(x)) } else { x }
        });
        o.axioms = restrictions.map(|c| Axiom::ClassAssertion(c, 100)).to_vec();
        o.sources = vec![Vec::new(); o.axioms.len()];
        let normalised = normalise_with(&o, Options::default());
        let heads: Vec<&HeadAtom> = normalised.clauses.iter().flat_map(|c| &c.head).collect();
        let has = |test: fn(&HeadAtom) -> bool| heads.iter().any(|h| test(h));
        let counted = [
            has(|h| matches!(h, HeadAtom::AtMost { n: u32::MAX, .. })),
            has(|h| matches!(h, HeadAtom::DataAtMost { n: u32::MAX, .. })),
        ];
        if normalised.unsupported.len() != left_out || (!negated && counted != [true, true]) {
            wrong.push(format!(
                "≤ {n}, negated {negated}: {:?} {counted:?}",
                normalised.unsupported
            ));
        }
    }
    assert!(wrong.is_empty(), "{}", wrong.join("\n"));
}

/// The count is refused exactly where the negation normal form needs it (PR #40's sol-loop,
/// S1 and S2): `a : ≤ 1 r.¬(≤ 4294967295 s)` puts the filler in negation normal form, which
/// is `≥ 4294967296 s`: left out; `b : ¬(≥ 0 r.(≤ 4294967295 s))` is `b : ⊥` whatever the
/// filler, which is never negated: kept; `¬(≤ 4294967295 s) ⊑ A` goes in as
/// `⊤ ⊑ (≤ 4294967295 s) ⊔ A`, its subclass negated (the second round, S1): kept.
#[test]
fn a_nested_negation_past_u32_is_left_out() {
    let mut o = Ontology::default();
    let mut e = |x| ExprId(o.classes.intern(x));
    let thing = e(ClassExpr::Thing);
    let a = e(ClassExpr::Class(1));
    let most = e(ClassExpr::Max(u32::MAX, ObjProp::Named(11), thing));
    let not_most = e(ClassExpr::Not(most));
    let one_not_most = e(ClassExpr::Max(1, ObjProp::Named(10), not_most));
    let none = e(ClassExpr::Min(0, ObjProp::Named(10), most));
    let not_none = e(ClassExpr::Not(none));
    o.axioms = vec![
        Axiom::ClassAssertion(one_not_most, 100),
        Axiom::ClassAssertion(not_none, 101),
        Axiom::SubClassOf(not_most, a),
    ];
    o.sources = vec![Vec::new(); o.axioms.len()];
    let unsupported = normalise_with(&o, Options::default()).unsupported;
    let left_out: Vec<usize> = unsupported.iter().map(|&(i, _)| i).collect();
    assert_eq!(left_out, [0], "{unsupported:?}");
}

/// A fresh name's definition that needs a count past `u32` (PR #40's sol-loop, second
/// round, S2): `a : ≤ 1 r.(≤ 4294967295 s)` and `b : ≤ 1 r.(≤ 4294967295 s)` share the name
/// of the restriction, whose definition negates the filler only when it is clausified, after
/// both took the name. Neither assertion is complete without it: both are left out.
#[test]
fn a_shared_definition_past_u32_leaves_out_every_user() {
    let mut o = Ontology::default();
    let mut e = |x| ExprId(o.classes.intern(x));
    let thing = e(ClassExpr::Thing);
    let most = e(ClassExpr::Max(u32::MAX, ObjProp::Named(11), thing));
    let one_most = e(ClassExpr::Max(1, ObjProp::Named(10), most));
    o.axioms = vec![
        Axiom::ClassAssertion(one_most, 100),
        Axiom::ClassAssertion(one_most, 101),
    ];
    o.sources = vec![Vec::new(); o.axioms.len()];
    let unsupported = normalise_with(&o, Options::default()).unsupported;
    let mut left_out: Vec<usize> = unsupported.iter().map(|&(i, _)| i).collect();
    left_out.sort_unstable();
    assert_eq!(left_out, [0, 1], "{unsupported:?}");
}

/// A shared definition that needs more variables than a `u16` numbers, and a universal over
/// an irregular role: every axiom that takes the name is left out, not only one (the name is
/// cached, so each other taker is weaker without its definition). Axioms that don't take it
/// stay: the Horn lower bound reads them.
#[test]
fn a_shared_definition_that_fails_leaves_out_every_user() {
    let mut wrong = Vec::new();
    // `B ⊑ ∃r.D` and `C ⊑ ∃s.D`, `D` a union of 65,537 universals; `B ⊑ C` takes nothing.
    let mut o = Ontology::default();
    let mut e = |x| ExprId(o.classes.intern(x));
    let a = e(ClassExpr::Class(1));
    let alls = (0..65_537)
        .map(|p| e(ClassExpr::All(ObjProp::Named(1_000 + p), a)))
        .collect();
    let d = e(ClassExpr::Or(alls));
    let (b, c) = (e(ClassExpr::Class(2)), e(ClassExpr::Class(3)));
    let rd = e(ClassExpr::Some(ObjProp::Named(10), d));
    let sd = e(ClassExpr::Some(ObjProp::Named(11), d));
    o.axioms = vec![
        Axiom::SubClassOf(b, rd),
        Axiom::SubClassOf(c, sd),
        Axiom::SubClassOf(b, c),
    ];
    o.sources = vec![Vec::new(); o.axioms.len()];
    let unsupported = normalise_with(&o, Options::default()).unsupported;
    let mut left_out: Vec<usize> = unsupported.iter().map(|&(i, _)| i).collect();
    left_out.sort_unstable();
    if left_out != [0, 1] {
        wrong.push(format!("too wide: {unsupported:?}"));
    }
    // `A ⊑ ∀R.C` and `B ⊑ ∀R.C` with `R ∘ S ⊑ S` and `S ∘ R ⊑ R` (irregular), and
    // `B ⊑ ∃T.(∀R.C ⊓ A)` and `A ⊑ ∃U.(∀R.C ⊓ A)`, which share a name whose definition
    // takes the automaton; `A ⊑ B` takes nothing.
    let mut o = Ontology::default();
    let mut e = |x| ExprId(o.classes.intern(x));
    let (r, s) = (ObjProp::Named(10), ObjProp::Named(11));
    let (a, b, c) = (
        e(ClassExpr::Class(1)),
        e(ClassExpr::Class(2)),
        e(ClassExpr::Class(3)),
    );
    let all = e(ClassExpr::All(r, c));
    let both = e(ClassExpr::And(vec![all, a]));
    let t_both = e(ClassExpr::Some(ObjProp::Named(12), both));
    let u_both = e(ClassExpr::Some(ObjProp::Named(13), both));
    o.axioms = vec![
        Axiom::SubObjectPropertyOf(vec![r, s], s),
        Axiom::SubObjectPropertyOf(vec![s, r], r),
        Axiom::SubClassOf(a, all),
        Axiom::SubClassOf(b, all),
        Axiom::SubClassOf(b, t_both),
        Axiom::SubClassOf(a, u_both),
        Axiom::SubClassOf(a, b),
    ];
    o.sources = vec![Vec::new(); o.axioms.len()];
    let unsupported = normalise_with(&o, Options::default()).unsupported;
    let mut left_out: Vec<usize> = unsupported.iter().map(|&(i, _)| i).collect();
    left_out.sort_unstable();
    if left_out != [2, 3, 4, 5] {
        wrong.push(format!("irregular: {unsupported:?}"));
    }
    // `A ⊑ ∀R.C ⊔ ∀T.⊤` is a tautology: it takes nothing and stays, whatever is read first
    // (Sol r1 S1 of the fix: the automaton `B` failed was taken before `∀T.⊤` was seen).
    let mut o = Ontology::default();
    let mut e = |x| ExprId(o.classes.intern(x));
    let (r, s) = (ObjProp::Named(10), ObjProp::Named(11));
    let (a, b, c) = (
        e(ClassExpr::Class(1)),
        e(ClassExpr::Class(2)),
        e(ClassExpr::Class(3)),
    );
    let thing = e(ClassExpr::Thing);
    let all = e(ClassExpr::All(r, c));
    let any = e(ClassExpr::All(ObjProp::Named(12), thing));
    let either = e(ClassExpr::Or(vec![all, any]));
    o.axioms = vec![
        Axiom::SubObjectPropertyOf(vec![r, s], s),
        Axiom::SubObjectPropertyOf(vec![s, r], r),
        Axiom::SubClassOf(a, either),
        Axiom::SubClassOf(b, all),
    ];
    o.sources = vec![Vec::new(); o.axioms.len()];
    let unsupported = normalise_with(&o, Options::default()).unsupported;
    let left_out: Vec<usize> = unsupported.iter().map(|&(i, _)| i).collect();
    if left_out != [3] {
        wrong.push(format!("tautology: {unsupported:?}"));
    }
    // `A ⊑ ≤ 0 T.(∃R.⊥)` and `B ⊑ ≤ 0 U.(∃R.⊥)` are tautologies too: the name of `∃R.⊥`
    // needs no definition, so takes no automaton (Sol r2 S2). And a key over `R` with no
    // named individual has no rule and takes nothing: only the keys' own mark (S3).
    let mut o = Ontology::default();
    let mut e = |x| ExprId(o.classes.intern(x));
    let (a, b, c) = (
        e(ClassExpr::Class(1)),
        e(ClassExpr::Class(2)),
        e(ClassExpr::Class(3)),
    );
    let nothing = e(ClassExpr::Nothing);
    let none = e(ClassExpr::Some(r, nothing));
    let t_none = e(ClassExpr::Max(0, ObjProp::Named(12), none));
    let u_none = e(ClassExpr::Max(0, ObjProp::Named(13), none));
    let some_c = e(ClassExpr::Some(r, c));
    o.axioms = vec![
        Axiom::SubObjectPropertyOf(vec![r, s], s),
        Axiom::SubObjectPropertyOf(vec![s, r], r),
        Axiom::SubClassOf(a, t_none),
        Axiom::SubClassOf(b, u_none),
        Axiom::HasKey(some_c, vec![r], Vec::new()),
    ];
    o.sources = vec![Vec::new(); o.axioms.len()];
    let unsupported = normalise_with(&o, Options::default()).unsupported;
    if unsupported
        .iter()
        .any(|&(i, why)| i != 4 || why.contains("irregular"))
    {
        wrong.push(format!("needed nothing: {unsupported:?}"));
    }
    assert!(wrong.is_empty(), "{}", wrong.join("\n"));
}

/// Clause and key variables past `u16` (review A-1203): a key over 65,537 data properties
/// or 65,536 object properties, and `⊤ ⊑ ∀p₀.A ⊔ … ⊔ ∀p₆₅₅₃₆.A`, need one variable more
/// than a `u16` numbers, so two would be one. The normalisation leaves such an axiom out;
/// one data property or universal fewer is normalised. (A key of 65,535 object properties,
/// the last that fits, builds its rule in time quadratic in them: `variables`'s own test.)
#[test]
fn clauses_and_keys_past_u16_are_left_out() {
    let key = |objects: u64, data: u64| {
        let mut o = Ontology::default();
        let thing = ExprId(o.classes.intern(ClassExpr::Thing));
        let objects = (0..objects).map(|p| ObjProp::Named(1_000 + p)).collect();
        let data = (0..data).map(|d| 200_000 + d).collect();
        o.axioms = vec![Axiom::HasKey(thing, objects, data)];
        o.sources = vec![Vec::new()];
        o
    };
    let universals = |m: u64| {
        let mut o = Ontology::default();
        let mut e = |x| ExprId(o.classes.intern(x));
        let (thing, a) = (e(ClassExpr::Thing), e(ClassExpr::Class(1)));
        let alls = (0..m)
            .map(|p| e(ClassExpr::All(ObjProp::Named(1_000 + p), a)))
            .collect();
        let or = e(ClassExpr::Or(alls));
        o.axioms = vec![Axiom::SubClassOf(thing, or)];
        o.sources = vec![Vec::new()];
        o
    };
    let mut wrong = Vec::new();
    for (what, o, too_wide) in [
        ("a key of 65,536 data properties", key(0, 65_536), false),
        ("a key of 65,537 data properties", key(0, 65_537), true),
        ("65,537 universals", universals(65_537), true),
        ("65,536 universals", universals(65_536), false),
        ("a key of 65,536 object properties", key(65_536, 0), true),
    ] {
        let unsupported = normalise_with(&o, Options::default()).unsupported;
        if unsupported
            .iter()
            .any(|(_, why)| why.contains("65535 variables"))
            != too_wide
        {
            wrong.push(format!("{what}: {unsupported:?}"));
        }
    }
    assert!(wrong.is_empty(), "{}", wrong.join("\n"));
}
