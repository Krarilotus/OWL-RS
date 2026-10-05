//! Conclusions with anonymous individuals: their assertions say that *some* individuals
//! are so connected, an existential. Each connected group of anonymous individuals, if it
//! is a tree, rolls up into a class expression `C` (named neighbours as `HasValue`), and
//! `O ⊨ ∃x. C(x)` iff `O ∪ {⊤ ⊑ ¬C}` is inconsistent.

use std::collections::{BTreeMap, BTreeSet};

use nrese_owl::{Axiom, ClassExpr, ExprId, ObjProp, Ontology, Term};

use crate::negate::copy;

/// What the conclusion says about one anonymous individual.
#[derive(Default)]
struct Node {
    types: Vec<ExprId>,
    /// Neighbours: the property as seen from this node, and the other node.
    blank: Vec<(ObjProp, Term)>,
    named: Vec<(ObjProp, Term)>,
    data: Vec<(Term, Term)>,
}

/// The rolled-up class expression of each group of anonymous individuals the `axioms`
/// (of `from`) assert about, in `to`'s interner; why not, where a group isn't a tree or an
/// axiom can't be rolled up.
pub fn roll_up(
    from: &Ontology,
    axioms: &[&Axiom],
    is_blank: impl Fn(Term) -> bool,
    to: &mut Ontology,
) -> Result<Vec<ExprId>, String> {
    let mut nodes: BTreeMap<Term, Node> = BTreeMap::new();
    let mut edges = 0usize;
    for axiom in axioms {
        match axiom {
            Axiom::ClassAssertion(c, x) if is_blank(*x) => {
                let c = copy(from, *c, to);
                nodes.entry(*x).or_default().types.push(c);
            }
            Axiom::ObjectPropertyAssertion(p, x, y) => {
                let (fwd, back) = (ObjProp::Named(*p), ObjProp::Inverse(*p));
                match (is_blank(*x), is_blank(*y)) {
                    (true, true) => {
                        nodes.entry(*x).or_default().blank.push((fwd, *y));
                        nodes.entry(*y).or_default().blank.push((back, *x));
                        edges += 1;
                    }
                    (true, false) => nodes.entry(*x).or_default().named.push((fwd, *y)),
                    (false, true) => nodes.entry(*y).or_default().named.push((back, *x)),
                    (false, false) => {}
                }
            }
            Axiom::DataPropertyAssertion(p, x, v) if is_blank(*x) => {
                nodes.entry(*x).or_default().data.push((*p, *v));
            }
            other => {
                return Err(format!(
                    "not-run: anonymous individuals in {}",
                    format!("{other:?}").split('(').next().unwrap_or("")
                ));
            }
        }
    }
    // Trees: as many edges as nodes less one per group, and no group revisits a node.
    let mut seen = BTreeSet::new();
    let mut groups = Vec::new();
    for &root in nodes.keys() {
        if seen.contains(&root) {
            continue;
        }
        let mut stack = vec![root];
        seen.insert(root);
        while let Some(x) = stack.pop() {
            for &(_, y) in &nodes[&x].blank {
                if seen.insert(y) {
                    stack.push(y);
                }
            }
        }
        groups.push(root);
    }
    if edges + groups.len() != nodes.len() {
        return Err("not-run: anonymous individuals that aren't a tree".into());
    }
    Ok(groups
        .into_iter()
        .map(|root| expression(&nodes, root, None, to))
        .collect())
}

fn expression(
    nodes: &BTreeMap<Term, Node>,
    x: Term,
    parent: Option<Term>,
    to: &mut Ontology,
) -> ExprId {
    let node = &nodes[&x];
    let mut parts = node.types.clone();
    for &(r, a) in &node.named {
        parts.push(e(to, ClassExpr::HasValue(r, a)));
    }
    for &(p, v) in &node.data {
        parts.push(e(to, ClassExpr::DataHasValue(p, v)));
    }
    for &(r, y) in &node.blank {
        if Some(y) == parent {
            continue;
        }
        let filler = expression(nodes, y, Some(x), to);
        parts.push(e(to, ClassExpr::Some(r, filler)));
    }
    parts.sort();
    parts.dedup();
    match parts.len() {
        0 => e(to, ClassExpr::Thing),
        1 => parts[0],
        _ => e(to, ClassExpr::And(parts)),
    }
}

fn e(o: &mut Ontology, expr: ClassExpr) -> ExprId {
    ExprId(o.classes.intern(expr))
}
