//! Certificates (docs/design/owl2-dl.md §10–11): answers a small checker verifies without
//! trusting the engines, as the number module's validator verifies its compressed models
//! ([`crate::numbers`]).
//!
//! **"Consistent"** is certified by a finite interpretation that [`check`] finds to be a
//! model of every axiom of the ontology as written, by the direct semantics (*OWL 2
//! Direct Semantics* §2.2–2.3): class expressions evaluated as sets of elements,
//! `owl:topObjectProperty` the universal relation, `owl:bottomObjectProperty` the empty
//! one, keys over the named individuals. It knows nothing of the normalisation, its
//! clauses, the hub that encodes the universal property, or the engines.
//!
//! [`certify`] builds the interpretation from a hypertableau model ([`Model`], folded at
//! the blocks), closes it under the role inclusions (the clauses encode chains and
//! transitivity by automata, which a folded model satisfies only on closure), and checks
//! it; where the fold merged what the semantics keeps apart (a number restriction or an
//! irreflexive property against a block), it checks the 3-fold covering lift instead
//! ([`lift`]), then the graph unravelled one step at each block
//! ([`Model::unravelled`]) and its lift. A model that passes none is no certificate:
//! [`Certificate::Declined`], which says nothing against the answer.
//!
//! Not yet covered: data values (the model has none: an ontology that uses data
//! properties or datatypes is declined), and "inconsistent" (a refutation is a search
//! tree, not yet recorded as a proof: docs/design/owl2-dl.md §10).

use std::collections::HashMap;

use nrese_owl::{Axiom, Characteristic, ClassExpr, Concept, ExprId, ObjProp, Ontology, Term};

use crate::tableau::Model;

/// A set of elements.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Set {
    words: Vec<u64>,
}

impl Set {
    fn empty(n: usize) -> Self {
        Self {
            words: vec![0; n.div_ceil(64)],
        }
    }

    fn full(n: usize) -> Self {
        let mut s = Self::empty(n);
        for e in 0..n {
            s.insert(e);
        }
        s
    }

    fn insert(&mut self, e: usize) -> bool {
        let (w, b) = (e / 64, 1u64 << (e % 64));
        let new = self.words[w] & b == 0;
        self.words[w] |= b;
        new
    }

    pub fn contains(&self, e: usize) -> bool {
        self.words[e / 64] & (1 << (e % 64)) != 0
    }

    fn and(&self, o: &Self) -> Self {
        Self {
            words: self
                .words
                .iter()
                .zip(&o.words)
                .map(|(a, b)| a & b)
                .collect(),
        }
    }

    fn or(&self, o: &Self) -> Self {
        Self {
            words: self
                .words
                .iter()
                .zip(&o.words)
                .map(|(a, b)| a | b)
                .collect(),
        }
    }

    fn not(&self, n: usize) -> Self {
        let mut s = Self {
            words: self.words.iter().map(|w| !w).collect(),
        };
        if !n.is_multiple_of(64) {
            let last = s.words.len() - 1;
            s.words[last] &= (1u64 << (n % 64)) - 1;
        }
        s
    }

    fn is_empty(&self) -> bool {
        self.words.iter().all(|&w| w == 0)
    }

    fn subset(&self, o: &Self) -> bool {
        self.words.iter().zip(&o.words).all(|(a, b)| a & !b == 0)
    }

    fn elements(&self) -> impl Iterator<Item = usize> + '_ {
        self.words.iter().enumerate().flat_map(|(i, &w)| {
            (0..64)
                .filter(move |b| w & (1 << b) != 0)
                .map(move |b| i * 64 + b)
        })
    }
}

/// A finite interpretation of an ontology's classes, object properties and individuals.
#[derive(Debug, Clone, Default)]
pub struct Interpretation {
    pub size: usize,
    pub classes: HashMap<Term, Vec<usize>>,
    /// Per property, its pairs.
    pub roles: HashMap<Term, Vec<(usize, usize)>>,
    pub individuals: HashMap<Term, usize>,
}

/// The checker's view: extensions as sets, successors per element both ways.
struct View<'a> {
    o: &'a Ontology,
    n: usize,
    classes: HashMap<Term, Set>,
    /// Per property: successors and predecessors of each element.
    succ: HashMap<Term, Vec<Vec<usize>>>,
    pred: HashMap<Term, Vec<Vec<usize>>>,
    individuals: &'a HashMap<Term, usize>,
    top: Option<Term>,
    bottom: Option<Term>,
}

/// Why a certificate couldn't be given.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Certificate {
    /// A model of every axiom: the ontology is consistent. `elements`: its size;
    /// `lifted`: the covering lift of the engine's model.
    Model { elements: usize, lifted: bool },
    /// No certificate (why): not that the ontology has no model.
    Declined(String),
}

impl Certificate {
    pub fn is_model(&self) -> bool {
        matches!(self, Certificate::Model { .. })
    }
}

impl<'a> View<'a> {
    fn new(o: &'a Ontology, i: &'a Interpretation) -> Self {
        let n = i.size;
        let mut classes = HashMap::new();
        for (&c, elements) in &i.classes {
            let mut s = Set::empty(n);
            for &e in elements {
                s.insert(e);
            }
            classes.insert(c, s);
        }
        let (mut succ, mut pred) = (HashMap::new(), HashMap::new());
        for (&p, pairs) in &i.roles {
            let s: &mut Vec<Vec<usize>> = succ.entry(p).or_insert_with(|| vec![Vec::new(); n]);
            let q: &mut Vec<Vec<usize>> = pred.entry(p).or_insert_with(|| vec![Vec::new(); n]);
            for &(a, b) in pairs {
                s[a].push(b);
                q[b].push(a);
            }
        }
        for lists in succ.values_mut().chain(pred.values_mut()) {
            for l in lists.iter_mut() {
                l.sort_unstable();
                l.dedup();
            }
        }
        Self {
            o,
            n,
            classes,
            succ,
            pred,
            individuals: &i.individuals,
            top: o.builtin.top_object,
            bottom: o.builtin.bottom_object,
        }
    }

    fn ind(&self, a: Term) -> Option<usize> {
        self.individuals.get(&a).copied()
    }

    /// The `r`-successors of `d`, or `None` for the universal property (every element).
    fn successors(&self, r: ObjProp, d: usize) -> Option<&[usize]> {
        let p = r.named();
        if Some(p) == self.top {
            return None;
        }
        if Some(p) == self.bottom {
            return Some(&[]);
        }
        let table = match r {
            ObjProp::Named(_) => &self.succ,
            ObjProp::Inverse(_) => &self.pred,
        };
        Some(table.get(&p).map_or(&[][..], |l| &l[d][..]))
    }

    fn related(&self, r: ObjProp, d: usize, e: usize) -> bool {
        self.successors(r, d)
            .is_none_or(|s| s.binary_search(&e).is_ok())
    }

    /// The number of `r`-successors of `d` in `c`.
    fn count(&self, r: ObjProp, d: usize, c: &Set) -> usize {
        match self.successors(r, d) {
            None => c.elements().count(),
            Some(s) => s.iter().filter(|&&e| c.contains(e)).count(),
        }
    }

    fn each(&self, test: impl Fn(usize) -> bool) -> Set {
        let mut s = Set::empty(self.n);
        for d in 0..self.n {
            if test(d) {
                s.insert(d);
            }
        }
        s
    }

    fn eval(&self, e: ExprId) -> Result<Set, String> {
        let n = self.n;
        Ok(match self.o.classes.get(e.0) {
            ClassExpr::Class(t) => self
                .classes
                .get(t)
                .cloned()
                .unwrap_or_else(|| Set::empty(n)),
            ClassExpr::Thing => Set::full(n),
            ClassExpr::Nothing => Set::empty(n),
            ClassExpr::And(xs) => {
                let mut s = Set::full(n);
                for &x in xs {
                    s = s.and(&self.eval(x)?);
                }
                s
            }
            ClassExpr::Or(xs) => {
                let mut s = Set::empty(n);
                for &x in xs {
                    s = s.or(&self.eval(x)?);
                }
                s
            }
            ClassExpr::Not(x) => self.eval(*x)?.not(n),
            ClassExpr::OneOf(xs) => {
                let mut s = Set::empty(n);
                for &a in xs {
                    s.insert(self.ind(a).ok_or("an individual the model doesn't name")?);
                }
                s
            }
            ClassExpr::Some(r, x) => {
                let c = self.eval(*x)?;
                self.each(|d| self.count(*r, d, &c) > 0)
            }
            ClassExpr::All(r, x) => {
                let not = self.eval(*x)?.not(n);
                self.each(|d| self.count(*r, d, &not) == 0)
            }
            ClassExpr::HasValue(r, a) => {
                let a = self.ind(*a).ok_or("an individual the model doesn't name")?;
                self.each(|d| self.related(*r, d, a))
            }
            ClassExpr::HasSelf(r) => self.each(|d| self.related(*r, d, d)),
            ClassExpr::Min(k, r, x) => {
                let c = self.eval(*x)?;
                self.each(|d| self.count(*r, d, &c) >= *k as usize)
            }
            ClassExpr::Max(k, r, x) => {
                let c = self.eval(*x)?;
                self.each(|d| self.count(*r, d, &c) <= *k as usize)
            }
            ClassExpr::Exact(k, r, x) => {
                let c = self.eval(*x)?;
                self.each(|d| self.count(*r, d, &c) == *k as usize)
            }
            ClassExpr::DataSome(..)
            | ClassExpr::DataAll(..)
            | ClassExpr::DataHasValue(..)
            | ClassExpr::DataMin(..)
            | ClassExpr::DataMax(..)
            | ClassExpr::DataExact(..) => return Err("data values aren't certified yet".into()),
        })
    }

    /// Per element, the elements a chain of properties reaches.
    fn compose(&self, chain: &[ObjProp]) -> Vec<Set> {
        let n = self.n;
        let mut reach: Vec<Set> = (0..n)
            .map(|d| {
                let mut s = Set::empty(n);
                s.insert(d);
                s
            })
            .collect();
        for &r in chain {
            reach = reach
                .iter()
                .map(|from| {
                    let mut s = Set::empty(n);
                    for d in from.elements() {
                        match self.successors(r, d) {
                            None => return Set::full(n),
                            Some(succ) => {
                                for &e in succ {
                                    s.insert(e);
                                }
                            }
                        }
                    }
                    s
                })
                .collect();
        }
        reach
    }

    fn relation(&self, r: ObjProp) -> Vec<Set> {
        self.compose(&[r])
    }

    fn holds(&self, axiom: &Axiom) -> Result<bool, String> {
        let n = self.n;
        let c = |e: ExprId| self.eval(e);
        let pairwise_disjoint = |xs: &[ExprId]| -> Result<bool, String> {
            let sets: Vec<Set> = xs.iter().map(|&x| c(x)).collect::<Result<_, _>>()?;
            Ok(sets
                .iter()
                .enumerate()
                .all(|(k, a)| sets[k + 1..].iter().all(|b| a.and(b).is_empty())))
        };
        Ok(match axiom {
            Axiom::Declaration(..) => true,
            Axiom::SubClassOf(a, b) => c(*a)?.subset(&c(*b)?),
            Axiom::EquivalentClasses(xs) => {
                let sets: Vec<Set> = xs.iter().map(|&x| c(x)).collect::<Result<_, _>>()?;
                sets.windows(2).all(|w| w[0] == w[1])
            }
            Axiom::DisjointClasses(xs) => pairwise_disjoint(xs)?,
            Axiom::DisjointUnion(class, xs) => {
                let named = self
                    .classes
                    .get(class)
                    .cloned()
                    .unwrap_or_else(|| Set::empty(n));
                let mut union = Set::empty(n);
                for &x in xs {
                    union = union.or(&c(x)?);
                }
                named == union && pairwise_disjoint(xs)?
            }
            Axiom::SubObjectPropertyOf(chain, sup) => {
                let (got, have) = (self.compose(chain), self.relation(*sup));
                got.iter().zip(&have).all(|(g, h)| g.subset(h))
            }
            Axiom::EquivalentObjectProperties(ps) => {
                let rels: Vec<Vec<Set>> = ps.iter().map(|&p| self.relation(p)).collect();
                rels.windows(2).all(|w| w[0] == w[1])
            }
            Axiom::InverseObjectProperties(a, b) => self.relation(*a) == self.relation(b.inverse()),
            Axiom::DisjointObjectProperties(ps) => {
                let rels: Vec<Vec<Set>> = ps.iter().map(|&p| self.relation(p)).collect();
                rels.iter().enumerate().all(|(k, a)| {
                    rels[k + 1..]
                        .iter()
                        .all(|b| a.iter().zip(b).all(|(x, y)| x.and(y).is_empty()))
                })
            }
            Axiom::ObjectPropertyDomain(r, x) => {
                let x = c(*x)?;
                (0..n).all(|d| self.count(*r, d, &Set::full(n)) == 0 || x.contains(d))
            }
            Axiom::ObjectPropertyRange(r, x) => {
                let not = c(*x)?.not(n);
                (0..n).all(|d| self.count(*r, d, &not) == 0)
            }
            Axiom::ObjectCharacteristic(kind, r) => {
                let all = Set::full(n);
                (0..n).all(|d| match kind {
                    Characteristic::Functional => self.count(*r, d, &all) <= 1,
                    Characteristic::InverseFunctional => self.count(r.inverse(), d, &all) <= 1,
                    Characteristic::Reflexive => self.related(*r, d, d),
                    Characteristic::Irreflexive => !self.related(*r, d, d),
                    Characteristic::Symmetric => {
                        (0..n).all(|e| !self.related(*r, d, e) || self.related(*r, e, d))
                    }
                    Characteristic::Asymmetric => {
                        (0..n).all(|e| !self.related(*r, d, e) || !self.related(*r, e, d))
                    }
                    Characteristic::Transitive => {
                        let two = &self.compose(&[*r, *r])[d];
                        two.elements().all(|e| self.related(*r, d, e))
                    }
                })
            }
            Axiom::HasKey(class, objects, data) => {
                if !data.is_empty() {
                    return Err("data values aren't certified yet".into());
                }
                let members = c(*class)?;
                // Keys apply to named individuals only (the Direct Semantics' NAMED).
                let named: Vec<usize> = {
                    let mut v: Vec<usize> = self
                        .individuals
                        .iter()
                        .filter(|(t, _)| !self.o.anonymous.contains(t))
                        .map(|(_, &e)| e)
                        .collect();
                    v.sort_unstable();
                    v.dedup();
                    v
                };
                // Two named members that share a named value of each key property are one.
                named.iter().all(|&x| {
                    named.iter().all(|&y| {
                        x == y
                            || !members.contains(x)
                            || !members.contains(y)
                            || !objects.iter().all(|&r| {
                                named
                                    .iter()
                                    .any(|&z| self.related(r, x, z) && self.related(r, y, z))
                            })
                    })
                })
            }
            Axiom::ClassAssertion(x, a) => {
                let a = self.ind(*a).ok_or("an individual the model doesn't name")?;
                c(*x)?.contains(a)
            }
            Axiom::ObjectPropertyAssertion(p, a, b)
            | Axiom::NegativeObjectPropertyAssertion(p, a, b) => {
                let (a, b) = (
                    self.ind(*a).ok_or("an individual the model doesn't name")?,
                    self.ind(*b).ok_or("an individual the model doesn't name")?,
                );
                let related = self.related(ObjProp::Named(*p), a, b);
                related == matches!(axiom, Axiom::ObjectPropertyAssertion(..))
            }
            Axiom::SameIndividual(xs) => {
                let es: Vec<Option<usize>> = xs.iter().map(|&x| self.ind(x)).collect();
                es.iter().all(Option::is_some) && es.windows(2).all(|w| w[0] == w[1])
            }
            Axiom::DifferentIndividuals(xs) => {
                let es: Vec<usize> = xs
                    .iter()
                    .map(|&x| self.ind(x).ok_or("an individual the model doesn't name"))
                    .collect::<Result<_, _>>()?;
                es.iter()
                    .enumerate()
                    .all(|(k, a)| es[k + 1..].iter().all(|b| a != b))
            }
            Axiom::SubDataPropertyOf(..)
            | Axiom::EquivalentDataProperties(..)
            | Axiom::DisjointDataProperties(..)
            | Axiom::DataPropertyDomain(..)
            | Axiom::DataPropertyRange(..)
            | Axiom::FunctionalDataProperty(..)
            | Axiom::DatatypeDefinition(..)
            | Axiom::DataPropertyAssertion(..)
            | Axiom::NegativeDataPropertyAssertion(..) => {
                return Err("data values aren't certified yet".into());
            }
        })
    }
}

/// The axioms of `o` (by index) `i` isn't a model of, or why it can't say.
pub fn failures(o: &Ontology, i: &Interpretation) -> Result<Vec<usize>, String> {
    let view = View::new(o, i);
    let mut out = Vec::new();
    for (index, axiom) in o.axioms.iter().enumerate() {
        if !view.holds(axiom)? {
            out.push(index);
        }
    }
    Ok(out)
}

/// Whether `i` is a model of every axiom of `o`: `Ok(())`, or the first axiom it fails
/// (its index), or why it can't say (`Err(Err(why))`).
pub fn check(o: &Ontology, i: &Interpretation) -> Result<(), Result<usize, String>> {
    let view = View::new(o, i);
    for (index, axiom) in o.axioms.iter().enumerate() {
        match view.holds(axiom) {
            Ok(true) => {}
            Ok(false) => return Err(Ok(index)),
            Err(why) => return Err(Err(why)),
        }
    }
    Ok(())
}

/// The interpretation a hypertableau model stands for over the ontology's own names: its
/// named classes, properties and individuals (fresh names and the hub left out).
pub fn interpretation(m: &Model) -> Interpretation {
    let mut i = Interpretation {
        size: m.size,
        ..Interpretation::default()
    };
    for &(c, e) in &m.concepts {
        if let Concept::Named(t) = c {
            i.classes.entry(t).or_default().push(e);
        }
    }
    for &(r, a, b) in &m.roles {
        i.roles.entry(r).or_default().push((a, b));
    }
    for &(t, e) in &m.individuals {
        i.individuals.insert(t, e);
    }
    i
}

/// Closes the properties of `i` under the role inclusions of `o` (chains, transitivity,
/// symmetry, inverses, equivalences): the least superset of each property they allow.
pub fn close(o: &Ontology, i: &mut Interpretation) {
    let mut rias: Vec<(Vec<ObjProp>, ObjProp)> = Vec::new();
    for a in &o.axioms {
        match a {
            Axiom::SubObjectPropertyOf(chain, sup) => rias.push((chain.clone(), *sup)),
            Axiom::ObjectCharacteristic(Characteristic::Transitive, r) => {
                rias.push((vec![*r, *r], *r));
            }
            Axiom::ObjectCharacteristic(Characteristic::Symmetric, r) => {
                rias.push((vec![r.inverse()], *r));
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
    // Into the universal property: nothing to add; out of the empty one: nothing.
    let top = o.builtin.top_object;
    rias.retain(|(_, sup)| Some(sup.named()) != top);
    if rias.is_empty() {
        return;
    }
    loop {
        let view = View::new(o, i);
        let mut added: Vec<(Term, usize, usize)> = Vec::new();
        for (chain, sup) in &rias {
            let got = view.compose(chain);
            for (d, reach) in got.iter().enumerate() {
                for e in reach.elements() {
                    let (a, b) = match sup {
                        ObjProp::Named(_) => (d, e),
                        ObjProp::Inverse(_) => (e, d),
                    };
                    if !view.related(ObjProp::Named(sup.named()), a, b) {
                        added.push((sup.named(), a, b));
                    }
                }
            }
        }
        if added.is_empty() {
            return;
        }
        for (p, a, b) in added {
            i.roles.entry(p).or_default().push((a, b));
        }
        for pairs in i.roles.values_mut() {
            pairs.sort_unstable();
            pairs.dedup();
        }
    }
}

/// Whether the ontology uses what the checker doesn't certify yet (data).
fn declines(o: &Ontology) -> Option<&'static str> {
    let data_axiom = o.axioms.iter().any(|a| {
        matches!(
            a,
            Axiom::SubDataPropertyOf(..)
                | Axiom::EquivalentDataProperties(..)
                | Axiom::DisjointDataProperties(..)
                | Axiom::DataPropertyDomain(..)
                | Axiom::DataPropertyRange(..)
                | Axiom::FunctionalDataProperty(..)
                | Axiom::DatatypeDefinition(..)
                | Axiom::DataPropertyAssertion(..)
                | Axiom::NegativeDataPropertyAssertion(..)
        ) || matches!(a, Axiom::HasKey(_, _, data) if !data.is_empty())
    });
    let data_expr = (0..o.classes.len() as u32).any(|id| {
        matches!(
            o.classes.get(id),
            ClassExpr::DataSome(..)
                | ClassExpr::DataAll(..)
                | ClassExpr::DataHasValue(..)
                | ClassExpr::DataMin(..)
                | ClassExpr::DataMax(..)
                | ClassExpr::DataExact(..)
        )
    });
    (data_axiom || data_expr).then_some("data values aren't certified yet")
}

/// A certificate for "consistent" from the engine's model `m` of `o`: the model as it is,
/// closed under the role inclusions, or its covering lift (with each property's loops
/// kept or broken; up to 2⁸ choices); else the same for the unravelled graph.
pub fn certify(o: &Ontology, m: &Model) -> Certificate {
    if let Some(why) = declines(o) {
        return Certificate::Declined(why.into());
    }
    let folded = certify_one(o, m);
    if folded.is_model() {
        return folded;
    }
    match &m.unravelled {
        Some(u) => match certify_one(o, u) {
            Certificate::Model { elements, .. } => Certificate::Model {
                elements,
                lifted: true,
            },
            declined => declined,
        },
        None => folded,
    }
}

/// [`certify`] for one candidate.
fn certify_one(o: &Ontology, m: &Model) -> Certificate {
    let mut i = interpretation(m);
    // An individual the ontology names where nothing constrains it (`≥ 0 R.{a}`) is in no
    // clause, so not in the model: it may be any element (the check decides).
    if i.size > 0 {
        for a in o.named_individuals() {
            i.individuals.entry(a).or_insert(0);
        }
    }
    close(o, &mut i);
    match check(o, &i) {
        Ok(()) => {
            return Certificate::Model {
                elements: i.size,
                lifted: false,
            };
        }
        Err(Err(why)) => return Certificate::Declined(why),
        Err(Ok(_)) => {}
    }
    let named: Vec<bool> = (0..i.size)
        .map(|e| i.individuals.values().any(|&v| v == e))
        .collect();
    let mut looped: Vec<Term> = i
        .roles
        .iter()
        .filter(|(_, pairs)| pairs.iter().any(|&(a, b)| a == b && !named[a]))
        .map(|(&p, _)| p)
        .collect();
    looped.sort_unstable();
    looped.truncate(8);
    for mask in 0u32..(1 << looped.len()) {
        let keep = |p: Term| {
            looped
                .iter()
                .position(|&q| q == p)
                .is_some_and(|k| mask & (1 << k) != 0)
        };
        let mut l = lift(&i, &named, &keep);
        close(o, &mut l);
        if check(o, &l).is_ok() {
            return Certificate::Model {
                elements: l.size,
                lifted: true,
            };
        }
    }
    Certificate::Declined("neither the folded model nor its lift is a model".into())
}

/// The 3-fold covering lift of a folded model: each element that isn't an individual
/// becomes three copies with its classes, an edge between two such elements runs from
/// copy `j` to copy `j + 1`, and an individual stays one element, joined to every copy of
/// its neighbours. Every copy's successors and predecessors have the classes its
/// original's have, so what doesn't count holds as before; a cycle through anonymous
/// elements gets a length divisible by three, so no loop or 2-cycle a fold closed is left
/// (an irreflexive or asymmetric property forbids them, while the unravelled model the
/// fold stands for is a model). `keep_loops`: the properties whose loops on an anonymous
/// element stay loops on each copy (a `Self` restriction or a reflexive property needs
/// them).
fn lift(i: &Interpretation, named: &[bool], keep_loops: &dyn Fn(Term) -> bool) -> Interpretation {
    let mut copies: Vec<[usize; 3]> = Vec::with_capacity(i.size);
    let mut n = 0usize;
    for &individual in named {
        if individual {
            copies.push([n; 3]);
            n += 1;
        } else {
            copies.push([n, n + 1, n + 2]);
            n += 3;
        }
    }
    let mut out = Interpretation {
        size: n,
        individuals: i
            .individuals
            .iter()
            .map(|(&t, &e)| (t, copies[e][0]))
            .collect(),
        ..Interpretation::default()
    };
    for (&c, elements) in &i.classes {
        let mut lifted: Vec<usize> = elements.iter().flat_map(|&e| copies[e]).collect();
        lifted.sort_unstable();
        lifted.dedup();
        out.classes.insert(c, lifted);
    }
    for (&p, pairs) in &i.roles {
        let mut lifted: Vec<(usize, usize)> = Vec::new();
        for &(u, v) in pairs {
            match (named[u], named[v]) {
                (true, true) => lifted.push((copies[u][0], copies[v][0])),
                (true, false) => lifted.extend(copies[v].map(|x| (copies[u][0], x))),
                (false, true) => lifted.extend(copies[u].map(|x| (x, copies[v][0]))),
                (false, false) => {
                    for j in 0..3 {
                        let to = if u == v && keep_loops(p) {
                            j
                        } else {
                            (j + 1) % 3
                        };
                        lifted.push((copies[u][j], copies[v][to]));
                    }
                }
            }
        }
        lifted.sort_unstable();
        lifted.dedup();
        out.roles.insert(p, lifted);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use nrese_owl::ExprId;

    fn ontology() -> (Ontology, [ExprId; 3]) {
        let mut o = Ontology::default();
        let a = ExprId(o.classes.intern(ClassExpr::Class(1)));
        let some = ExprId(o.classes.intern(ClassExpr::Some(ObjProp::Named(10), a)));
        let not_a = ExprId(o.classes.intern(ClassExpr::Not(a)));
        (o, [a, some, not_a])
    }

    /// A model is certified and a non-model isn't; the universal property relates every
    /// pair however the interpretation's own edges go.
    #[test]
    fn models_pass_and_non_models_fail() {
        let (mut o, [a, some, _]) = ontology();
        o.axioms = vec![Axiom::SubClassOf(a, some), Axiom::ClassAssertion(a, 100)];
        let mut i = Interpretation {
            size: 2,
            ..Interpretation::default()
        };
        i.classes.insert(1, vec![0, 1]);
        i.individuals.insert(100, 0);
        assert_eq!(check(&o, &i), Err(Ok(0)));
        i.roles.insert(10, vec![(0, 1), (1, 1)]);
        assert_eq!(check(&o, &i), Ok(()));
        // ∀U.¬A at the individual: U reaches the A-elements, whatever U's own edges.
        o.builtin.top_object = Some(11);
        let not_a = ExprId(o.classes.intern(ClassExpr::Not(a)));
        let all = ExprId(o.classes.intern(ClassExpr::All(ObjProp::Named(11), not_a)));
        o.axioms.push(Axiom::ClassAssertion(all, 100));
        assert_eq!(check(&o, &i), Err(Ok(2)));
    }

    /// Closure adds what a chain implies; a transitive property's folded loop is broken
    /// by the lift where it is irreflexive.
    #[test]
    fn closure_and_lift() {
        let (mut o, [a, some, _]) = ontology();
        let r = ObjProp::Named(10);
        o.axioms = vec![
            Axiom::SubClassOf(a, some),
            Axiom::ClassAssertion(a, 100),
            Axiom::ObjectCharacteristic(Characteristic::Transitive, r),
            Axiom::ObjectCharacteristic(Characteristic::Irreflexive, r),
        ];
        // The engine's fold: an anonymous element blocked by itself (a loop).
        let m = Model {
            size: 2,
            concepts: vec![(Concept::Named(1), 0), (Concept::Named(1), 1)],
            roles: vec![(10, 0, 1), (10, 1, 1)],
            individuals: vec![(100, 0)],
            unravelled: None,
        };
        // Folded, transitive and irreflexive clash at the loop; the lift breaks it but
        // closes a 3-cycle, which transitivity makes reflexive again: no certificate.
        assert!(!certify(&o, &m).is_model());
        // Without irreflexivity the folded model passes.
        o.axioms.pop();
        assert_eq!(
            certify(&o, &m),
            Certificate::Model {
                elements: 2,
                lifted: false
            }
        );
    }
}
