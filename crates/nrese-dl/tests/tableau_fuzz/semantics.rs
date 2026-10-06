//! The direct semantics on finite interpretations, independent of the clauses: the slow,
//! obviously correct reference (adapted from nrese-owl's model-equivalence test). Sets of
//! elements are bitmasks, so domains have at most 128 elements.

use std::collections::HashMap;

use nrese_owl::{Axiom, Characteristic, ClassExpr, ExprId, ObjProp, Ontology, Term};

#[derive(Debug, Clone, Default)]
pub struct Interp {
    pub n: u32,
    pub concepts: HashMap<Term, u128>,
    /// Per role, per element, its successors.
    pub roles: HashMap<Term, Vec<u128>>,
    pub individuals: HashMap<Term, u32>,
}

impl Interp {
    pub fn all(&self) -> u128 {
        if self.n >= 128 {
            u128::MAX
        } else {
            (1u128 << self.n) - 1
        }
    }

    fn ind(&self, a: Term) -> u32 {
        self.individuals.get(&a).copied().unwrap_or(0)
    }

    pub fn succ(&self, role: ObjProp, d: u32) -> u128 {
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

    pub fn add_edge(&mut self, role: Term, a: u32, b: u32) -> bool {
        let n = self.n as usize;
        let row = &mut self.roles.entry(role).or_insert_with(|| vec![0; n])[a as usize];
        let new = *row & (1 << b) == 0;
        *row |= 1 << b;
        new
    }
}

pub fn eval(o: &Ontology, e: ExprId, i: &Interp) -> u128 {
    let all = i.all();
    let each = |test: &dyn Fn(u32) -> bool| {
        (0..i.n)
            .filter(|&d| test(d))
            .fold(0u128, |m, d| m | (1 << d))
    };
    match o.classes.get(e.0) {
        ClassExpr::Class(t) => i.concepts.get(t).copied().unwrap_or(0),
        ClassExpr::Thing => all,
        ClassExpr::Nothing => 0,
        ClassExpr::And(xs) => xs.iter().fold(all, |m, &x| m & eval(o, x, i)),
        ClassExpr::Or(xs) => xs.iter().fold(0, |m, &x| m | eval(o, x, i)),
        ClassExpr::Not(x) => all & !eval(o, *x, i),
        ClassExpr::OneOf(xs) => xs.iter().fold(0, |m, a| m | (1 << i.ind(*a))),
        ClassExpr::Some(r, x) => {
            let c = eval(o, *x, i);
            each(&|d| i.succ(*r, d) & c != 0)
        }
        ClassExpr::All(r, x) => {
            let c = eval(o, *x, i);
            each(&|d| i.succ(*r, d) & !c == 0)
        }
        ClassExpr::HasValue(r, a) => each(&|d| i.succ(*r, d) & (1 << i.ind(*a)) != 0),
        ClassExpr::HasSelf(r) => each(&|d| i.succ(*r, d) & (1 << d) != 0),
        ClassExpr::Min(n, r, x) => {
            let c = eval(o, *x, i);
            each(&|d| (i.succ(*r, d) & c).count_ones() >= *n)
        }
        ClassExpr::Max(n, r, x) => {
            let c = eval(o, *x, i);
            each(&|d| (i.succ(*r, d) & c).count_ones() <= *n)
        }
        ClassExpr::Exact(n, r, x) => {
            let c = eval(o, *x, i);
            each(&|d| (i.succ(*r, d) & c).count_ones() == *n)
        }
        other => panic!("no data in these ontologies: {other:?}"),
    }
}

/// The pairs of a chain of roles, per element.
fn compose(i: &Interp, chain: &[ObjProp]) -> Vec<u128> {
    let mut reach: Vec<u128> = (0..i.n).map(|d| 1u128 << d).collect();
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

pub fn holds(o: &Ontology, axiom: &Axiom, i: &Interp) -> bool {
    let c = |e: ExprId| eval(o, e, i);
    let rel = |r: ObjProp| (0..i.n).map(|d| i.succ(r, d)).collect::<Vec<u128>>();
    let pairwise = |xs: &[ExprId]| {
        xs.iter()
            .enumerate()
            .all(|(k, &a)| xs[k + 1..].iter().all(|&b| c(a) & c(b) == 0))
    };
    match axiom {
        Axiom::Declaration(..) => true,
        Axiom::SubClassOf(a, b) => c(*a) & !c(*b) == 0,
        Axiom::EquivalentClasses(xs) => xs.windows(2).all(|w| c(w[0]) == c(w[1])),
        Axiom::DisjointClasses(xs) => pairwise(xs),
        Axiom::DisjointUnion(class, xs) => {
            let named = i.concepts.get(class).copied().unwrap_or(0);
            named == xs.iter().fold(0, |m, &x| m | c(x)) && pairwise(xs)
        }
        Axiom::SubObjectPropertyOf(chain, sup) => {
            let (got, have) = (compose(i, chain), rel(*sup));
            got.iter().zip(&have).all(|(g, h)| g & !h == 0)
        }
        Axiom::InverseObjectProperties(a, b) => rel(*a) == rel(b.inverse()),
        Axiom::EquivalentObjectProperties(ps) => ps.windows(2).all(|w| rel(w[0]) == rel(w[1])),
        Axiom::ObjectPropertyDomain(r, x) => {
            let d = (0..i.n)
                .filter(|&d| i.succ(*r, d) != 0)
                .fold(0u128, |m, d| m | (1 << d));
            d & !c(*x) == 0
        }
        Axiom::ObjectPropertyRange(r, x) => (0..i.n).all(|d| i.succ(*r, d) & !c(*x) == 0),
        Axiom::ObjectCharacteristic(kind, r) => (0..i.n).all(|d| {
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
        }),
        Axiom::DisjointObjectProperties(ps) => ps.iter().enumerate().all(|(k, &a)| {
            ps[k + 1..]
                .iter()
                .all(|&b| (0..i.n).all(|d| i.succ(a, d) & i.succ(b, d) == 0))
        }),
        Axiom::ClassAssertion(x, a) => c(*x) & (1 << i.ind(*a)) != 0,
        Axiom::ObjectPropertyAssertion(p, a, b) => i.edge(*p, i.ind(*a), i.ind(*b)),
        Axiom::NegativeObjectPropertyAssertion(p, a, b) => !i.edge(*p, i.ind(*a), i.ind(*b)),
        Axiom::SameIndividual(xs) => xs.windows(2).all(|w| i.ind(w[0]) == i.ind(w[1])),
        Axiom::DifferentIndividuals(xs) => xs
            .iter()
            .enumerate()
            .all(|(k, a)| xs[k + 1..].iter().all(|b| i.ind(*a) != i.ind(*b))),
        other => panic!("not generated: {other:?}"),
    }
}

pub fn model_of(o: &Ontology, i: &Interp) -> bool {
    o.axioms.iter().all(|a| holds(o, a, i))
}

/// Closes the roles under the role inclusions (chains, transitivity, inverses, symmetry):
/// the clauses encode these by automata, exact on closed interpretations.
pub fn close(o: &Ontology, i: &mut Interp) {
    let mut rias: Vec<(Vec<ObjProp>, ObjProp)> = Vec::new();
    for a in &o.axioms {
        match a {
            Axiom::SubObjectPropertyOf(chain, sup) => rias.push((chain.clone(), *sup)),
            Axiom::ObjectCharacteristic(Characteristic::Transitive, r) => {
                rias.push((vec![*r, *r], *r))
            }
            Axiom::ObjectCharacteristic(Characteristic::Symmetric, r) => {
                rias.push((vec![r.inverse()], *r))
            }
            Axiom::InverseObjectProperties(a, b) => {
                rias.push((vec![*a], b.inverse()));
                rias.push((vec![*b], a.inverse()));
            }
            Axiom::EquivalentObjectProperties(ps) => {
                for &a in ps {
                    for &b in ps {
                        if a != b {
                            rias.push((vec![a], b));
                        }
                    }
                }
            }
            _ => {}
        }
    }
    loop {
        let mut changed = false;
        for (chain, sup) in &rias {
            let got = compose(i, chain);
            for d in 0..i.n {
                for e in 0..i.n {
                    if got[d as usize] & (1 << e) != 0 {
                        let (a, b, p) = match sup {
                            ObjProp::Named(p) => (d, e, *p),
                            ObjProp::Inverse(p) => (e, d, *p),
                        };
                        changed |= i.add_edge(p, a, b);
                    }
                }
            }
        }
        if !changed {
            break;
        }
    }
}

/// A model of `o` over at most `max` elements, by exhaustive search where the space is
/// small and by `samples` random interpretations where it isn't.
pub fn find_model(
    o: &Ontology,
    classes: &[Term],
    roles: &[Term],
    individuals: &[Term],
    max: u32,
    samples: u64,
    rng: &mut nrese_owl::fuzz::Rng,
) -> Option<Interp> {
    for n in 1..=max {
        let bits = classes.len() as u32 * n + roles.len() as u32 * n * n;
        let inds = (n as u64).pow(individuals.len() as u32);
        let space = (1u64 << bits.min(63)).saturating_mul(inds);
        let exhaustive = bits <= 20 && space <= 1 << 20;
        let total = if exhaustive { space } else { samples };
        for k in 0..total {
            let (mut code, mut ind) = if exhaustive {
                (k % (1u64 << bits), k >> bits)
            } else {
                (rng.below(1 << bits.min(62)), rng.below(inds))
            };
            let mut i = Interp {
                n,
                ..Interp::default()
            };
            let mask = (1u64 << n) - 1;
            for &c in classes {
                i.concepts.insert(c, u128::from(code & mask));
                code >>= n;
            }
            for &r in roles {
                let rows = (0..n)
                    .map(|_| {
                        let row = u128::from(code & mask);
                        code >>= n;
                        row
                    })
                    .collect();
                i.roles.insert(r, rows);
            }
            for &a in individuals {
                i.individuals.insert(a, (ind % u64::from(n)) as u32);
                ind /= u64::from(n);
            }
            if model_of(o, &i) {
                return Some(i);
            }
        }
    }
    None
}

/// Whether a folded model `i` of the engine's confirms `o`: closed under the role
/// inclusions, as it is or with its loops untied ([`untie_loops`]).
pub fn confirms(o: &Ontology, mut i: Interp) -> bool {
    close(o, &mut i);
    model_of(o, &i)
        || untie_loops(&i).is_some_and(|mut u| {
            close(o, &mut u);
            model_of(o, &u)
        })
}

/// `i` with every self-loop untied: each looped element `v` becomes a cycle of three
/// copies with `v`'s concepts and `v`'s edges to the other elements, its loops running
/// along the cycle. Folding a blocked node onto its blocker can close a loop on one
/// element, which an irreflexive role (a property disjoint with its inverse, say)
/// forbids, while the unravelled model it stands for is a model (seed 94543, case 143);
/// three copies keep every non-counting constraint the element met. `None` without a
/// loop, or past 128 elements.
fn untie_loops(i: &Interp) -> Option<Interp> {
    let looped: Vec<u32> = (0..i.n)
        .filter(|&v| i.roles.values().any(|r| r[v as usize] & (1 << v) != 0))
        .collect();
    if looped.is_empty() || i.n as usize + 2 * looped.len() > 128 {
        return None;
    }
    let mut out = i.clone();
    out.n = i.n + 2 * looped.len() as u32;
    for rows in out.roles.values_mut() {
        rows.resize(out.n as usize, 0);
    }
    for (k, &v) in looped.iter().enumerate() {
        let copies = [v, i.n + 2 * k as u32, i.n + 2 * k as u32 + 1];
        for c in copies[1..].iter() {
            for set in out.concepts.values_mut() {
                if *set & (1 << v) != 0 {
                    *set |= 1 << c;
                }
            }
        }
        for (p, rows) in &i.roles {
            let row = rows[v as usize];
            let others = row & !(1 << v);
            let looping = row & (1 << v) != 0;
            let out_rows = out.roles.get_mut(p).expect("same roles");
            for (at, &c) in copies.iter().enumerate() {
                out_rows[c as usize] = others;
                if looping {
                    out_rows[c as usize] |= 1 << copies[(at + 1) % 3];
                }
            }
        }
    }
    Some(out)
}
