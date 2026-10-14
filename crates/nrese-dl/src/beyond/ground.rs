//! Rule instances as OWL 2 DL axioms. A DL-safe rule's instance over named individuals and
//! data values is an implication between facts about them, which OWL 2 DL writes with
//! nominals: for a pivot individual `a` the body names,
//!
//! `{a} ⊓ Bₐ ⊓ ∃U.({b} ⊓ B_b) ⊓ … ⊑ Hₐ ⊓ ∃U.({c} ⊓ H_c) ⊓ …`
//!
//! where `Bₓ` (`Hₓ`) is the conjunction of the body's (head's) atoms about `x`: `C` for
//! `C(x)`, `∃p.{y}` for `p(x, y)`, `∃d.{v}` for `d(x, v)`, `{y}` for `x = y`, `¬{y}` for
//! `x ≠ y`; `U` is `owl:topObjectProperty`, and the head is `⊥` where it is empty. Built-in
//! and data-range atoms have constant arguments in an instance: they are evaluated, and
//! drop out (true) or void the instance (a false body atom) or the head (`⊥`).
//!
//! The instance follows from the rule (*DL-safe semantics*, `nrese_owl::rules`), so adding
//! it changes no model of the input. Where every body atom is an assertion of the
//! ontology, the head's atoms are entailed and are added as assertions instead.

use std::collections::{HashMap, HashSet};

use nrese_owl::rules::{Atom, DArg, IArg, Rule};
use nrese_owl::{Axiom, ClassExpr, ExprId, Literal, ObjProp, Ontology, Term};
use nrese_xsd::owl::{Rational, Value};

use crate::certificate::Binding;
use crate::certificate::data::{Values, built_in};

const OWL: &str = "http://www.w3.org/2002/07/owl#";
const XSD: &str = "http://www.w3.org/2001/XMLSchema#";
const RDF: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#";

/// The first term id minted for a literal or IRI the ontology doesn't have (counting
/// down): far above any store's ids.
const MINTED: Term = u64::MAX - 1;

/// An atom with its arguments bound: individuals and literal terms.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum Fact {
    Class(ExprId, Term),
    Object(ObjProp, Term, Term),
    Data(Term, Term, Term),
    Same(Term, Term),
    Different(Term, Term),
}

/// Grounds rule instances into an ontology.
pub struct Grounder {
    /// Literal terms by value: the ontology's, then minted ones.
    literals: HashMap<Value, Term>,
    next: Term,
    /// The assertions of the ontology and those added.
    facts: HashSet<Axiom>,
    /// The instances added, by rule and bound arguments.
    done: HashSet<(usize, Vec<Term>)>,
}

/// A data value as a literal: its lexical form and datatype IRI (and tag).
fn literal_of(v: &Value) -> Result<Literal, String> {
    let typed = |lexical: String, datatype: &str| Literal {
        lexical,
        datatype: Some(datatype.to_owned()),
        language: None,
    };
    Ok(match v {
        Value::Real(r) if r.is_integer() => {
            typed(r.numerator().to_string(), &format!("{XSD}integer"))
        }
        Value::Real(r) if r.is_decimal() => typed(decimal(*r), &format!("{XSD}decimal")),
        Value::Real(r) => typed(
            format!("{}/{}", r.numerator(), r.denominator()),
            &format!("{OWL}rational"),
        ),
        Value::String(s) => typed(s.clone(), &format!("{XSD}string")),
        Value::LangString(s, tag) => Literal {
            lexical: s.clone(),
            datatype: Some(format!("{RDF}langString")),
            language: Some(tag.clone()),
        },
        Value::Boolean(b) => typed(b.to_string(), &format!("{XSD}boolean")),
        other => return Err(format!("{other:?} isn't written as a literal here")),
    })
}

/// A decimal's lexical form (the denominator divides a power of ten).
fn decimal(r: Rational) -> String {
    let (mut n, d) = (r.numerator(), r.denominator());
    let negative = n < 0;
    n = n.abs();
    let mut digits = 0u32;
    let mut scale = 1i128;
    while scale % d != 0 {
        scale *= 10;
        digits += 1;
    }
    let scaled = n * (scale / d);
    let text = format!("{scaled:0>width$}", width = digits as usize + 1);
    let (whole, fraction) = text.split_at(text.len() - digits as usize);
    format!("{}{whole}.{fraction}", if negative { "-" } else { "" })
}

impl Grounder {
    pub fn new(o: &Ontology) -> Self {
        let values = Values::new(o);
        let mut literals = HashMap::new();
        for &t in o.data.literals.keys() {
            if let Ok(v) = values.literal(t) {
                literals.entry(v).or_insert(t);
            }
        }
        let facts = o
            .axioms
            .iter()
            .filter(|a| a.is_assertion())
            .cloned()
            .collect();
        let used = o
            .data
            .literals
            .keys()
            .chain(o.data.iris.keys())
            .copied()
            .filter(|&t| t > MINTED / 2)
            .min()
            .unwrap_or(MINTED + 1);
        Self {
            literals,
            next: used - 1,
            facts,
            done: HashSet::new(),
        }
    }

    fn mint(&mut self) -> Term {
        let t = self.next;
        self.next -= 1;
        t
    }

    /// The literal term of `v`: the ontology's, or a new one.
    fn literal(&mut self, o: &mut Ontology, v: &Value) -> Result<Term, String> {
        if let Some(&t) = self.literals.get(v) {
            return Ok(t);
        }
        let l = literal_of(v)?;
        let t = self.mint();
        if let Some(d) = &l.datatype {
            o.data.iris.entry(t).or_insert_with(|| d.clone());
        }
        o.data.literals.insert(t, l);
        self.literals.insert(v.clone(), t);
        Ok(t)
    }

    /// `owl:topObjectProperty`'s term: the ontology's, or a new one.
    fn universal(&mut self, o: &mut Ontology) -> Term {
        if let Some(t) = o.builtin.top_object {
            return t;
        }
        let t = self.mint();
        o.data.iris.insert(t, format!("{OWL}topObjectProperty"));
        o.builtin.top_object = Some(t);
        t
    }

    fn add(&mut self, o: &mut Ontology, a: Axiom) -> bool {
        if a.is_assertion() && !self.facts.insert(a.clone()) {
            return false;
        }
        o.axioms.push(a);
        o.sources.push(Vec::new());
        true
    }

    fn asserted(&self, f: &Fact, o: &Ontology) -> bool {
        match *f {
            Fact::Class(c, a) => {
                matches!(o.classes.get(c.0), ClassExpr::Thing)
                    || self.facts.contains(&Axiom::ClassAssertion(c, a))
            }
            Fact::Object(ObjProp::Named(p), a, b) => {
                self.facts.contains(&Axiom::ObjectPropertyAssertion(p, a, b))
            }
            Fact::Object(ObjProp::Inverse(p), a, b) => {
                self.facts.contains(&Axiom::ObjectPropertyAssertion(p, b, a))
            }
            Fact::Data(p, a, v) => self.facts.contains(&Axiom::DataPropertyAssertion(p, a, v)),
            Fact::Same(a, b) => {
                a == b
                    || self.facts.iter().any(|x| {
                        matches!(x, Axiom::SameIndividual(xs) if xs.contains(&a) && xs.contains(&b))
                    })
            }
            Fact::Different(a, b) => self.facts.iter().any(|x| {
                matches!(x, Axiom::DifferentIndividuals(xs) if xs.contains(&a) && xs.contains(&b))
            }),
        }
    }

    /// Adds the instance of `rule` (index `index`) under `binding` (elements named by
    /// `name`); whether it was new. `Err`: it can't be written (a value without a literal
    /// form here, an element no named individual denotes, a built-in not evaluated).
    pub fn ground(
        &mut self,
        o: &mut Ontology,
        index: usize,
        rule: &Rule,
        binding: &[Binding],
        name: &dyn Fn(usize) -> Option<Term>,
    ) -> Result<bool, String> {
        // The bound arguments: individuals by name, values as literal terms.
        let mut args: Vec<Term> = Vec::with_capacity(binding.len());
        let mut values: Vec<Option<Value>> = Vec::with_capacity(binding.len());
        for b in binding {
            match b {
                Binding::Element(e) => {
                    args.push(name(*e).ok_or("an element no named individual denotes")?);
                    values.push(None);
                }
                Binding::Value(v) => {
                    args.push(self.literal(o, v)?);
                    values.push(Some(v.clone()));
                }
            }
        }
        if !self.done.insert((index, args.clone())) {
            return Ok(false);
        }
        let ind = |a: &IArg| match *a {
            IArg::Var(v) => args[v as usize],
            IArg::Individual(t) => t,
        };
        let checker = Values::new(o);
        let value = |a: &DArg| -> Result<Value, String> {
            match a {
                DArg::Var(v) => values[*v as usize]
                    .clone()
                    .ok_or_else(|| "an individual bound as a data value".to_owned()),
                DArg::Literal(t) => checker.literal(*t),
            }
        };
        // Each side's facts; `false` where a constant atom fails.
        let mut sides: [(Vec<Fact>, bool); 2] = [(Vec::new(), true), (Vec::new(), true)];
        let mut data_args: Vec<(usize, Term, IArg, DArg)> = Vec::new();
        for (side, atoms) in [&rule.body, &rule.head].into_iter().enumerate() {
            for atom in atoms {
                let fact = match atom {
                    Atom::Class(c, x) => Fact::Class(*c, ind(x)),
                    Atom::Object(p, x, y) => Fact::Object(*p, ind(x), ind(y)),
                    Atom::Same(x, y) => Fact::Same(ind(x), ind(y)),
                    Atom::Different(x, y) => Fact::Different(ind(x), ind(y)),
                    Atom::Data(p, x, v) => {
                        data_args.push((side, *p, *x, *v));
                        continue;
                    }
                    Atom::Range(r, v) => {
                        sides[side].1 &= checker.member(*r, &value(v)?)?;
                        continue;
                    }
                    Atom::BuiltIn(b, xs) => {
                        let iri = o
                            .data
                            .iris
                            .get(b)
                            .cloned()
                            .ok_or("a built-in without an IRI")?;
                        let vs: Vec<Value> = xs.iter().map(&value).collect::<Result<_, _>>()?;
                        let (first, rest) =
                            vs.split_first().ok_or("a built-in without arguments")?;
                        sides[side].1 &= built_in(&iri, Some(first), rest)?.is_some();
                        continue;
                    }
                };
                sides[side].0.push(fact);
            }
        }
        drop(checker);
        for (side, p, x, v) in data_args {
            let t = match v {
                DArg::Var(k) => args[k as usize],
                DArg::Literal(t) => t,
            };
            sides[side].0.push(Fact::Data(p, ind(&x), t));
        }
        let [(body, body_holds), (head, head_holds)] = sides;
        if !body_holds {
            return Ok(false);
        }
        // An empty head is false (SWRL §3): the instance says its body doesn't hold. Of the
        // facts, `a = a` is true and `a ≠ a` false.
        let reflexive = |f: &Fact| matches!(*f, Fact::Same(a, b) | Fact::Different(a, b) if a == b);
        if body
            .iter()
            .any(|f| matches!(*f, Fact::Different(a, b) if a == b))
        {
            return Ok(false);
        }
        let head_holds = head_holds
            && !rule.head.is_empty()
            && !head
                .iter()
                .any(|f| matches!(*f, Fact::Different(a, b) if a == b));
        let body: Vec<Fact> = body.into_iter().filter(|f| !reflexive(f)).collect();
        let head: Vec<Fact> = head.into_iter().filter(|f| !reflexive(f)).collect();
        // A body of assertions: the head is entailed.
        if body.iter().all(|f| self.asserted(f, o)) {
            if !head_holds {
                let (thing, nothing) = (
                    ExprId(o.classes.intern(ClassExpr::Thing)),
                    ExprId(o.classes.intern(ClassExpr::Nothing)),
                );
                self.add(o, Axiom::SubClassOf(thing, nothing));
                return Ok(true);
            }
            let mut added = false;
            for f in head {
                let a = match f {
                    Fact::Class(c, a) => Axiom::ClassAssertion(c, a),
                    Fact::Object(ObjProp::Named(p), a, b) => {
                        Axiom::ObjectPropertyAssertion(p, a, b)
                    }
                    Fact::Object(ObjProp::Inverse(p), a, b) => {
                        Axiom::ObjectPropertyAssertion(p, b, a)
                    }
                    Fact::Data(p, a, v) => Axiom::DataPropertyAssertion(p, a, v),
                    Fact::Same(a, b) if a == b => continue,
                    Fact::Same(a, b) => Axiom::SameIndividual(sorted(vec![a, b])),
                    Fact::Different(a, b) => Axiom::DifferentIndividuals(sorted(vec![a, b])),
                };
                added |= self.add(o, a);
            }
            return Ok(added);
        }
        // Else the nominal form.
        let pivot = match body.first().or(head.first()) {
            Some(f) => subject(f),
            None => return Ok(false),
        };
        let lhs = self.conjunction(o, pivot, &body, true);
        let rhs = if head_holds {
            self.conjunction(o, pivot, &head, false)
        } else {
            ExprId(o.classes.intern(ClassExpr::Nothing))
        };
        Ok(self.add(o, Axiom::SubClassOf(lhs, rhs)))
    }

    /// The conjunction of `facts` at `pivot` (with `{pivot}` on the body's side).
    fn conjunction(&mut self, o: &mut Ontology, pivot: Term, facts: &[Fact], body: bool) -> ExprId {
        let mut by: Vec<(Term, Vec<ExprId>)> = Vec::new();
        for f in facts {
            let x = subject(f);
            let e = local(o, f);
            match by.iter_mut().find(|(t, _)| *t == x) {
                Some((_, es)) => es.push(e),
                None => by.push((x, vec![e])),
            }
        }
        let mut parts: Vec<ExprId> = Vec::new();
        if body {
            parts.push(ExprId(o.classes.intern(ClassExpr::OneOf(vec![pivot]))));
        }
        for (x, es) in by {
            let here = and(o, es);
            if x == pivot {
                parts.push(here);
            } else {
                let u = self.universal(o);
                let at = ExprId(o.classes.intern(ClassExpr::OneOf(vec![x])));
                let there = and(o, vec![at, here]);
                parts.push(ExprId(
                    o.classes.intern(ClassExpr::Some(ObjProp::Named(u), there)),
                ));
            }
        }
        and(o, parts)
    }
}

fn sorted(mut v: Vec<Term>) -> Vec<Term> {
    v.sort_unstable();
    v.dedup();
    v
}

/// The individual a fact is about.
fn subject(f: &Fact) -> Term {
    match *f {
        Fact::Class(_, a)
        | Fact::Object(_, a, _)
        | Fact::Data(_, a, _)
        | Fact::Same(a, _)
        | Fact::Different(a, _) => a,
    }
}

/// The class of the elements a fact holds at, as its subject.
fn local(o: &mut Ontology, f: &Fact) -> ExprId {
    let e = match *f {
        Fact::Class(c, _) => return c,
        Fact::Object(p, _, b) => {
            let one = ExprId(o.classes.intern(ClassExpr::OneOf(vec![b])));
            ClassExpr::Some(p, one)
        }
        Fact::Data(p, _, v) => ClassExpr::DataHasValue(p, v),
        Fact::Same(_, b) => ClassExpr::OneOf(vec![b]),
        Fact::Different(_, b) => {
            let one = ExprId(o.classes.intern(ClassExpr::OneOf(vec![b])));
            ClassExpr::Not(one)
        }
    };
    ExprId(o.classes.intern(e))
}

/// The conjunction of `es` (one operand: itself; none: `owl:Thing`), operands sorted.
fn and(o: &mut Ontology, mut es: Vec<ExprId>) -> ExprId {
    es.sort_unstable();
    es.dedup();
    match es.len() {
        0 => ExprId(o.classes.intern(ClassExpr::Thing)),
        1 => es[0],
        _ => ExprId(o.classes.intern(ClassExpr::And(es))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn literals_of_values() {
        let r = |n, d| Value::Real(Rational::new(n, d).expect("small"));
        assert_eq!(literal_of(&r(3, 1)).expect("int").lexical, "3");
        assert_eq!(literal_of(&r(-1, 4)).expect("decimal").lexical, "-0.25");
        assert_eq!(literal_of(&r(1, 3)).expect("rational").lexical, "1/3");
        assert_eq!(decimal(Rational::new(5, 2).expect("small")), "2.5");
    }
}
