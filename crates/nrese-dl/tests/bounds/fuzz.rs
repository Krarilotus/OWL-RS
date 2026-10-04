//! U1 on fuzzed ontologies with small ABoxes (`nrese_owl::fuzz`, SROIQ without data),
//! read back from their RDF as the store reads them:
//!
//! - **A clash-free U1 is a model.** Its closure, with `owl:sameAs` classes as elements,
//!   is a finite interpretation; every axiom of the ontology must hold in it, checked by
//!   brute force over its elements. Then U1 contains every certain answer, class and role
//!   atoms alike: an atom U1 lacks is false in this model (PAGOdA, Theorem 5.10's
//!   argument for str(K)).
//! - **L ⊆ U1** on the answers, where L has no violation (an inconsistent ontology entails
//!   everything, and RL then derives every class at an `owl:Nothing` member).
//! - **An OWL 2 RL violation means a clash in U1** (L is sound, so the ontology is
//!   inconsistent, and Theorem 5.5 (i) says U1 derives `⊥s`).
//!
//! `NRESE_FUZZ_CASES` and `NRESE_FUZZ_SEED` change the run.

use std::collections::HashMap;

use nrese_dl::bounds::{Bounds, Program};
use nrese_owl::fuzz::{self, Name, Profile, Rng, Signature, Sizes};
use nrese_owl::{Axiom, Characteristic, ClassExpr, ExprId, ObjProp, Ontology, Term, Vocabulary};

use super::support::{self, Table, Triple};

/// A finite interpretation over at most 128 elements, sets as bitmasks.
struct Interp {
    n: u32,
    concepts: HashMap<Term, u128>,
    /// Per role, per element, its successors.
    roles: HashMap<Term, Vec<u128>>,
    element: HashMap<Term, u32>,
}

impl Interp {
    fn all(&self) -> u128 {
        if self.n == 128 {
            u128::MAX
        } else {
            (1u128 << self.n) - 1
        }
    }

    fn succ(&self, role: ObjProp, d: u32) -> u128 {
        match role {
            ObjProp::Named(p) => self.roles.get(&p).map_or(0, |r| r[d as usize]),
            ObjProp::Inverse(p) => self.roles.get(&p).map_or(0, |r| {
                (0..self.n)
                    .filter(|&e| r[e as usize] & (1 << d) != 0)
                    .fold(0, |m, e| m | (1 << e))
            }),
        }
    }

    fn each(&self, test: &dyn Fn(u32) -> bool) -> u128 {
        (0..self.n)
            .filter(|&d| test(d))
            .fold(0, |m, d| m | (1 << d))
    }

    fn of(&self, a: Term) -> u128 {
        1 << self.element[&a]
    }

    fn eval(&self, o: &Ontology, e: ExprId) -> u128 {
        let all = self.all();
        match o.class(e) {
            ClassExpr::Class(t) => self.concepts.get(t).copied().unwrap_or(0),
            ClassExpr::Thing => all,
            ClassExpr::Nothing => 0,
            ClassExpr::And(xs) => xs.iter().fold(all, |m, &x| m & self.eval(o, x)),
            ClassExpr::Or(xs) => xs.iter().fold(0, |m, &x| m | self.eval(o, x)),
            ClassExpr::Not(x) => all & !self.eval(o, *x),
            ClassExpr::OneOf(xs) => xs.iter().fold(0, |m, &a| m | self.of(a)),
            ClassExpr::Some(r, x) => {
                let c = self.eval(o, *x);
                self.each(&|d| self.succ(*r, d) & c != 0)
            }
            ClassExpr::All(r, x) => {
                let c = self.eval(o, *x);
                self.each(&|d| self.succ(*r, d) & !c == 0)
            }
            ClassExpr::HasValue(r, a) => self.each(&|d| self.succ(*r, d) & self.of(*a) != 0),
            ClassExpr::HasSelf(r) => self.each(&|d| self.succ(*r, d) & (1 << d) != 0),
            ClassExpr::Min(n, r, x) => {
                let c = self.eval(o, *x);
                self.each(&|d| (self.succ(*r, d) & c).count_ones() >= *n)
            }
            ClassExpr::Max(n, r, x) => {
                let c = self.eval(o, *x);
                self.each(&|d| (self.succ(*r, d) & c).count_ones() <= *n)
            }
            ClassExpr::Exact(n, r, x) => {
                let c = self.eval(o, *x);
                self.each(&|d| (self.succ(*r, d) & c).count_ones() == *n)
            }
            other => panic!("no data in these ontologies: {other:?}"),
        }
    }

    /// The pairs `(a, b)` of a chain of roles, per `a`.
    fn compose(&self, chain: &[ObjProp]) -> Vec<u128> {
        let mut reach: Vec<u128> = (0..self.n).map(|d| 1u128 << d).collect();
        for &r in chain {
            reach = reach
                .iter()
                .map(|&from| {
                    (0..self.n)
                        .filter(|&d| from & (1 << d) != 0)
                        .fold(0, |m, d| m | self.succ(r, d))
                })
                .collect();
        }
        reach
    }

    fn rel(&self, r: ObjProp) -> Vec<u128> {
        (0..self.n).map(|d| self.succ(r, d)).collect()
    }

    fn holds(&self, o: &Ontology, axiom: &Axiom) -> bool {
        let c = |e: ExprId| self.eval(o, e);
        let disjoint = |xs: &[u128]| {
            xs.iter()
                .enumerate()
                .all(|(k, a)| xs[k + 1..].iter().all(|b| a & b == 0))
        };
        match axiom {
            Axiom::Declaration(..) => true,
            Axiom::SubClassOf(a, b) => c(*a) & !c(*b) == 0,
            Axiom::EquivalentClasses(xs) => xs.windows(2).all(|w| c(w[0]) == c(w[1])),
            Axiom::DisjointClasses(xs) => disjoint(&xs.iter().map(|&x| c(x)).collect::<Vec<_>>()),
            Axiom::DisjointUnion(class, xs) => {
                let parts: Vec<u128> = xs.iter().map(|&x| c(x)).collect();
                let named = self.concepts.get(class).copied().unwrap_or(0);
                named == parts.iter().fold(0, |m, p| m | p) && disjoint(&parts)
            }
            Axiom::SubObjectPropertyOf(chain, sup) => {
                let (got, have) = (self.compose(chain), self.rel(*sup));
                got.iter().zip(&have).all(|(g, h)| g & !h == 0)
            }
            Axiom::InverseObjectProperties(a, b) => self.rel(*a) == self.rel(b.inverse()),
            Axiom::EquivalentObjectProperties(ps) => {
                ps.windows(2).all(|w| self.rel(w[0]) == self.rel(w[1]))
            }
            Axiom::DisjointObjectProperties(ps) => ps.iter().enumerate().all(|(k, &a)| {
                ps[k + 1..]
                    .iter()
                    .all(|&b| (0..self.n).all(|d| self.succ(a, d) & self.succ(b, d) == 0))
            }),
            Axiom::ObjectPropertyDomain(r, x) => {
                self.each(&|d| self.succ(*r, d) != 0) & !c(*x) == 0
            }
            Axiom::ObjectPropertyRange(r, x) => (0..self.n).all(|d| self.succ(*r, d) & !c(*x) == 0),
            Axiom::ObjectCharacteristic(kind, r) => (0..self.n).all(|d| {
                let s = self.succ(*r, d);
                match kind {
                    Characteristic::Functional => s.count_ones() <= 1,
                    Characteristic::InverseFunctional => {
                        self.succ(r.inverse(), d).count_ones() <= 1
                    }
                    Characteristic::Reflexive => s & (1 << d) != 0,
                    Characteristic::Irreflexive => s & (1 << d) == 0,
                    Characteristic::Symmetric => s == self.succ(r.inverse(), d),
                    Characteristic::Asymmetric => s & self.succ(r.inverse(), d) == 0,
                    Characteristic::Transitive => self.compose(&[*r, *r])[d as usize] & !s == 0,
                }
            }),
            Axiom::ClassAssertion(x, a) => c(*x) & self.of(*a) != 0,
            Axiom::ObjectPropertyAssertion(p, a, b) => {
                self.succ(ObjProp::Named(*p), self.element[a]) & self.of(*b) != 0
            }
            Axiom::NegativeObjectPropertyAssertion(p, a, b) => {
                self.succ(ObjProp::Named(*p), self.element[a]) & self.of(*b) == 0
            }
            Axiom::SameIndividual(xs) => xs.windows(2).all(|w| self.of(w[0]) == self.of(w[1])),
            Axiom::DifferentIndividuals(xs) => {
                disjoint(&xs.iter().map(|&a| self.of(a)).collect::<Vec<_>>())
            }
            other => panic!("not generated without data: {other:?}"),
        }
    }
}

/// U1's closure as an interpretation: `owl:sameAs` classes are the elements. `None` if
/// there are more than 128.
fn interpretation(program: &Program, closure: &[Triple]) -> Option<Interp> {
    let names = &program.names;
    let sig = &program.signature;
    let mut parent: HashMap<Term, Term> = HashMap::new();
    fn find(parent: &mut HashMap<Term, Term>, x: Term) -> Term {
        let p = *parent.entry(x).or_insert(x);
        if p == x {
            return x;
        }
        let root = find(parent, p);
        parent.insert(x, root);
        root
    }
    let relevant = |t: &Triple| {
        (t[1] == names.rdf_type && sig.classes.contains(&t[2]))
            || sig.object_properties.contains(&t[1])
    };
    let mut terms: Vec<Term> = sig.individuals.clone();
    for t in closure {
        if t[1] == names.same_as {
            let (a, b) = (find(&mut parent, t[0]), find(&mut parent, t[2]));
            parent.insert(a, b);
        }
        if relevant(t) {
            terms.push(t[0]);
            if t[1] != names.rdf_type {
                terms.push(t[2]);
            }
        }
    }
    let mut element: HashMap<Term, u32> = HashMap::new();
    let mut roots: HashMap<Term, u32> = HashMap::new();
    for t in terms {
        let root = find(&mut parent, t);
        let next = roots.len() as u32;
        let e = *roots.entry(root).or_insert(next);
        element.insert(t, e);
    }
    let n = u32::try_from(roots.len()).ok().filter(|&n| n <= 128)?;
    let mut i = Interp {
        n,
        concepts: HashMap::new(),
        roles: HashMap::new(),
        element,
    };
    for t in closure.iter().filter(|t| relevant(t)) {
        let s = i.element[&t[0]];
        if t[1] == names.rdf_type {
            *i.concepts.entry(t[2]).or_default() |= 1 << s;
        } else {
            let o = i.element[&t[2]];
            i.roles.entry(t[1]).or_insert_with(|| vec![0; n as usize])[s as usize] |= 1 << o;
        }
    }
    Some(i)
}

/// What a fuzzed case came to.
#[derive(Debug, Default)]
struct Tally {
    cases: u64,
    models: u64,
    clashes: u64,
    violations: u64,
    too_large: u64,
}

#[test]
fn u1_is_a_model_when_clash_free_and_contains_l() {
    let env = |name: &str| std::env::var(name).ok().and_then(|v| v.parse::<u64>().ok());
    let cases = env("NRESE_FUZZ_CASES").unwrap_or(300);
    let seed = env("NRESE_FUZZ_SEED").unwrap_or(0x2026_1003_0322);
    let mut tally = Tally::default();
    let mut failures = Vec::new();
    for case in 0..cases {
        let mut rng = Rng::new(seed.wrapping_add(case));
        let mut table = Table::default();
        let sizes = Sizes {
            classes: 3,
            object_properties: 3,
            simple: 2,
            data_properties: 0,
            individuals: 3,
            literals: 0,
        };
        let sig = Signature::new(sizes, &mut |name| match name {
            Name::Iri(iri) => table.named(iri),
            Name::Integer(i) => nrese_owl::Make::literal(
                &mut table,
                &i.to_string(),
                "http://www.w3.org/2001/XMLSchema#integer",
            ),
        });
        let profile = Profile {
            axioms: 2 + rng.below(6) as usize,
            data: false,
            ..Profile::sroiq()
        };
        let generated = fuzz::ontology(&mut rng, &sig, profile);
        for (_, iri) in Vocabulary::iris() {
            table.named(&iri);
        }
        let vocabulary = Vocabulary::new(&|iri| nrese_owl::Terms::iri(&table, iri));
        let triples = nrese_owl::write(&generated, &vocabulary, &mut table);
        let (ontology, normalised) = support::read(&mut table, &triples);
        let program = support::compile(&mut table, &ontology, &normalised);
        let lower = support::lower(&mut table, &triples);
        let upper = support::upper(&mut table, &program, &triples);
        let bounds = Bounds::new(&program, &table, lower.facts.clone(), upper.facts.clone());
        tally.cases += 1;
        let render = |a: &Axiom| ontology.functional(a, &|t| table.text(t));
        // An inconsistent ontology (an RL violation) entails everything, so L may exceed U1.
        let missing = bounds.lower_not_in_upper();
        if !missing.is_empty() && lower.violations.is_empty() {
            failures.push(format!(
                "case {case}: L not in U1: {:?}\n  in {:?}",
                missing
                    .iter()
                    .map(|t| t.map(|x| table.text(x)))
                    .collect::<Vec<_>>(),
                ontology
                    .axioms
                    .iter()
                    .filter(|a| !matches!(a, Axiom::Declaration(..)))
                    .map(render)
                    .collect::<Vec<_>>()
            ));
        }
        if !lower.violations.is_empty() {
            tally.violations += 1;
            if bounds.clashes().is_empty() {
                failures.push(format!(
                    "case {case}: L violates {:?} but U1 has no clash",
                    lower.violations
                ));
            }
        }
        if !bounds.clashes().is_empty() {
            tally.clashes += 1;
            continue;
        }
        if !program.incomplete.is_empty() {
            failures.push(format!("case {case}: incomplete {:?}", program.incomplete));
        }
        // The closure over representatives, with each individual tied to its own.
        let mut closure = upper.representatives.clone();
        for &a in &program.signature.individuals {
            closure.push([a, program.names.same_as, upper.classes.representative(a)]);
        }
        let Some(model) = interpretation(&program, &closure) else {
            tally.too_large += 1;
            continue;
        };
        tally.models += 1;
        let broken: Vec<String> = ontology
            .axioms
            .iter()
            .filter(|a| !model.holds(&ontology, a))
            .map(render)
            .collect();
        if !broken.is_empty() {
            failures.push(format!("case {case}: U1 is no model; fails {broken:?}"));
        }
    }
    eprintln!("{tally:?}");
    assert!(failures.is_empty(), "{}", failures.join("\n"));
    assert!(
        tally.models * 3 >= tally.cases,
        "too few clash-free cases: {tally:?}"
    );
}
