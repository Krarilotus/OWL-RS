//! Axiom entailment under OWL 2 DL (docs/design/owl2-dl.md, the contract's tasks):
//! `O ⊨ α` iff `O ∪ ¬α` is inconsistent, with `¬α` written as axioms over fresh
//! individuals where `α` is a terminological axiom (a subclass axiom `C ⊑ D` as
//! `(C ⊓ ¬D)(x)` for a fresh `x`; transitivity of `p` as `p(x, y), p(y, z), ¬p(x, z)`).
//! Each consistency test goes to the engine the ontology allows ([`super::consistency`]).
//!
//! It is the oracle of the exact services (`ExactGroundEntailment`: a ground atom is an
//! axiom), and of entailment checks of whole documents ([`crate::StoreService::entails_dl`]).

use nrese_owl::{Axiom, Characteristic, ClassExpr, DataRange, ExprId, ObjProp, Ontology, RangeId};

use super::consistency::{self, Budget, Verdict};

mod batch;
pub(crate) use batch::{Batch, Test};

/// Whether an axiom (or every axiom of a set) is entailed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Entailed {
    Yes,
    No,
    /// Not decided: a budget, a construct the engines lack, an axiom kind not reduced.
    Unknown(String),
}

impl Entailed {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Yes => "entailed",
            Self::No => "not-entailed",
            Self::Unknown(_) => "unknown",
        }
    }

    /// Both: yes if both are, no if either is, else unknown.
    fn and(self, other: Self) -> Self {
        match (self, other) {
            (Self::No, _) | (_, Self::No) => Self::No,
            (Self::Yes, Self::Yes) => Self::Yes,
            (Self::Unknown(why), _) | (_, Self::Unknown(why)) => Self::Unknown(why),
        }
    }
}

/// Copies class expressions and data ranges of one ontology into another's interners
/// (the terms are the same store ids in both).
struct Import<'a> {
    from: &'a Ontology,
    to: &'a mut Ontology,
}

impl Import<'_> {
    fn class(&mut self, id: ExprId) -> ExprId {
        let sorted = |mut v: Vec<ExprId>| {
            v.sort_unstable();
            v.dedup();
            v
        };
        let expr = match self.from.class(id).clone() {
            ClassExpr::And(v) => {
                ClassExpr::And(sorted(v.into_iter().map(|e| self.class(e)).collect()))
            }
            ClassExpr::Or(v) => {
                ClassExpr::Or(sorted(v.into_iter().map(|e| self.class(e)).collect()))
            }
            ClassExpr::Not(e) => ClassExpr::Not(self.class(e)),
            ClassExpr::Some(p, e) => ClassExpr::Some(p, self.class(e)),
            ClassExpr::All(p, e) => ClassExpr::All(p, self.class(e)),
            ClassExpr::Min(n, p, e) => ClassExpr::Min(n, p, self.class(e)),
            ClassExpr::Max(n, p, e) => ClassExpr::Max(n, p, self.class(e)),
            ClassExpr::Exact(n, p, e) => ClassExpr::Exact(n, p, self.class(e)),
            ClassExpr::DataSome(d, r) => ClassExpr::DataSome(d, self.range(r)),
            ClassExpr::DataAll(d, r) => ClassExpr::DataAll(d, self.range(r)),
            ClassExpr::DataMin(n, d, r) => ClassExpr::DataMin(n, d, self.range(r)),
            ClassExpr::DataMax(n, d, r) => ClassExpr::DataMax(n, d, self.range(r)),
            ClassExpr::DataExact(n, d, r) => ClassExpr::DataExact(n, d, self.range(r)),
            other => other,
        };
        ExprId(self.to.classes.intern(expr))
    }

    fn range(&mut self, id: RangeId) -> RangeId {
        let range = match self.from.range(id).clone() {
            DataRange::And(v) => {
                let mut v: Vec<RangeId> = v.into_iter().map(|r| self.range(r)).collect();
                v.sort_unstable();
                v.dedup();
                DataRange::And(v)
            }
            DataRange::Or(v) => {
                let mut v: Vec<RangeId> = v.into_iter().map(|r| self.range(r)).collect();
                v.sort_unstable();
                v.dedup();
                DataRange::Or(v)
            }
            DataRange::Not(r) => DataRange::Not(self.range(r)),
            other => other,
        };
        RangeId(self.to.ranges.intern(range))
    }

    fn axiom(&mut self, axiom: &Axiom) -> Axiom {
        let classes = |v: &[ExprId], me: &mut Self| v.iter().map(|&e| me.class(e)).collect();
        match axiom {
            Axiom::SubClassOf(a, b) => Axiom::SubClassOf(self.class(*a), self.class(*b)),
            Axiom::EquivalentClasses(v) => Axiom::EquivalentClasses(classes(v, self)),
            Axiom::DisjointClasses(v) => Axiom::DisjointClasses(classes(v, self)),
            Axiom::DisjointUnion(c, v) => Axiom::DisjointUnion(*c, classes(v, self)),
            Axiom::ObjectPropertyDomain(p, c) => Axiom::ObjectPropertyDomain(*p, self.class(*c)),
            Axiom::ObjectPropertyRange(p, c) => Axiom::ObjectPropertyRange(*p, self.class(*c)),
            Axiom::DataPropertyDomain(d, c) => Axiom::DataPropertyDomain(*d, self.class(*c)),
            Axiom::DataPropertyRange(d, r) => Axiom::DataPropertyRange(*d, self.range(*r)),
            Axiom::DatatypeDefinition(t, r) => Axiom::DatatypeDefinition(*t, self.range(*r)),
            Axiom::HasKey(c, p, d) => Axiom::HasKey(self.class(*c), p.clone(), d.clone()),
            Axiom::ClassAssertion(c, a) => Axiom::ClassAssertion(self.class(*c), *a),
            other => other.clone(),
        }
    }
}

/// The ontology's class expression `expr`, interned.
fn intern(o: &mut Ontology, expr: ClassExpr) -> ExprId {
    ExprId(o.classes.intern(expr))
}

/// The assertion `p(a, b)` for a property expression (an inverse swaps the two).
fn role(p: ObjProp, a: u64, b: u64) -> Axiom {
    match p {
        ObjProp::Named(p) => Axiom::ObjectPropertyAssertion(p, a, b),
        ObjProp::Inverse(p) => Axiom::ObjectPropertyAssertion(p, b, a),
    }
}

fn not_role(p: ObjProp, a: u64, b: u64) -> Axiom {
    match p {
        ObjProp::Named(p) => Axiom::NegativeObjectPropertyAssertion(p, a, b),
        ObjProp::Inverse(p) => Axiom::NegativeObjectPropertyAssertion(p, b, a),
    }
}

/// The test sets whose addition to `o` must each make it inconsistent for `axiom` (of
/// `o`'s interners) to be entailed; `Err` where the axiom kind isn't reduced. `fresh` are
/// individuals `o` doesn't name (at least three, more for long chains).
fn negations(o: &mut Ontology, axiom: &Axiom, fresh: &[u64]) -> Result<Vec<Vec<Axiom>>, String> {
    let [x, y, z] = [fresh[0], fresh[1], fresh[2]];
    let subclass = |o: &mut Ontology, c: ExprId, d: ExprId| {
        let not_d = intern(o, ClassExpr::Not(d));
        let both = intern(o, ClassExpr::And(sorted_pair(c, not_d)));
        vec![Axiom::ClassAssertion(both, x)]
    };
    Ok(match axiom.clone() {
        // Declarations hold no logical content.
        Axiom::Declaration(..) => Vec::new(),
        Axiom::ClassAssertion(c, a) => {
            let not = intern(o, ClassExpr::Not(c));
            vec![vec![Axiom::ClassAssertion(not, a)]]
        }
        Axiom::SubClassOf(c, d) => vec![subclass(o, c, d)],
        Axiom::EquivalentClasses(v) => {
            let mut out = Vec::new();
            for &c in &v {
                for &d in &v {
                    if c != d {
                        out.push(subclass(o, c, d));
                    }
                }
            }
            out
        }
        Axiom::DisjointClasses(v) => {
            let mut out = Vec::new();
            for (i, &c) in v.iter().enumerate() {
                for &d in &v[i + 1..] {
                    let both = intern(o, ClassExpr::And(sorted_pair(c, d)));
                    out.push(vec![Axiom::ClassAssertion(both, x)]);
                }
            }
            out
        }
        Axiom::ObjectPropertyDomain(p, c) => {
            let thing = intern(o, ClassExpr::Thing);
            let some = intern(o, ClassExpr::Some(p, thing));
            vec![subclass(o, some, c)]
        }
        Axiom::ObjectPropertyRange(p, c) => {
            let not = intern(o, ClassExpr::Not(c));
            let some = intern(o, ClassExpr::Some(p, not));
            vec![vec![Axiom::ClassAssertion(some, x)]]
        }
        Axiom::DataPropertyDomain(d, c) => {
            let literal = RangeId(o.ranges.intern(DataRange::Literal));
            let some = intern(o, ClassExpr::DataSome(d, literal));
            vec![subclass(o, some, c)]
        }
        Axiom::DataPropertyRange(d, r) => {
            let not = RangeId(o.ranges.intern(DataRange::Not(r)));
            let some = intern(o, ClassExpr::DataSome(d, not));
            vec![vec![Axiom::ClassAssertion(some, x)]]
        }
        Axiom::ObjectPropertyAssertion(p, a, b) => {
            vec![vec![Axiom::NegativeObjectPropertyAssertion(p, a, b)]]
        }
        Axiom::NegativeObjectPropertyAssertion(p, a, b) => {
            vec![vec![Axiom::ObjectPropertyAssertion(p, a, b)]]
        }
        Axiom::DataPropertyAssertion(p, a, v) => {
            vec![vec![Axiom::NegativeDataPropertyAssertion(p, a, v)]]
        }
        Axiom::NegativeDataPropertyAssertion(p, a, v) => {
            vec![vec![Axiom::DataPropertyAssertion(p, a, v)]]
        }
        Axiom::SameIndividual(v) => pairs(&v)
            .map(|(a, b)| vec![Axiom::DifferentIndividuals(vec![a, b])])
            .collect(),
        Axiom::DifferentIndividuals(v) => pairs(&v)
            .map(|(a, b)| vec![Axiom::SameIndividual(vec![a, b])])
            .collect(),
        Axiom::SubObjectPropertyOf(chain, q) => {
            if chain.len() + 1 > fresh.len() {
                return Err("a property chain longer than the fresh individuals".to_owned());
            }
            let mut test: Vec<Axiom> = chain
                .iter()
                .enumerate()
                .map(|(i, &p)| role(p, fresh[i], fresh[i + 1]))
                .collect();
            test.push(not_role(q, fresh[0], fresh[chain.len()]));
            vec![test]
        }
        Axiom::EquivalentObjectProperties(v) => {
            let mut out = Vec::new();
            for &p in &v {
                for &q in &v {
                    if p != q {
                        out.push(vec![role(p, x, y), not_role(q, x, y)]);
                    }
                }
            }
            out
        }
        Axiom::InverseObjectProperties(p, q) => vec![
            vec![role(p, x, y), not_role(q, y, x)],
            vec![role(q, x, y), not_role(p, y, x)],
        ],
        Axiom::DisjointObjectProperties(v) => pairs(&v)
            .map(|(p, q)| vec![role(p, x, y), role(q, x, y)])
            .collect(),
        Axiom::ObjectCharacteristic(c, p) => vec![match c {
            Characteristic::Transitive => vec![role(p, x, y), role(p, y, z), not_role(p, x, z)],
            Characteristic::Reflexive => vec![not_role(p, x, x)],
            Characteristic::Irreflexive => vec![role(p, x, x)],
            Characteristic::Symmetric => vec![role(p, x, y), not_role(p, y, x)],
            Characteristic::Asymmetric => vec![role(p, x, y), role(p, y, x)],
            Characteristic::Functional => vec![
                role(p, x, y),
                role(p, x, z),
                Axiom::DifferentIndividuals(vec![y, z]),
            ],
            Characteristic::InverseFunctional => vec![
                role(p, y, x),
                role(p, z, x),
                Axiom::DifferentIndividuals(vec![y, z]),
            ],
        }],
        Axiom::FunctionalDataProperty(d) => {
            let literal = RangeId(o.ranges.intern(DataRange::Literal));
            let two = intern(o, ClassExpr::DataMin(2, d, literal));
            vec![vec![Axiom::ClassAssertion(two, x)]]
        }
        other => {
            return Err(format!(
                "entailment of {other:?} isn't reduced to consistency"
            ));
        }
    })
}

fn sorted_pair(a: ExprId, b: ExprId) -> Vec<ExprId> {
    let mut v = vec![a, b];
    v.sort_unstable();
    v.dedup();
    v
}

fn pairs<T: Copy>(v: &[T]) -> impl Iterator<Item = (T, T)> + '_ {
    v.iter()
        .enumerate()
        .flat_map(move |(i, &a)| v[i + 1..].iter().map(move |&b| (a, b)))
}

/// Whether `premise` (consistent) entails `axiom`, an axiom over `premise`'s interners.
/// `fresh` are individuals the premise doesn't name (three or more).
pub fn entails(premise: &Ontology, axiom: &Axiom, fresh: &[u64], budget: &Budget) -> Entailed {
    let mut o = premise.clone();
    let tests = match negations(&mut o, axiom, fresh) {
        Ok(tests) => tests,
        Err(why) => return Entailed::Unknown(why),
    };
    let base = o.axioms.len();
    let mut answer = Entailed::Yes;
    for test in tests {
        o.axioms.truncate(base);
        o.sources.truncate(base);
        for a in test {
            o.axioms.push(a);
            o.sources.push(Vec::new());
        }
        let found = match consistency::check(&o, budget).verdict {
            Verdict::Inconsistent => Entailed::Yes,
            Verdict::Consistent => Entailed::No,
            Verdict::Unknown(why) => Entailed::Unknown(why),
        };
        answer = answer.and(found);
        if answer == Entailed::No {
            break;
        }
    }
    answer
}

/// Whether `premise` (consistent) entails that some individual is an instance of
/// `class` (of `premise`'s interners): `class ⊑ ⊥` makes it inconsistent.
pub fn nonempty(premise: &Ontology, class: ExprId, budget: &Budget) -> Entailed {
    let mut o = premise.clone();
    let nothing = intern(&mut o, ClassExpr::Nothing);
    o.axioms.push(Axiom::SubClassOf(class, nothing));
    o.sources.push(Vec::new());
    match consistency::check(&o, budget).verdict {
        Verdict::Inconsistent => Entailed::Yes,
        Verdict::Consistent => Entailed::No,
        Verdict::Unknown(why) => Entailed::Unknown(why),
    }
}

/// Whether `premise` (consistent) entails every logical axiom of `conclusion` (an
/// ontology read over the same term ids).
pub fn entails_ontology(
    premise: &Ontology,
    conclusion: &Ontology,
    fresh: &[u64],
    budget: &Budget,
) -> Entailed {
    let mut o = premise.clone();
    o.data.extend(&conclusion.data);
    let axioms: Vec<Axiom> = {
        let mut import = Import {
            from: conclusion,
            to: &mut o,
        };
        conclusion.axioms.iter().map(|a| import.axiom(a)).collect()
    };
    let tests: Vec<_> = axioms.iter().cloned().map(Test::Axiom).collect();
    let started = std::time::Instant::now();
    let compiled = Batch::new(&mut o, &tests);
    let mut answer = Entailed::Yes;
    for (index, axiom) in axioms.iter().enumerate() {
        let remaining = Budget {
            timeout: budget.timeout.saturating_sub(started.elapsed()),
            ..budget.clone()
        };
        let found = compiled.check(index, &remaining).unwrap_or_else(|| {
            let remaining = Budget {
                timeout: budget.timeout.saturating_sub(started.elapsed()),
                ..budget.clone()
            };
            entails(&o, axiom, fresh, &remaining)
        });
        answer = answer.and(found);
        if answer == Entailed::No {
            break;
        }
    }
    answer
}
