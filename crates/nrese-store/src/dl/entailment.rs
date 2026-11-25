//! Axiom entailment under OWL 2 DL (docs/design/owl2-dl.md, the contract's tasks):
//! `O ⊨ α` iff `O ∪ ¬α` is inconsistent, with `¬α` written as axioms over fresh
//! individuals where `α` is a terminological axiom (a subclass axiom `C ⊑ D` as
//! `(C ⊓ ¬D)(x)` for a fresh `x`; transitivity of `p` as `p(x, y), p(y, z), ¬p(x, z)`).
//! Each consistency test goes to the engine the ontology allows ([`super::consistency`]).
//!
//! It is the oracle of the exact services (`ExactGroundEntailment`: a ground atom is an
//! axiom), and of entailment checks of whole documents ([`crate::StoreService::entails_dl`]).

use std::mem::{size_of, take};

use nrese_dl::limits::{Deadline, Limit};
use nrese_dl::memory::Scratch;
use nrese_owl::{
    Axiom, Characteristic, ClassExpr, DataRange, Diagnostic, ExprId, ObjProp, Ontology, RangeId,
    Terms,
};

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
/// (the terms are the same store ids in both), memoised by source id and bounded by one
/// entry reservation and a deadline poll per visited node.
struct Import<'a> {
    from: &'a Ontology,
    to: &'a mut Ontology,
    classes: Vec<Option<ExprId>>,
    ranges: Vec<Option<RangeId>>,
    deadline: Deadline,
    work: u64,
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

impl<'a> Import<'a> {
    fn new(
        from: &'a Ontology,
        to: &'a mut Ontology,
        copy_scratch: &mut Scratch,
        memo_scratch: &mut Scratch,
        deadline: Deadline,
    ) -> Result<Self, Stopped> {
        charge(
            copy_scratch,
            nrese_dl::memory::ontology_bytes(from).saturating_mul(2),
        )?;
        let memo = from.classes.len() * size_of::<Option<ExprId>>()
            + from.ranges.len() * size_of::<Option<RangeId>>();
        charge(memo_scratch, memo)?;
        Ok(Self {
            from,
            to,
            classes: vec![None; from.classes.len()],
            ranges: vec![None; from.ranges.len()],
            deadline,
            work: 0,
        })
    }

    /// `id` copied, with a stack of its own (the nesting depth is no call depth): operands
    /// in order, each expression interned once its operands are.
    fn class(&mut self, id: ExprId) -> Result<ExprId, Stopped> {
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
        let mut waiting: Vec<(ExprId, Copying)> = Vec::new();
        let mut next = id;
        'copy: loop {
            #[cfg(test)]
            test_hooks::visit();
            tick(&mut self.work, 1, self.deadline)?;
            let hit = self.classes[next.0 as usize];
            let expr = if hit.is_some() {
                ClassExpr::Thing
            } else {
                self.from.class(next).clone()
            };
            let (then, operand) = match expr {
                ClassExpr::And(v) if !v.is_empty() => set(false, v),
                ClassExpr::Or(v) if !v.is_empty() => set(true, v),
                ClassExpr::Not(e) => (Copying::Not, e),
                ClassExpr::Some(p, e) => (Copying::Wrap(ClassExpr::Some, p), e),
                ClassExpr::All(p, e) => (Copying::Wrap(ClassExpr::All, p), e),
                ClassExpr::Min(n, p, e) => (Copying::Count(ClassExpr::Min, n, p), e),
                ClassExpr::Max(n, p, e) => (Copying::Count(ClassExpr::Max, n, p), e),
                ClassExpr::Exact(n, p, e) => (Copying::Count(ClassExpr::Exact, n, p), e),
                leaf => {
                    let mut found = if let Some(mapped) = hit {
                        mapped
                    } else {
                        #[cfg(test)]
                        if let ClassExpr::OneOf(v) = &leaf {
                            test_hooks::record_clone(v.len());
                        }
                        let expr = match leaf {
                            ClassExpr::DataSome(d, r) => ClassExpr::DataSome(d, self.range(r)?),
                            ClassExpr::DataAll(d, r) => ClassExpr::DataAll(d, self.range(r)?),
                            ClassExpr::DataMin(n, d, r) => ClassExpr::DataMin(n, d, self.range(r)?),
                            ClassExpr::DataMax(n, d, r) => ClassExpr::DataMax(n, d, self.range(r)?),
                            ClassExpr::DataExact(n, d, r) => {
                                ClassExpr::DataExact(n, d, self.range(r)?)
                            }
                            other => other,
                        };
                        let mapped = ExprId(self.to.classes.intern(expr));
                        self.classes[next.0 as usize] = Some(mapped);
                        mapped
                    };
                    // Up through what waits, as far as it has every operand.
                    loop {
                        let Some((src, then)) = waiting.pop() else {
                            return Ok(found);
                        };
                        let expr = match then {
                            Copying::Set {
                                or,
                                mut rest,
                                mut done,
                            } => {
                                done.push(found);
                                if let Some(operand) = rest.next() {
                                    waiting.push((src, Copying::Set { or, rest, done }));
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
                        self.classes[src.0 as usize] = Some(found);
                    }
                }
            };
            waiting.push((next, then));
            next = operand;
        }
    }

    /// `id` copied as [`Self::class`] copies a class expression: with a stack of its own,
    /// operands in order, each range interned once its operands are.
    fn range(&mut self, id: RangeId) -> Result<RangeId, Stopped> {
        // The ranges waiting for their operands, each with those copied so far.
        let mut waiting: Vec<(RangeId, DataRange, Vec<RangeId>)> = Vec::new();
        let mut next = id;
        loop {
            #[cfg(test)]
            test_hooks::visit();
            tick(&mut self.work, 1, self.deadline)?;
            let mut found = match self.ranges[next.0 as usize] {
                Some(mapped) => Some(mapped),
                None => match self.from.range(next).clone() {
                    range @ (DataRange::And(_) | DataRange::Or(_) | DataRange::Not(_)) => {
                        waiting.push((next, range, Vec::new()));
                        None
                    }
                    leaf => {
                        let mapped = RangeId(self.to.ranges.intern(leaf));
                        self.ranges[next.0 as usize] = Some(mapped);
                        Some(mapped)
                    }
                },
            };
            // Up through what waits, as far as it has every operand.
            loop {
                let Some((_, range, done)) = waiting.last_mut() else {
                    return Ok(found.expect("the range is copied"));
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
                let (src, range, mut done) = waiting.pop().expect("a waiting range");
                done.sort_unstable();
                done.dedup();
                let copy = match range {
                    DataRange::And(_) => DataRange::And(done),
                    DataRange::Or(_) => DataRange::Or(done),
                    _ => DataRange::Not(done[0]),
                };
                let mapped = RangeId(self.to.ranges.intern(copy));
                self.ranges[src.0 as usize] = Some(mapped);
                found = Some(mapped);
            }
        }
    }

    fn axiom(&mut self, axiom: &Axiom) -> Result<Axiom, Stopped> {
        let classes = |v: &[ExprId], me: &mut Self| -> Result<Vec<ExprId>, Stopped> {
            let mut out = Vec::with_capacity(v.len());
            for &e in v {
                out.push(me.class(e)?);
            }
            out.sort_unstable();
            out.dedup();
            Ok(out)
        };
        Ok(match axiom {
            Axiom::SubClassOf(a, b) => Axiom::SubClassOf(self.class(*a)?, self.class(*b)?),
            Axiom::EquivalentClasses(v) => Axiom::EquivalentClasses(classes(v, self)?),
            Axiom::DisjointClasses(v) => Axiom::DisjointClasses(classes(v, self)?),
            Axiom::DisjointUnion(c, v) => Axiom::DisjointUnion(self.class(*c)?, classes(v, self)?),
            Axiom::ObjectPropertyDomain(p, c) => Axiom::ObjectPropertyDomain(*p, self.class(*c)?),
            Axiom::ObjectPropertyRange(p, c) => Axiom::ObjectPropertyRange(*p, self.class(*c)?),
            Axiom::DataPropertyDomain(d, c) => Axiom::DataPropertyDomain(*d, self.class(*c)?),
            Axiom::DataPropertyRange(d, r) => Axiom::DataPropertyRange(*d, self.range(*r)?),
            Axiom::DatatypeDefinition(t, r) => Axiom::DatatypeDefinition(*t, self.range(*r)?),
            Axiom::HasKey(c, p, d) => Axiom::HasKey(self.class(*c)?, p.clone(), d.clone()),
            Axiom::ClassAssertion(c, a) => Axiom::ClassAssertion(self.class(*c)?, *a),
            other => {
                #[cfg(test)]
                if let Axiom::DifferentIndividuals(v) = other {
                    test_hooks::record_clone(v.len());
                }
                other.clone()
            }
        })
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

enum Stopped {
    Time,
    Memory,
}

impl From<Stopped> for Entailed {
    fn from(s: Stopped) -> Self {
        match s {
            Stopped::Time => Self::Unknown(Limit::Time.reached("the deadline passed")),
            Stopped::Memory => Self::Unknown(nrese_dl::memory::EXHAUSTED.to_owned()),
        }
    }
}

fn charge(s: &mut Scratch, bytes: usize) -> Result<(), Stopped> {
    s.charge(|| bytes).map_err(|_| Stopped::Memory)
}

fn tick(work: &mut u64, units: u64, deadline: Deadline) -> Result<(), Stopped> {
    *work = work.saturating_add(units);
    if *work >= 4_096 {
        *work %= 4_096;
        if deadline.passed() {
            return Err(Stopped::Time);
        }
    }
    Ok(())
}

/// A premise copy and imported conclusion axioms ready for entailment testing.
pub(crate) struct PreparedCheck {
    o: Ontology,
    _copy_scratch: Scratch,
    axioms: Vec<Axiom>,
}

#[derive(Clone, Copy)]
enum Origin {
    Premise,
    Conclusion,
}

fn import_into(
    premise: &Ontology,
    conclusion: &Ontology,
    budget: &Budget,
    deadline: Deadline,
) -> Result<PreparedCheck, Entailed> {
    let (mut o, mut copy_scratch) = copy(premise, budget)?;
    #[cfg(test)]
    let _entry_guard = test_hooks::on_entry(budget.memory.as_ref());
    let mut memo_scratch = Scratch::new(budget.memory.as_ref());
    let mut import = Import::new(
        conclusion,
        &mut o,
        &mut copy_scratch,
        &mut memo_scratch,
        deadline,
    )?;
    import.to.data.extend(&conclusion.data);
    let (b, cb) = (&mut import.to.builtin, conclusion.builtin);
    b.top_object = b.top_object.or(cb.top_object);
    b.bottom_object = b.bottom_object.or(cb.bottom_object);
    b.top_data = b.top_data.or(cb.top_data);
    b.bottom_data = b.bottom_data.or(cb.bottom_data);
    let mut axioms = Vec::with_capacity(conclusion.axioms.len());
    for a in &conclusion.axioms {
        axioms.push(import.axiom(a)?);
    }
    drop(import);
    drop(memo_scratch);
    Ok(PreparedCheck {
        o,
        _copy_scratch: copy_scratch,
        axioms,
    })
}

/// Copies `premise` once, imports `conclusion` once, checks OWL 2 DL membership over the
/// deduplicated joint union, and restores the premise copy by identity.
pub(crate) fn admit_and_prepare(
    premise: &Ontology,
    conclusion: &Ontology,
    terms: &dyn Terms,
    name: &dyn Fn(u64) -> String,
    budget: &Budget,
    deadline: Deadline,
) -> Result<PreparedCheck, Entailed> {
    let mut prepared = import_into(premise, conclusion, budget, deadline)?;
    let o = &mut prepared.o;
    let mut imported = take(&mut prepared.axioms);
    imported.sort_unstable();
    imported.dedup();

    let mut js = Scratch::new(budget.memory.as_ref());
    charge(&mut js, imported.len() * size_of::<Axiom>())?;
    let (mut extra, mut already, mut work) = (Vec::with_capacity(imported.len()), Vec::new(), 0u64);
    for a in imported {
        tick(&mut work, 1, deadline)?;
        if o.axioms.binary_search(&a).is_ok() {
            already.push(a);
        } else {
            extra.push(a);
        }
    }

    let (p_axioms, p_sources) = (take(&mut o.axioms), take(&mut o.sources));
    let (extra_len, total) = (extra.len(), p_axioms.len() + extra.len());
    charge(&mut js, total * (size_of::<Axiom>() + size_of::<Origin>()))?;
    let (mut joint, mut origin) = (Vec::with_capacity(total), Vec::with_capacity(total));
    #[cfg(not(test))]
    let mut p_it = p_axioms.into_iter().peekable();
    #[cfg(test)]
    let mut p_it = test_hooks::track_axioms(p_axioms).peekable();
    let mut c_it = extra.into_iter().peekable();
    while p_it.peek().is_some() || c_it.peek().is_some() {
        tick(&mut work, 1, deadline)?;
        let from_p = match (p_it.peek(), c_it.peek()) {
            (Some(pa), Some(ca)) => pa <= ca,
            (Some(_), None) => true,
            _ => false,
        };
        if from_p {
            joint.push(p_it.next().unwrap());
            origin.push(Origin::Premise);
        } else {
            joint.push(c_it.next().unwrap());
            origin.push(Origin::Conclusion);
        }
    }
    drop(p_it);
    drop(c_it);

    let saved = (
        take(&mut o.anonymous),
        take(&mut o.annotation_uses),
        take(&mut o.annotation_axioms),
    );
    let n_sets = saved.0.len()
        + conclusion.anonymous.len()
        + saved.1.len()
        + conclusion.annotation_uses.len()
        + saved.2.len()
        + conclusion.annotation_axioms.len();
    charge(&mut js, n_sets * (size_of::<u64>() + 8))?;
    let merge = |a: &std::collections::BTreeSet<u64>, b: &std::collections::BTreeSet<u64>| {
        a.union(b).copied().collect()
    };
    o.anonymous = merge(&saved.0, &conclusion.anonymous);
    o.annotation_uses = merge(&saved.1, &conclusion.annotation_uses);
    o.annotation_axioms = merge(&saved.2, &conclusion.annotation_axioms);
    o.axioms = joint;

    if deadline.passed() {
        return Err(Stopped::Time.into());
    }
    #[cfg(test)]
    let _membership_guard = test_hooks::on_membership(budget.memory.as_ref());
    let memo_bytes = nrese_owl::diagnostics::non_simple_memo_bytes(o.classes.len());
    charge(&mut js, memo_bytes)?;
    let findings = nrese_owl::membership(o, terms);
    js.release(memo_bytes);
    let blocks_j = |d: &Diagnostic| match *d {
        Diagnostic::AnonymousIndividual { node, axioms, .. } => {
            // Fresh blank identities make a witnessless finding on a conclusion blank its own.
            axioms
                .iter()
                .flatten()
                .any(|&k| matches!(origin[k], Origin::Conclusion))
                || (axioms.iter().all(Option::is_none) && conclusion.anonymous.contains(&node))
        }
        _ => d.is_fatal(),
    };
    if let Some(d) = findings
        .iter()
        .find(|d| blocks_j(d))
        .or_else(|| premise.diagnostics.iter().find(|d| d.is_fatal()))
    {
        return Err(Entailed::Unknown(format!(
            "the joint ontology isn't well-formed OWL 2 DL: {}",
            d.describe(name)
        )));
    }

    let (mut restored, mut axioms) = (Vec::with_capacity(premise.axioms.len()), already);
    #[cfg(test)]
    test_hooks::on_restore(
        o.axioms.capacity() * size_of::<Axiom>()
            + restored.capacity() * size_of::<Axiom>()
            + p_sources.capacity() * size_of::<Vec<nrese_owl::Source>>()
            + origin.capacity() * size_of::<Origin>(),
        budget.memory.as_ref().map_or(0, |e| e.used()),
    );
    axioms.reserve(extra_len);
    for (a, from) in take(&mut o.axioms).into_iter().zip(origin) {
        match from {
            Origin::Premise => restored.push(a),
            Origin::Conclusion => axioms.push(a),
        }
    }
    (o.axioms, o.sources) = (restored, p_sources);
    (o.anonymous, o.annotation_uses, o.annotation_axioms) = saved;
    drop(js);
    prepared.axioms = axioms;
    Ok(prepared)
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
    match import_into(premise, conclusion, budget, deadline) {
        Ok(p) => {
            entails_prepared_within(p, !conclusion.anonymous.is_empty(), fresh, budget, deadline)
        }
        Err(unknown) => unknown,
    }
}

/// Runs entailment tests on an already-prepared premise copy and imported conclusion axioms.
pub(crate) fn entails_prepared_within(
    prepared: PreparedCheck,
    conclusion_has_anonymous: bool,
    fresh: &[u64],
    budget: &Budget,
    deadline: Deadline,
) -> Entailed {
    let PreparedCheck {
        mut o,
        _copy_scratch,
        axioms,
    } = prepared;
    let tests: Vec<Test> = axioms.into_iter().map(Test::Axiom).collect();
    let compiled = Batch::new(&mut o, &tests, budget.memory.as_ref());
    let mut answer = Entailed::Yes;
    for (index, test) in tests.iter().enumerate() {
        answer = answer.and(compiled.check(index, test, &o, fresh, budget, deadline));
        if answer == Entailed::No {
            break;
        }
    }
    if answer == Entailed::No && conclusion_has_anonymous {
        return Entailed::Unknown(
            "not entailed for one value of the conclusion's anonymous individual".into(),
        );
    }
    answer
}

#[cfg(test)]
pub(crate) mod test_hooks {
    use std::cell::{Cell, RefCell};

    use nrese_exec::{Envelope, Reservation};

    type VisitHook = Box<dyn Fn()>;
    type EntryHook = Box<dyn Fn(Option<&Envelope>) -> Option<Reservation<Envelope>>>;

    thread_local! {
        pub(crate) static IMPORT_VISIT: RefCell<Option<VisitHook>> = const { RefCell::new(None) };
        pub(crate) static IMPORT_ENTRY: RefCell<Option<EntryHook>> = const { RefCell::new(None) };
        pub(crate) static MEMBERSHIP_ENTRY: RefCell<Option<EntryHook>> = const { RefCell::new(None) };
        pub(crate) static IMPORT_CLONED_OPERANDS: Cell<usize> = const { Cell::new(0) };
        static LIVE_MERGE_BYTES: Cell<usize> = const { Cell::new(0) };
        static RESTORE_PEAK: Cell<Option<(usize, usize)>> = const { Cell::new(None) };
    }

    pub(crate) struct TrackedAxioms {
        inner: std::vec::IntoIter<crate::dl::entailment::Axiom>,
        bytes: usize,
    }

    impl Iterator for TrackedAxioms {
        type Item = crate::dl::entailment::Axiom;

        fn next(&mut self) -> Option<Self::Item> {
            self.inner.next()
        }
    }

    impl Drop for TrackedAxioms {
        fn drop(&mut self) {
            LIVE_MERGE_BYTES.with(|c| c.set(c.get().saturating_sub(self.bytes)));
        }
    }

    pub(crate) fn track_axioms(v: Vec<crate::dl::entailment::Axiom>) -> TrackedAxioms {
        let bytes = v.capacity() * std::mem::size_of::<crate::dl::entailment::Axiom>();
        LIVE_MERGE_BYTES.with(|c| c.set(c.get() + bytes));
        TrackedAxioms {
            inner: v.into_iter(),
            bytes,
        }
    }

    pub(crate) fn on_restore(other_live_bytes: usize, reserved_bytes: usize) {
        let total = LIVE_MERGE_BYTES.with(Cell::get) + other_live_bytes;
        RESTORE_PEAK.with(|slot| slot.set(Some((total, reserved_bytes))));
    }

    pub(crate) fn take_restore_peak() -> Option<(usize, usize)> {
        RESTORE_PEAK.with(Cell::take)
    }

    pub(crate) fn visit() {
        IMPORT_VISIT.with(|slot| {
            if let Some(f) = slot.borrow().as_ref() {
                f();
            }
        });
    }

    pub(crate) fn on_entry(envelope: Option<&Envelope>) -> Option<Reservation<Envelope>> {
        IMPORT_ENTRY.with(|slot| slot.borrow().as_ref().and_then(|f| f(envelope)))
    }

    pub(crate) fn on_membership(envelope: Option<&Envelope>) -> Option<Reservation<Envelope>> {
        MEMBERSHIP_ENTRY.with(|slot| slot.borrow().as_ref().and_then(|f| f(envelope)))
    }

    pub(crate) fn record_clone(operands: usize) {
        IMPORT_CLONED_OPERANDS.with(|c| c.set(c.get() + operands));
    }
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

        let (mut cs, mut ms) = (Scratch::default(), Scratch::default());
        let mut import = Import::new(&from, &mut to, &mut cs, &mut ms, Deadline::after(None))
            .map_err(Entailed::from)
            .expect("unbounded import");
        assert_eq!(
            import.axiom(&eq).ok(),
            Some(Axiom::EquivalentClasses(vec![to_a, to_b]))
        );
        assert_eq!(
            import.axiom(&disj).ok(),
            Some(Axiom::DisjointClasses(vec![to_a, to_b]))
        );
        assert_eq!(
            import.axiom(&union).ok(),
            Some(Axiom::DisjointUnion(to_c, vec![to_a, to_b]))
        );
    }

    struct StubTerms;

    impl Terms for StubTerms {
        fn kind(&self, _term: u64) -> nrese_owl::TermKind {
            nrese_owl::TermKind::Iri
        }
        fn lexical(&self, _term: u64) -> Option<String> {
            None
        }
        fn iri(&self, _iri: &str) -> Option<u64> {
            None
        }
        fn datatype(&self, _term: u64) -> Option<String> {
            None
        }
        fn iri_text(&self, _term: u64) -> Option<String> {
            None
        }
    }

    fn test_budget(memory: Option<nrese_exec::Envelope>) -> Budget {
        Budget {
            timeout: None,
            memory_bytes: None,
            memory,
            workers: nrese_exec::workers::Workers::serial(),
            cancel: None,
            max_nodes: None,
            max_branch_points: None,
            max_merges: None,
            max_join: None,
            explanation_tests: None,
        }
    }

    #[cfg(debug_assertions)]
    fn shared_class_fixture(k: usize, n: usize, with_needing: bool) -> (Ontology, Ontology) {
        let mut premise = Ontology::default();
        premise.axioms.push(Axiom::ObjectCharacteristic(
            Characteristic::Transitive,
            ObjProp::Named(1),
        ));
        premise.sources.push(Vec::new());
        if with_needing {
            premise
                .classes
                .intern(ClassExpr::HasSelf(ObjProp::Named(1)));
        }

        let mut conclusion = Ontology::default();
        let mut cur = ExprId(conclusion.classes.intern(ClassExpr::Class(100)));
        for _ in 0..n {
            cur = ExprId(
                conclusion
                    .classes
                    .intern(ClassExpr::Some(ObjProp::Named(2), cur)),
            );
        }
        let root = cur;
        for i in 0..k {
            let sub = ExprId(
                conclusion
                    .classes
                    .intern(ClassExpr::Class(1_000 + i as u64)),
            );
            conclusion.axioms.push(Axiom::SubClassOf(sub, root));
            conclusion.sources.push(Vec::new());
        }
        conclusion.axioms.sort_unstable();
        (premise, conclusion)
    }

    /// Spec (contracts.md, Cancellation and Work admission; Structural Specification §11.2):
    /// the joint non-simple-property check expands each shared class expression at most once
    /// across axioms, so work stays linear when K and N both double, and an already-passed
    /// deadline at membership entry stops with a typed time Unknown before the walk.
    #[cfg(debug_assertions)]
    #[test]
    fn joint_membership_bounds_shared_class_work() {
        use std::time::Duration;

        let terms = StubTerms;
        let name = |t: u64| format!("t{t}");
        let budget = test_budget(None);
        let generous = Deadline::after(Some(Duration::from_secs(60)));

        // Without cardinalities or self restrictions: generous deadline admits both sizes,
        // and expansions stay within a linear bound in K + N.
        for (k, n) in [(64usize, 64usize), (128, 128)] {
            let (premise, conclusion) = shared_class_fixture(k, n, false);
            let before = nrese_owl::diagnostics::non_simple_expansions();
            let res = admit_and_prepare(&premise, &conclusion, &terms, &name, &budget, generous);
            let expansions = nrese_owl::diagnostics::non_simple_expansions() - before;
            assert!(res.is_ok(), "generous control ({k}, {n}) must admit");
            let bound = 4 * (k + n) as u64;
            assert!(
                expansions <= bound,
                "shared class walk ({k}, {n}) expanded {expansions} nodes (bound {bound})"
            );
        }

        // With an unused self-restriction on the non-simple property so the memoised walk runs:
        // doubling both K and N grows expansions linearly (at most 2.5x), not quadratically (4x).
        let mut counts = Vec::new();
        for (k, n) in [(64usize, 64usize), (128, 128)] {
            let (premise, conclusion) = shared_class_fixture(k, n, true);
            let before = nrese_owl::diagnostics::non_simple_expansions();
            let res = admit_and_prepare(&premise, &conclusion, &terms, &name, &budget, generous);
            let expansions = nrese_owl::diagnostics::non_simple_expansions() - before;
            assert!(
                res.is_ok(),
                "non-empty needing control ({k}, {n}) must admit"
            );
            let bound = 2 * (k + n) as u64 + 16;
            assert!(
                expansions > 0 && expansions <= bound,
                "memoised walk ({k}, {n}) expanded {expansions} nodes (bound {bound})"
            );
            counts.push(expansions);
        }
        assert!(
            counts[1] * 2 <= counts[0] * 5,
            "doubling K and N grew expansions super-linearly: {counts:?}"
        );

        // Deadline already passed at membership entry: stops with typed time Unknown before walk.
        let (premise, conclusion) = shared_class_fixture(64, 64, true);
        let expired = Deadline::after(Some(Duration::ZERO));
        let before = nrese_owl::diagnostics::non_simple_expansions();
        let late = admit_and_prepare(&premise, &conclusion, &terms, &name, &budget, expired);
        let after = nrese_owl::diagnostics::non_simple_expansions() - before;
        match late {
            Err(Entailed::Unknown(why)) => {
                assert_eq!(
                    Limit::named_in(&why),
                    Some(Limit::Time),
                    "expected typed time Unknown, got: {why}"
                );
            }
            other => panic!(
                "expected time Unknown at membership entry, got {}",
                match other {
                    Ok(_) => "Ok(PreparedCheck)".to_owned(),
                    Err(e) => format!("Err({e:?})"),
                }
            ),
        }
        assert_eq!(after, 0, "walk must not run past an expired deadline");
    }

    /// Spec (contracts.md, Memory): restoring the premise axioms after the joint check must
    /// not hold three premise-sized axiom buffers against two reservations; without a cap,
    /// restored axioms and sources preserve their exact order.
    #[test]
    fn restoration_respects_the_joint_envelope() {
        const N: usize = 262_144;
        let mut premise = Ontology::default();
        premise.axioms.reserve_exact(N);
        premise.sources.reserve_exact(N);
        for i in 0..N as u64 {
            premise
                .axioms
                .push(Axiom::ObjectPropertyAssertion(1, 10 + i, 10 + i));
            premise.sources.push(Vec::new());
        }
        premise.axioms.sort_unstable();

        let mut conclusion = Ontology::default();
        conclusion
            .axioms
            .push(Axiom::ObjectPropertyAssertion(1, 1, 2));
        conclusion.sources.push(Vec::new());

        let terms = StubTerms;
        let name = |t: u64| format!("t{t}");
        let generous = Deadline::after(Some(std::time::Duration::from_secs(60)));

        // Envelope sized between the two-buffer and three-buffer peaks.
        let two_buffers = nrese_dl::memory::ontology_bytes(&premise)
            + nrese_dl::memory::ontology_bytes(&conclusion) * 2
            + (N + 1) * (size_of::<Axiom>() + size_of::<Origin>())
            + 65_536;
        let env = nrese_exec::Envelope::exact(Some(two_buffers), None);
        let budget = test_budget(Some(env));

        let _ = test_hooks::take_restore_peak();
        let res = admit_and_prepare(&premise, &conclusion, &terms, &name, &budget, generous);
        match res {
            Ok(_) => {
                let (live, reserved) =
                    test_hooks::take_restore_peak().expect("restore hook must run");
                assert!(
                    live <= reserved && live <= two_buffers,
                    "restore held {live} bytes across live vectors against {reserved} reserved (envelope {two_buffers})"
                );
            }
            Err(Entailed::Unknown(why)) => {
                assert_eq!(
                    Limit::named_in(&why),
                    Some(Limit::Envelope),
                    "expected memory Unknown, got: {why}"
                );
            }
            Err(other) => panic!("unexpected admission error: {other:?}"),
        }

        // No-cap control: restored axioms and sources match the premise in order.
        let prepared = admit_and_prepare(
            &premise,
            &conclusion,
            &terms,
            &name,
            &test_budget(None),
            generous,
        )
        .expect("no-cap control");
        assert_eq!(prepared.o.axioms, premise.axioms);
        assert_eq!(prepared.o.sources, premise.sources);
        assert_eq!(prepared.axioms, conclusion.axioms);
    }

    /// Spec (contracts.md, Memory; Structural Specification §11.2): the joint membership
    /// check reserves its non-simple-property memo and stack from the request envelope before
    /// allocating them, answering a typed memory Unknown under a tight envelope while a roomy
    /// envelope reports the non-simple-property violation.
    #[test]
    fn joint_membership_reserves_its_non_simple_memo() {
        use nrese_exec::{Envelope, Reservation};
        use std::time::Duration;

        const CLASSES: usize = 262_144;
        let mut premise = Ontology::default();
        for i in 0..CLASSES as u64 {
            premise.classes.intern(ClassExpr::Class(100 + i));
        }
        premise.axioms.push(Axiom::ObjectCharacteristic(
            Characteristic::Transitive,
            ObjProp::Named(1),
        ));
        premise.sources.push(Vec::new());

        let mut conclusion = Ontology::default();
        let c = ExprId(conclusion.classes.intern(ClassExpr::Class(1)));
        let has_self = ExprId(
            conclusion
                .classes
                .intern(ClassExpr::HasSelf(ObjProp::Named(1))),
        );
        conclusion.axioms.push(Axiom::SubClassOf(c, has_self));
        conclusion.sources.push(Vec::new());

        let terms = StubTerms;
        let name = |t: u64| format!("t{t}");
        let generous = Deadline::after(Some(Duration::from_secs(60)));

        // Tight cell: leave 256 KiB at membership entry, far below a 262,144-class memo.
        let tight_env = Envelope::exact(Some(128 << 20), None);
        test_hooks::MEMBERSHIP_ENTRY.replace(Some(Box::new(|envelope| {
            let env = envelope.expect("bounded envelope");
            let take = env.remaining().saturating_sub(256 << 10);
            let mut held = Reservation::new(env.clone());
            held.charge(take).expect("tighten membership headroom");
            Some(held)
        })));
        let tight = admit_and_prepare(
            &premise,
            &conclusion,
            &terms,
            &name,
            &test_budget(Some(tight_env)),
            generous,
        );
        test_hooks::MEMBERSHIP_ENTRY.replace(None);
        match tight {
            Err(Entailed::Unknown(why)) => {
                assert_eq!(
                    Limit::named_in(&why),
                    Some(Limit::Envelope),
                    "tight memo headroom must answer typed memory Unknown, got: {why}"
                );
            }
            other => panic!(
                "expected memory Unknown on tight memo headroom, got {}",
                match other {
                    Ok(_) => "Ok(PreparedCheck)".to_owned(),
                    Err(e) => format!("Err({e:?})"),
                }
            ),
        }

        // Roomy control: reports the non-simple-property finding.
        let roomy_env = Envelope::exact(Some(128 << 20), None);
        let roomy = admit_and_prepare(
            &premise,
            &conclusion,
            &terms,
            &name,
            &test_budget(Some(roomy_env)),
            generous,
        );
        match roomy {
            Err(Entailed::Unknown(why)) => {
                assert!(
                    why.contains("a non-simple property in a self restriction"),
                    "roomy control must report non-simple restriction, got: {why}"
                );
            }
            other => panic!(
                "expected non-simple Unknown on roomy envelope, got {}",
                match other {
                    Ok(_) => "Ok(PreparedCheck)".to_owned(),
                    Err(e) => format!("Err({e:?})"),
                }
            ),
        }
    }

    /// Spec (contracts.md, Memory; Structural Specification §11.2): a dense shared-operand
    /// class DAG in the joint ontology must keep its live non-simple-property scratch within
    /// the advertised memo reservation and remaining envelope headroom.
    #[cfg(debug_assertions)]
    #[test]
    fn joint_membership_bounds_shared_operand_stack() {
        use nrese_exec::{Envelope, Reservation};
        use std::time::Duration;

        let mut premise = Ontology::default();
        premise.axioms.push(Axiom::ObjectCharacteristic(
            Characteristic::Transitive,
            ObjProp::Named(1),
        ));
        premise.sources.push(Vec::new());

        let mut conclusion = Ontology::default();
        let mut leaves = Vec::with_capacity(512);
        for i in 0..512u64 {
            leaves.push(ExprId(conclusion.classes.intern(ClassExpr::Class(100 + i))));
        }
        let mut prev = ExprId(
            conclusion
                .classes
                .intern(ClassExpr::HasSelf(ObjProp::Named(1))),
        );
        for _ in 0..512usize {
            let mut operands = leaves.clone();
            operands.push(prev);
            prev = ExprId(conclusion.classes.intern(ClassExpr::And(operands)));
        }
        let sub = ExprId(conclusion.classes.intern(ClassExpr::Class(1)));
        conclusion.axioms.push(Axiom::SubClassOf(sub, prev));
        conclusion.sources.push(Vec::new());

        let total_classes = premise.classes.len() + conclusion.classes.len();
        let advertised = nrese_owl::diagnostics::non_simple_memo_bytes(total_classes);
        let headroom = advertised + 4_096;
        let terms = StubTerms;
        let name = |t: u64| format!("t{t}");
        let generous = Deadline::after(Some(Duration::from_secs(60)));

        // Tight cell: leave only the advertised headroom at membership entry.
        let tight_env = Envelope::exact(Some(128 << 20), None);
        test_hooks::MEMBERSHIP_ENTRY.replace(Some(Box::new(move |envelope| {
            let env = envelope.expect("bounded envelope");
            let take = env.remaining().saturating_sub(headroom);
            let mut held = Reservation::new(env.clone());
            held.charge(take).expect("tighten membership headroom");
            Some(held)
        })));
        let _ = nrese_owl::diagnostics::take_non_simple_scratch_peak();
        let tight = admit_and_prepare(
            &premise,
            &conclusion,
            &terms,
            &name,
            &test_budget(Some(tight_env)),
            generous,
        );
        test_hooks::MEMBERSHIP_ENTRY.replace(None);
        let tight_peak = nrese_owl::diagnostics::take_non_simple_scratch_peak();
        match tight {
            Err(Entailed::Unknown(why)) if Limit::named_in(&why) == Some(Limit::Envelope) => {
                assert!(
                    tight_peak <= headroom,
                    "refused pass held {tight_peak} scratch bytes past headroom {headroom}"
                );
            }
            Err(Entailed::Unknown(why))
                if why.contains("a non-simple property in a self restriction") =>
            {
                assert!(
                    tight_peak <= advertised && tight_peak <= headroom,
                    "live scratch {tight_peak} exceeded advertised memo {advertised} (headroom {headroom})"
                );
            }
            other => panic!(
                "expected memory or non-simple Unknown, got {}",
                match other {
                    Ok(_) => "Ok(PreparedCheck)".to_owned(),
                    Err(e) => format!("Err({e:?})"),
                }
            ),
        }

        // Roomy control: reports the self-restriction finding within the advertised bound.
        let roomy_env = Envelope::exact(Some(128 << 20), None);
        let _ = nrese_owl::diagnostics::take_non_simple_scratch_peak();
        let roomy = admit_and_prepare(
            &premise,
            &conclusion,
            &terms,
            &name,
            &test_budget(Some(roomy_env)),
            generous,
        );
        let roomy_peak = nrese_owl::diagnostics::take_non_simple_scratch_peak();
        match roomy {
            Err(Entailed::Unknown(why)) => {
                assert!(
                    why.contains("a non-simple property in a self restriction"),
                    "roomy control must report non-simple restriction, got: {why}"
                );
                assert!(
                    roomy_peak <= advertised,
                    "roomy live scratch {roomy_peak} exceeded advertised memo {advertised}"
                );
            }
            other => panic!(
                "expected non-simple Unknown on roomy envelope, got {}",
                match other {
                    Ok(_) => "Ok(PreparedCheck)".to_owned(),
                    Err(e) => format!("Err({e:?})"),
                }
            ),
        }
    }
}
