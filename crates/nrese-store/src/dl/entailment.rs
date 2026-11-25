//! Axiom entailment under OWL 2 DL (docs/design/owl2-dl.md, the contract's tasks):
//! `O ⊨ α` iff `O ∪ ¬α` is inconsistent, with `¬α` written as axioms over fresh
//! individuals where `α` is a terminological axiom (a subclass axiom `C ⊑ D` as
//! `(C ⊓ ¬D)(x)` for a fresh `x`; transitivity of `p` as `p(x, y), p(y, z), ¬p(x, z)`).
//! Each consistency test goes to the engine the ontology allows ([`super::consistency`]).
//!
//! It is the oracle of the exact services (`ExactGroundEntailment`: a ground atom is an
//! axiom), and of entailment checks of whole documents ([`crate::StoreService::entails_dl`]).

use nrese_dl::limits::Deadline;
use nrese_owl::{Axiom, Characteristic, ClassExpr, DataRange, ExprId, ObjProp, Ontology, RangeId};

use super::consistency::{self, Budget, Verdict};

mod batch;
pub(crate) use batch::{Batch, Test, compile};

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

/// What a class expression [`Import::class`] copies waits for: its operands' copies.
enum Copying {
    /// An intersection (`or`: a union): the operands copied so far, and those left.
    Set {
        or: bool,
        rest: std::vec::IntoIter<ExprId>,
        done: Vec<ExprId>,
    },
    Not,
    Wrap(fn(ObjProp, ExprId) -> ClassExpr, ObjProp),
    Count(fn(u32, ObjProp, ExprId) -> ClassExpr, u32, ObjProp),
}

impl Import<'_> {
    /// `id` copied, with a stack of its own (the nesting depth is no call depth): operands
    /// in order, each expression interned once its operands are.
    fn class(&mut self, id: ExprId) -> ExprId {
        let sorted = |mut v: Vec<ExprId>| {
            v.sort_unstable();
            v.dedup();
            v
        };
        // What waits for a nonempty set's first operand, and that operand.
        let set = |or: bool, v: Vec<ExprId>| {
            let mut rest = v.into_iter();
            let first = rest.next().expect("a nonempty set");
            let done = Vec::with_capacity(rest.len() + 1);
            (Copying::Set { or, rest, done }, first)
        };
        let mut waiting: Vec<Copying> = Vec::new();
        let mut next = id;
        'copy: loop {
            let (then, operand) = match self.from.class(next).clone() {
                ClassExpr::And(v) if !v.is_empty() => set(false, v),
                ClassExpr::Or(v) if !v.is_empty() => set(true, v),
                ClassExpr::Not(e) => (Copying::Not, e),
                ClassExpr::Some(p, e) => (Copying::Wrap(ClassExpr::Some, p), e),
                ClassExpr::All(p, e) => (Copying::Wrap(ClassExpr::All, p), e),
                ClassExpr::Min(n, p, e) => (Copying::Count(ClassExpr::Min, n, p), e),
                ClassExpr::Max(n, p, e) => (Copying::Count(ClassExpr::Max, n, p), e),
                ClassExpr::Exact(n, p, e) => (Copying::Count(ClassExpr::Exact, n, p), e),
                leaf => {
                    let expr = match leaf {
                        ClassExpr::DataSome(d, r) => ClassExpr::DataSome(d, self.range(r)),
                        ClassExpr::DataAll(d, r) => ClassExpr::DataAll(d, self.range(r)),
                        ClassExpr::DataMin(n, d, r) => ClassExpr::DataMin(n, d, self.range(r)),
                        ClassExpr::DataMax(n, d, r) => ClassExpr::DataMax(n, d, self.range(r)),
                        ClassExpr::DataExact(n, d, r) => ClassExpr::DataExact(n, d, self.range(r)),
                        other => other,
                    };
                    let mut found = ExprId(self.to.classes.intern(expr));
                    // Up through what waits, as far as it has every operand.
                    loop {
                        let Some(then) = waiting.pop() else {
                            return found;
                        };
                        let expr = match then {
                            Copying::Set {
                                or,
                                mut rest,
                                mut done,
                            } => {
                                done.push(found);
                                if let Some(operand) = rest.next() {
                                    waiting.push(Copying::Set { or, rest, done });
                                    next = operand;
                                    continue 'copy;
                                }
                                match or {
                                    true => ClassExpr::Or(sorted(done)),
                                    false => ClassExpr::And(sorted(done)),
                                }
                            }
                            Copying::Not => ClassExpr::Not(found),
                            Copying::Wrap(make, p) => make(p, found),
                            Copying::Count(make, n, p) => make(n, p, found),
                        };
                        found = ExprId(self.to.classes.intern(expr));
                    }
                }
            };
            waiting.push(then);
            next = operand;
        }
    }

    /// `id` copied as [`Self::class`] copies a class expression: with a stack of its own,
    /// operands in order, each range interned once its operands are.
    fn range(&mut self, id: RangeId) -> RangeId {
        // The ranges waiting for their operands, each with those copied so far.
        let mut waiting: Vec<(DataRange, Vec<RangeId>)> = Vec::new();
        let mut next = id;
        loop {
            let mut found = match self.from.range(next).clone() {
                range @ (DataRange::And(_) | DataRange::Or(_) | DataRange::Not(_)) => {
                    waiting.push((range, Vec::new()));
                    None
                }
                leaf => Some(RangeId(self.to.ranges.intern(leaf))),
            };
            // Up through what waits, as far as it has every operand.
            loop {
                let Some((range, done)) = waiting.last_mut() else {
                    return found.expect("the range is copied");
                };
                done.extend(found.take());
                let operands = match range {
                    DataRange::And(v) | DataRange::Or(v) => &v[..],
                    DataRange::Not(r) => std::slice::from_ref(r),
                    _ => &[],
                };
                if let Some(&operand) = operands.get(done.len()) {
                    next = operand;
                    break;
                }
                let (range, mut done) = waiting.pop().expect("a waiting range");
                done.sort_unstable();
                done.dedup();
                let copy = match range {
                    DataRange::And(_) => DataRange::And(done),
                    DataRange::Or(_) => DataRange::Or(done),
                    _ => DataRange::Not(done[0]),
                };
                found = Some(RangeId(self.to.ranges.intern(copy)));
            }
        }
    }

    fn axiom(&mut self, axiom: &Axiom) -> Axiom {
        let classes = |v: &[ExprId], me: &mut Self| v.iter().map(|&e| me.class(e)).collect();
        match axiom {
            Axiom::SubClassOf(a, b) => Axiom::SubClassOf(self.class(*a), self.class(*b)),
            Axiom::EquivalentClasses(v) => Axiom::EquivalentClasses(classes(v, self)),
            Axiom::DisjointClasses(v) => Axiom::DisjointClasses(classes(v, self)),
            Axiom::DisjointUnion(c, v) => Axiom::DisjointUnion(self.class(*c), classes(v, self)),
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

/// The individuals a test axiom of [`negations`] names.
fn individuals(axiom: &Axiom) -> Vec<u64> {
    match axiom {
        Axiom::ClassAssertion(_, a) => vec![*a],
        Axiom::ObjectPropertyAssertion(_, a, b)
        | Axiom::NegativeObjectPropertyAssertion(_, a, b) => vec![*a, *b],
        Axiom::DataPropertyAssertion(_, a, _) | Axiom::NegativeDataPropertyAssertion(_, a, _) => {
            vec![*a]
        }
        Axiom::SameIndividual(v) | Axiom::DifferentIndividuals(v) => v.clone(),
        _ => Vec::new(),
    }
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

/// A copy of `premise` for tests to extend: compilation scratch, reserved from the
/// request's envelope before it is made; `Unknown` where the envelope has no room.
fn copy(
    premise: &Ontology,
    budget: &Budget,
) -> Result<(Ontology, nrese_dl::memory::Scratch), Entailed> {
    let mut scratch = nrese_dl::memory::Scratch::new(budget.memory.as_ref());
    scratch
        .charge(|| nrese_dl::memory::ontology_bytes(premise))
        .map_err(|_| Entailed::Unknown(nrese_dl::memory::EXHAUSTED.to_owned()))?;
    Ok((premise.clone(), scratch))
}

/// Whether `premise` (consistent) entails `axiom`, an axiom over `premise`'s interners.
/// `fresh` are individuals the premise doesn't name (three or more).
pub fn entails(premise: &Ontology, axiom: &Axiom, fresh: &[u64], budget: &Budget) -> Entailed {
    entails_within(
        premise,
        axiom,
        fresh,
        budget,
        Deadline::after(budget.timeout),
    )
}

/// [`entails`], each test within what is left of `deadline` when it starts.
pub(crate) fn entails_within(
    premise: &Ontology,
    axiom: &Axiom,
    fresh: &[u64],
    budget: &Budget,
    deadline: Deadline,
) -> Entailed {
    let (mut o, _scratch) = match copy(premise, budget) {
        Ok(copy) => copy,
        Err(unknown) => return unknown,
    };
    let tests = match negations(&mut o, axiom, fresh) {
        Ok(tests) => tests,
        Err(why) => return Entailed::Unknown(why),
    };
    // ¬α over fresh individuals says that *some* individuals are so: they are unnamed
    // (Direct Semantics §2.3.7). Only a key tells unnamed from named ones (§2.3.5), so
    // where the premise has none they stay as they are, on the routes they had.
    if o.axioms.iter().any(|a| matches!(a, Axiom::HasKey(..))) {
        let witnesses = tests.iter().flatten().flat_map(individuals);
        o.anonymous
            .extend(witnesses.filter(|t| fresh.contains(t)).collect::<Vec<_>>());
    }
    let base = o.axioms.len();
    let mut answer = Entailed::Yes;
    for test in tests {
        o.axioms.truncate(base);
        o.sources.truncate(base);
        for a in test {
            o.axioms.push(a);
            o.sources.push(Vec::new());
        }
        let found = match consistency::check(&o, &budget.remaining(deadline)).verdict {
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
    nonempty_within(premise, class, budget, Deadline::after(budget.timeout))
}

/// [`nonempty`] within what is left of `deadline`.
pub(crate) fn nonempty_within(
    premise: &Ontology,
    class: ExprId,
    budget: &Budget,
    deadline: Deadline,
) -> Entailed {
    let (mut o, _scratch) = match copy(premise, budget) {
        Ok(copy) => copy,
        Err(unknown) => return unknown,
    };
    let nothing = intern(&mut o, ClassExpr::Nothing);
    o.axioms.push(Axiom::SubClassOf(class, nothing));
    o.sources.push(Vec::new());
    match consistency::check(&o, &budget.remaining(deadline)).verdict {
        Verdict::Inconsistent => Entailed::Yes,
        Verdict::Consistent => Entailed::No,
        Verdict::Unknown(why) => Entailed::Unknown(why),
    }
}

/// Whether `premise` (consistent) entails every logical axiom of `conclusion` (an
/// ontology read over the same term ids), within `budget`'s time from the call.
///
/// A conclusion's anonymous individual says that *some* individual is so (*OWL 2 Direct
/// Semantics* §2.3.7–2.4). It is held here as a fresh constant: entailed for every value
/// it may take is entailed for some, but not entailed for one says nothing of the others,
/// so then the answer is undecided.
pub fn entails_ontology(
    premise: &Ontology,
    conclusion: &Ontology,
    fresh: &[u64],
    budget: &Budget,
) -> Entailed {
    entails_ontology_within(
        premise,
        conclusion,
        fresh,
        budget,
        Deadline::after(budget.timeout),
    )
}

/// [`entails_ontology`], each test within what is left of `deadline` (a caller's, that
/// began before this check).
pub(crate) fn entails_ontology_within(
    premise: &Ontology,
    conclusion: &Ontology,
    fresh: &[u64],
    budget: &Budget,
    deadline: Deadline,
) -> Entailed {
    let (mut o, _scratch) = match copy(premise, budget) {
        Ok(copy) => copy,
        Err(unknown) => return unknown,
    };
    o.data.extend(&conclusion.data);
    let tests: Vec<Test> = {
        let mut import = Import {
            from: conclusion,
            to: &mut o,
        };
        conclusion
            .axioms
            .iter()
            .map(|a| Test::Axiom(import.axiom(a)))
            .collect()
    };
    let compiled = Batch::new(&mut o, &tests, budget.memory.as_ref());
    let mut answer = Entailed::Yes;
    for (index, test) in tests.iter().enumerate() {
        answer = answer.and(compiled.check(index, test, &o, fresh, budget, deadline));
        if answer == Entailed::No {
            break;
        }
    }
    if answer == Entailed::No && !conclusion.anonymous.is_empty() {
        return Entailed::Unknown(
            "not entailed for one value of the conclusion's anonymous individual".into(),
        );
    }
    answer
}

#[cfg(test)]
mod import_tests {
    use super::*;

    /// Structural Specification §2.3: set-valued axiom operands remain sorted
    /// and unique after remapping into the premise's expression interner.
    #[test]
    fn import_keeps_set_operands_canonical() {
        let mut from = Ontology::default();
        let from_b = ExprId(from.classes.intern(ClassExpr::Class(20)));
        let from_a = ExprId(from.classes.intern(ClassExpr::Class(10)));
        let from_c = ExprId(from.classes.intern(ClassExpr::Class(30)));
        let eq = Axiom::EquivalentClasses(vec![from_b, from_a]);
        let disj = Axiom::DisjointClasses(vec![from_b, from_a]);
        let union = Axiom::DisjointUnion(from_c, vec![from_b, from_a]);

        let mut to = Ontology::default();
        let to_a = ExprId(to.classes.intern(ClassExpr::Class(10)));
        let to_b = ExprId(to.classes.intern(ClassExpr::Class(20)));
        let to_c = ExprId(to.classes.intern(ClassExpr::Class(30)));
        assert!(to_a < to_b);

        let mut import = Import {
            from: &from,
            to: &mut to,
        };
        assert_eq!(
            import.axiom(&eq),
            Axiom::EquivalentClasses(vec![to_a, to_b])
        );
        assert_eq!(
            import.axiom(&disj),
            Axiom::DisjointClasses(vec![to_a, to_b])
        );
        assert_eq!(
            import.axiom(&union),
            Axiom::DisjointUnion(to_c, vec![to_a, to_b])
        );
    }
}
