//! The properties an axiom mentions, object and data alike, in its class expressions too:
//! for the built-in properties ([`crate::mapping::BuiltinProperties`]), which the
//! normalisation must not take for ordinary ones.

use std::collections::HashSet;

use crate::mapping::Ontology;
use crate::model::{Axiom, ClassExpr, ExprId, ObjProp, Term};

/// Whether `axiom` mentions a property `wanted` holds of (not counting a declaration).
pub(crate) fn mentions(ontology: &Ontology, axiom: &Axiom, wanted: &dyn Fn(Term) -> bool) -> bool {
    let p = |r: &ObjProp| wanted(r.named());
    let ps = |rs: &[ObjProp]| rs.iter().any(p);
    let ds = |ts: &[Term]| ts.iter().any(|&t| wanted(t));
    let in_exprs = |xs: &[ExprId]| xs.iter().any(|&x| in_expr(ontology, x, wanted));
    match axiom {
        Axiom::Declaration(..) => false,
        Axiom::SubClassOf(a, b) => in_exprs(&[*a, *b]),
        Axiom::EquivalentClasses(xs) | Axiom::DisjointClasses(xs) => in_exprs(xs),
        Axiom::DisjointUnion(_, xs) => in_exprs(xs),
        Axiom::SubObjectPropertyOf(chain, sup) => ps(chain) || p(sup),
        Axiom::EquivalentObjectProperties(rs) | Axiom::DisjointObjectProperties(rs) => ps(rs),
        Axiom::InverseObjectProperties(a, b) => p(a) || p(b),
        Axiom::ObjectPropertyDomain(r, c) | Axiom::ObjectPropertyRange(r, c) => {
            p(r) || in_exprs(&[*c])
        }
        Axiom::ObjectCharacteristic(_, r) => p(r),
        Axiom::SubDataPropertyOf(a, b) => wanted(*a) || wanted(*b),
        Axiom::EquivalentDataProperties(ts) | Axiom::DisjointDataProperties(ts) => ds(ts),
        Axiom::DataPropertyDomain(d, c) => wanted(*d) || in_exprs(&[*c]),
        Axiom::DataPropertyRange(d, _) | Axiom::FunctionalDataProperty(d) => wanted(*d),
        Axiom::DatatypeDefinition(..)
        | Axiom::SameIndividual(_)
        | Axiom::DifferentIndividuals(_) => false,
        Axiom::HasKey(c, rs, ts) => in_exprs(&[*c]) || ps(rs) || ds(ts),
        Axiom::ClassAssertion(c, _) => in_exprs(&[*c]),
        Axiom::ObjectPropertyAssertion(r, ..)
        | Axiom::NegativeObjectPropertyAssertion(r, ..)
        | Axiom::DataPropertyAssertion(r, ..)
        | Axiom::NegativeDataPropertyAssertion(r, ..) => wanted(*r),
    }
}

fn in_expr(ontology: &Ontology, id: ExprId, wanted: &dyn Fn(Term) -> bool) -> bool {
    let mut stack = vec![id];
    let mut seen = HashSet::new();
    while let Some(id) = stack.pop() {
        if !seen.insert(id) {
            continue;
        }
        match ontology.class(id) {
            ClassExpr::Class(_) | ClassExpr::Thing | ClassExpr::Nothing | ClassExpr::OneOf(_) => {}
            ClassExpr::And(xs) | ClassExpr::Or(xs) => stack.extend(xs),
            ClassExpr::Not(x) => stack.push(*x),
            ClassExpr::Some(r, x)
            | ClassExpr::All(r, x)
            | ClassExpr::Min(_, r, x)
            | ClassExpr::Max(_, r, x)
            | ClassExpr::Exact(_, r, x) => {
                if wanted(r.named()) {
                    return true;
                }
                stack.push(*x);
            }
            ClassExpr::HasValue(r, _) | ClassExpr::HasSelf(r) => {
                if wanted(r.named()) {
                    return true;
                }
            }
            ClassExpr::DataSome(d, _)
            | ClassExpr::DataAll(d, _)
            | ClassExpr::DataHasValue(d, _)
            | ClassExpr::DataMin(_, d, _)
            | ClassExpr::DataMax(_, d, _)
            | ClassExpr::DataExact(_, d, _) => {
                if wanted(*d) {
                    return true;
                }
            }
        }
    }
    false
}
