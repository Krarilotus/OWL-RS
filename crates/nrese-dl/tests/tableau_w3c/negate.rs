//! Entailment as unsatisfiability: `O ⊨ α` iff `O ∪ ¬α` is inconsistent, with `¬α` as
//! assertions about fresh individuals. An axiom whose negation needs more than one case
//! (an equivalence, n-ary axioms) is entailed iff every case is inconsistent.

use nrese_owl::{
    Axiom, Characteristic, ClassExpr, DataRange, ExprId, ObjProp, Ontology, RangeId, Term,
};

/// Fresh individuals for the negations, above every term of the test.
pub const FRESH: Term = 1 << 40;

/// Copies an expression of `from` into `to`'s interner.
pub fn copy(from: &Ontology, e: ExprId, to: &mut Ontology) -> ExprId {
    let c = |x: ExprId, to: &mut Ontology| copy(from, x, to);
    let expr = match from.classes.get(e.0).clone() {
        ClassExpr::And(xs) => ClassExpr::And(sorted(xs.iter().map(|&x| c(x, to)).collect())),
        ClassExpr::Or(xs) => ClassExpr::Or(sorted(xs.iter().map(|&x| c(x, to)).collect())),
        ClassExpr::Not(x) => ClassExpr::Not(c(x, to)),
        ClassExpr::Some(r, x) => ClassExpr::Some(r, c(x, to)),
        ClassExpr::All(r, x) => ClassExpr::All(r, c(x, to)),
        ClassExpr::Min(n, r, x) => ClassExpr::Min(n, r, c(x, to)),
        ClassExpr::Max(n, r, x) => ClassExpr::Max(n, r, c(x, to)),
        ClassExpr::Exact(n, r, x) => ClassExpr::Exact(n, r, c(x, to)),
        ClassExpr::DataSome(p, r) => ClassExpr::DataSome(p, copy_range(from, r, to)),
        ClassExpr::DataAll(p, r) => ClassExpr::DataAll(p, copy_range(from, r, to)),
        ClassExpr::DataMin(n, p, r) => ClassExpr::DataMin(n, p, copy_range(from, r, to)),
        ClassExpr::DataMax(n, p, r) => ClassExpr::DataMax(n, p, copy_range(from, r, to)),
        ClassExpr::DataExact(n, p, r) => ClassExpr::DataExact(n, p, copy_range(from, r, to)),
        other => other,
    };
    ExprId(to.classes.intern(expr))
}

fn copy_range(from: &Ontology, r: RangeId, to: &mut Ontology) -> RangeId {
    let range = match from.ranges.get(r.0).clone() {
        DataRange::And(xs) => DataRange::And(sorted(
            xs.iter().map(|&x| copy_range(from, x, to)).collect(),
        )),
        DataRange::Or(xs) => DataRange::Or(sorted(
            xs.iter().map(|&x| copy_range(from, x, to)).collect(),
        )),
        DataRange::Not(x) => DataRange::Not(copy_range(from, x, to)),
        other => other,
    };
    RangeId(to.ranges.intern(range))
}

fn sorted<T: Ord>(mut v: Vec<T>) -> Vec<T> {
    v.sort();
    v.dedup();
    v
}

fn range(o: &mut Ontology, r: DataRange) -> RangeId {
    RangeId(o.ranges.intern(r))
}

/// Builds negations in the premise's interner, with fresh individuals.
pub struct Negator<'a> {
    pub o: &'a mut Ontology,
    next: Term,
}

impl<'a> Negator<'a> {
    pub fn new(o: &'a mut Ontology) -> Self {
        Self { o, next: FRESH }
    }

    fn fresh(&mut self) -> Term {
        self.next += 1;
        self.next
    }

    fn e(&mut self, expr: ClassExpr) -> ExprId {
        ExprId(self.o.classes.intern(expr))
    }

    fn not(&mut self, x: ExprId) -> ExprId {
        self.e(ClassExpr::Not(x))
    }

    /// `r(a, b)` for a property expression.
    fn edge(r: ObjProp, a: Term, b: Term) -> Axiom {
        match r {
            ObjProp::Named(p) => Axiom::ObjectPropertyAssertion(p, a, b),
            ObjProp::Inverse(p) => Axiom::ObjectPropertyAssertion(p, b, a),
        }
    }

    /// `¬r(a, b)`, as `a : ∀r.¬{b}` (fine for non-simple properties too).
    fn not_edge(&mut self, r: ObjProp, a: Term, b: Term) -> Axiom {
        let one = self.e(ClassExpr::OneOf(vec![b]));
        let not = self.not(one);
        let all = self.e(ClassExpr::All(r, not));
        Axiom::ClassAssertion(all, a)
    }

    /// `x : c ⊓ ¬d`.
    fn sub(&mut self, c: ExprId, d: ExprId) -> Vec<Axiom> {
        let nd = self.not(d);
        let both = self.e(ClassExpr::And(sorted(vec![c, nd])));
        let x = self.fresh();
        vec![Axiom::ClassAssertion(both, x)]
    }

    /// A chain `r₁ ∘ … ∘ rₙ` from `a` to `b` through fresh individuals.
    fn chain(&mut self, chain: &[ObjProp], a: Term, b: Term) -> Vec<Axiom> {
        let mut out = Vec::new();
        let mut at = a;
        for (i, &r) in chain.iter().enumerate() {
            let next = if i + 1 == chain.len() {
                b
            } else {
                self.fresh()
            };
            out.push(Self::edge(r, at, next));
            at = next;
        }
        out
    }

    fn sub_property(&mut self, chain: &[ObjProp], sup: ObjProp) -> Vec<Axiom> {
        let (x, y) = (self.fresh(), self.fresh());
        let mut out = self.chain(chain, x, y);
        out.push(self.not_edge(sup, x, y));
        out
    }

    /// `x : ∃p.⊤` for a fresh `x`.
    fn some_value(&mut self, p: Term) -> Axiom {
        let literal = range(self.o, DataRange::Literal);
        let some = self.e(ClassExpr::DataSome(p, literal));
        Axiom::ClassAssertion(some, self.fresh())
    }

    /// `(x, w) ∈ a` and not in `b`: a fresh data property `c ⊑ a` disjoint with `b`, and
    /// `x : ∃c.⊤` (a model of the premise with such a pair extends to `c`).
    fn data_outside(&mut self, a: Term, b: Term) -> Vec<Axiom> {
        let c = self.fresh();
        vec![
            Axiom::SubDataPropertyOf(c, a),
            Axiom::DisjointDataProperties(sorted(vec![c, b])),
            self.some_value(c),
        ]
    }

    /// The cases of `¬α` (`α` already in the premise's interner); `None` where the
    /// reduction isn't available (keys, datatype definitions).
    pub fn cases(&mut self, axiom: &Axiom) -> Option<Vec<Vec<Axiom>>> {
        let pairs = |xs: &[ExprId]| -> Vec<(ExprId, ExprId)> {
            let mut out = Vec::new();
            for (i, &a) in xs.iter().enumerate() {
                for &b in &xs[i + 1..] {
                    out.push((a, b));
                }
            }
            out
        };
        Some(match axiom {
            Axiom::Declaration(..) => Vec::new(),
            Axiom::SubClassOf(c, d) => vec![self.sub(*c, *d)],
            Axiom::EquivalentClasses(xs) => {
                let mut out = Vec::new();
                for w in xs.windows(2) {
                    out.push(self.sub(w[0], w[1]));
                    out.push(self.sub(w[1], w[0]));
                }
                out
            }
            Axiom::DisjointClasses(xs) => pairs(xs)
                .into_iter()
                .map(|(a, b)| {
                    let both = self.e(ClassExpr::And(sorted(vec![a, b])));
                    vec![Axiom::ClassAssertion(both, self.fresh())]
                })
                .collect(),
            Axiom::DisjointUnion(class, xs) => {
                let named = self.e(ClassExpr::Class(*class));
                let union = self.e(ClassExpr::Or(xs.clone()));
                let mut out = vec![self.sub(named, union), self.sub(union, named)];
                for (a, b) in pairs(xs) {
                    let both = self.e(ClassExpr::And(sorted(vec![a, b])));
                    out.push(vec![Axiom::ClassAssertion(both, self.fresh())]);
                }
                out
            }
            Axiom::ClassAssertion(c, a) => vec![vec![Axiom::ClassAssertion(self.not(*c), *a)]],
            Axiom::ObjectPropertyAssertion(p, a, b) => {
                vec![vec![self.not_edge(ObjProp::Named(*p), *a, *b)]]
            }
            Axiom::NegativeObjectPropertyAssertion(p, a, b) => {
                vec![vec![Axiom::ObjectPropertyAssertion(*p, *a, *b)]]
            }
            Axiom::SameIndividual(xs) => xs
                .windows(2)
                .map(|w| vec![Axiom::DifferentIndividuals(vec![w[0], w[1]])])
                .collect(),
            Axiom::DifferentIndividuals(xs) => {
                let mut out = Vec::new();
                for (i, &a) in xs.iter().enumerate() {
                    for &b in &xs[i + 1..] {
                        out.push(vec![Axiom::SameIndividual(sorted(vec![a, b]))]);
                    }
                }
                out
            }
            Axiom::SubObjectPropertyOf(chain, sup) => vec![self.sub_property(chain, *sup)],
            Axiom::EquivalentObjectProperties(ps) => {
                let mut out = Vec::new();
                for w in ps.windows(2) {
                    out.push(self.sub_property(&[w[0]], w[1]));
                    out.push(self.sub_property(&[w[1]], w[0]));
                }
                out
            }
            Axiom::InverseObjectProperties(a, b) => vec![
                self.sub_property(&[*a], b.inverse()),
                self.sub_property(&[b.inverse()], *a),
            ],
            Axiom::ObjectPropertyDomain(r, c) => {
                let (x, y) = (self.fresh(), self.fresh());
                vec![vec![
                    Self::edge(*r, x, y),
                    Axiom::ClassAssertion(self.not(*c), x),
                ]]
            }
            Axiom::ObjectPropertyRange(r, c) => {
                let (x, y) = (self.fresh(), self.fresh());
                vec![vec![
                    Self::edge(*r, x, y),
                    Axiom::ClassAssertion(self.not(*c), y),
                ]]
            }
            Axiom::ObjectCharacteristic(kind, r) => {
                let (x, y, z) = (self.fresh(), self.fresh(), self.fresh());
                let case = match kind {
                    Characteristic::Functional => vec![
                        Self::edge(*r, x, y),
                        Self::edge(*r, x, z),
                        Axiom::DifferentIndividuals(sorted(vec![y, z])),
                    ],
                    Characteristic::InverseFunctional => vec![
                        Self::edge(*r, y, x),
                        Self::edge(*r, z, x),
                        Axiom::DifferentIndividuals(sorted(vec![y, z])),
                    ],
                    Characteristic::Reflexive => {
                        let own = self.e(ClassExpr::HasSelf(*r));
                        vec![Axiom::ClassAssertion(self.not(own), x)]
                    }
                    Characteristic::Irreflexive => {
                        vec![Axiom::ClassAssertion(self.e(ClassExpr::HasSelf(*r)), x)]
                    }
                    Characteristic::Symmetric => {
                        vec![Self::edge(*r, x, y), self.not_edge(*r, y, x)]
                    }
                    Characteristic::Asymmetric => vec![Self::edge(*r, x, y), Self::edge(*r, y, x)],
                    Characteristic::Transitive => vec![
                        Self::edge(*r, x, y),
                        Self::edge(*r, y, z),
                        self.not_edge(*r, x, z),
                    ],
                };
                vec![case]
            }
            Axiom::DisjointObjectProperties(ps) => {
                let mut out = Vec::new();
                for (i, &a) in ps.iter().enumerate() {
                    for &b in &ps[i + 1..] {
                        let (x, y) = (self.fresh(), self.fresh());
                        out.push(vec![Self::edge(a, x, y), Self::edge(b, x, y)]);
                    }
                }
                out
            }
            Axiom::DataPropertyAssertion(p, a, v) => {
                vec![vec![Axiom::NegativeDataPropertyAssertion(*p, *a, *v)]]
            }
            Axiom::NegativeDataPropertyAssertion(p, a, v) => {
                vec![vec![Axiom::DataPropertyAssertion(*p, *a, *v)]]
            }
            Axiom::DataPropertyRange(p, r) => {
                let not = range(self.o, DataRange::Not(*r));
                let some = self.e(ClassExpr::DataSome(*p, not));
                vec![vec![Axiom::ClassAssertion(some, self.fresh())]]
            }
            Axiom::DataPropertyDomain(p, c) => {
                let literal = range(self.o, DataRange::Literal);
                let some = self.e(ClassExpr::DataSome(*p, literal));
                let not = self.not(*c);
                let both = self.e(ClassExpr::And(sorted(vec![some, not])));
                vec![vec![Axiom::ClassAssertion(both, self.fresh())]]
            }
            Axiom::FunctionalDataProperty(p) => {
                let literal = range(self.o, DataRange::Literal);
                let two = self.e(ClassExpr::DataMin(2, *p, literal));
                vec![vec![Axiom::ClassAssertion(two, self.fresh())]]
            }
            Axiom::SubDataPropertyOf(a, b) => vec![self.data_outside(*a, *b)],
            Axiom::EquivalentDataProperties(ps) => {
                let mut out = Vec::new();
                for w in ps.windows(2) {
                    out.push(self.data_outside(w[0], w[1]));
                    out.push(self.data_outside(w[1], w[0]));
                }
                out
            }
            Axiom::DisjointDataProperties(ps) => {
                let mut out = Vec::new();
                for (i, &a) in ps.iter().enumerate() {
                    for &b in &ps[i + 1..] {
                        // A pair in both: a fresh c below each, with a value.
                        let c = self.fresh();
                        out.push(vec![
                            Axiom::SubDataPropertyOf(c, a),
                            Axiom::SubDataPropertyOf(c, b),
                            self.some_value(c),
                        ]);
                    }
                }
                out
            }
            _ => return None,
        })
    }
}

/// The conclusion's axiom in the premise's interner.
pub fn import(from: &Ontology, axiom: &Axiom, to: &mut Ontology) -> Axiom {
    let mut c = |e: ExprId| copy(from, e, to);
    match axiom {
        Axiom::SubClassOf(a, b) => Axiom::SubClassOf(c(*a), c(*b)),
        Axiom::EquivalentClasses(xs) => {
            Axiom::EquivalentClasses(sorted(xs.iter().map(|&x| c(x)).collect()))
        }
        Axiom::DisjointClasses(xs) => {
            Axiom::DisjointClasses(sorted(xs.iter().map(|&x| c(x)).collect()))
        }
        Axiom::DisjointUnion(k, xs) => Axiom::DisjointUnion(*k, xs.iter().map(|&x| c(x)).collect()),
        Axiom::ObjectPropertyDomain(r, x) => Axiom::ObjectPropertyDomain(*r, c(*x)),
        Axiom::ObjectPropertyRange(r, x) => Axiom::ObjectPropertyRange(*r, c(*x)),
        Axiom::DataPropertyDomain(p, x) => Axiom::DataPropertyDomain(*p, c(*x)),
        Axiom::DataPropertyRange(p, r) => Axiom::DataPropertyRange(*p, copy_range(from, *r, to)),
        Axiom::ClassAssertion(x, a) => Axiom::ClassAssertion(c(*x), *a),
        Axiom::HasKey(x, ps, ds) => Axiom::HasKey(c(*x), ps.clone(), ds.clone()),
        other => other.clone(),
    }
}
