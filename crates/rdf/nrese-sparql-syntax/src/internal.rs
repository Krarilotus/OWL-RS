//! What the engine names for itself, apart from every name a query or a store can hold.
//!
//! An executor makes variables the query doesn't name (a pattern's blank node, the parts of
//! a triple term pattern, the columns it adds), and puts constants into a pattern for terms
//! the algebra has no syntax for (a stored blank node, an identity's representative). Each
//! is made here, in a form nothing from outside has:
//!
//! - **a made variable's name holds a space**, and no SPARQL `VARNAME` does: the parser's
//!   variables, [`Variable::new`]'s, and every other name that comes in are `VARNAME`s.
//!   The first word is the kind (`blank`, `exists`, `triple-term`, …), so kinds differ too;
//! - **a made constant is a [`NamedNode`] whose text begins with a space**, and no IRI
//!   (RFC 3987) has one: every stored or written IRI is an IRI.
//!
//! A query that spells a name like one of these still means its own variable or its own
//! IRI. Text for another endpoint names none of them ([`printable`]).

use std::collections::{HashMap, HashSet};
use std::fmt::Display;

use nrese_rdf::{BlankNode, NamedNode, Term, Variable};

use crate::algebra::{Expression, Function, GraphPattern};
use crate::term::{GroundTerm, NamedNodePattern, TermPattern, TriplePattern};
use crate::visit::NodeMut;

/// A made variable: `kind` (one word) and `n`.
pub fn variable(kind: &str, n: impl Display) -> Variable {
    debug_assert!(
        !kind.is_empty() && !kind.contains(' '),
        "a kind is one word"
    );
    Variable::new_unchecked(format!("{kind} {n}"))
}

/// The variable a pattern's blank node stands for (SPARQL 1.1 §18.3: a blank node of a
/// basic graph pattern acts as a variable), apart from every variable the query names.
pub fn blank_variable(b: &BlankNode) -> Variable {
    variable("blank", b.as_str())
}

/// Whether `v` is a made variable.
pub fn is_variable(v: &Variable) -> bool {
    v.as_str().contains(' ')
}

/// The constant that stands for the blank node `label` of the store.
pub fn blank_constant(label: &str) -> NamedNode {
    NamedNode::new_unchecked(format!(" blank {label}"))
}

/// The constant that stands for the stored term `id`, the representative of its class.
pub fn class_constant(id: u64) -> NamedNode {
    NamedNode::new_unchecked(format!(" class {id}"))
}

/// A constant that stands for no term: a literal put in where only an IRI may stand.
pub fn no_term() -> NamedNode {
    NamedNode::new_unchecked(" nothing")
}

/// Whether `iri` is a made constant's text.
pub fn is_constant(iri: &str) -> bool {
    iri.starts_with(' ')
}

/// The namespace a made constant is sent in to another endpoint, as SERVICE calls sent
/// them before: the endpoint can't hold the local term it stands for.
const SENT_CONSTANT: &str = "urn:nrese:substituted-blank-node:";

/// `pattern` as another endpoint can read it: each made variable renamed to one the
/// pattern doesn't use, each made constant in its previous spelling ([`SENT_CONSTANT`]);
/// with the names to read the endpoint's answer by ([`Sent::local`]). A blank node whose
/// made variable ([`blank_variable`]) the pattern names too is written as that renamed
/// variable, so the two stay one node. A pattern that would match a made constant matches
/// nothing ([`unmatchable`]). Unchanged, and nothing renamed, when the pattern holds no
/// made name.
pub fn printable(pattern: GraphPattern) -> (GraphPattern, Sent) {
    printable_names(pattern)
}

/// Prints resolved ground triple values as RDF1.2 terms (or TRIPLE expressions).
/// Blank identities remain local-only, as in printable without a resolver.
pub fn printable_with_values(
    mut pattern: GraphPattern,
    value: impl Fn(&str) -> Option<Term>,
) -> (GraphPattern, Sent) {
    let ground = |n: &NamedNode| {
        if !is_constant(n.as_str()) {
            return None;
        }
        let term = value(n.as_str())?;
        if !matches!(term, Term::Triple(_)) {
            return None;
        }
        GroundTerm::try_from(term).ok()
    };
    let mut stack = vec![NodeMut::Pattern(&mut pattern)];
    while let Some(node) = stack.pop() {
        match node {
            NodeMut::Pattern(p) => {
                match p {
                    GraphPattern::Bgp { patterns } => {
                        for t in patterns {
                            sent_term(&mut t.subject, &ground);
                            sent_term(&mut t.object, &ground);
                        }
                    }
                    GraphPattern::Path {
                        subject, object, ..
                    } => {
                        sent_term(subject, &ground);
                        sent_term(object, &ground);
                    }
                    _ => {}
                }
                stack.extend(p.children_mut());
            }
            NodeMut::Expression(e) => {
                if let Expression::NamedNode(n) = e
                    && let Some(term) = ground(n)
                {
                    *e = sent_expression(term);
                }
                stack.extend(e.children_mut());
            }
        }
    }
    printable_names(pattern)
}

fn printable_names(mut pattern: GraphPattern) -> (GraphPattern, Sent) {
    // First what the pattern names, then the renames: a new name is clear of every one.
    let mut made: HashSet<Variable> = HashSet::new();
    let mut constants = false;
    let mut taken: HashSet<String> = HashSet::new();
    walk_mut(NodeMut::Pattern(&mut pattern), &mut |slot| match slot {
        SlotMut::Variable(v) if is_variable(v) => {
            made.insert(v.clone());
        }
        SlotMut::Variable(v) => {
            taken.insert(v.as_str().to_owned());
        }
        SlotMut::Iri(n) => constants |= is_constant(n.as_str()),
        SlotMut::Blank(_) => {}
    });
    if constants {
        unmatchable(&mut pattern);
    }
    let mut names: HashMap<Variable, Variable> = HashMap::new();
    if !made.is_empty() || constants {
        let mut next = 0usize;
        let mut rename = |v: &Variable| -> Variable {
            names
                .entry(v.clone())
                .or_insert_with(|| {
                    loop {
                        let name = format!("_m{next}");
                        next += 1;
                        if !taken.contains(&name) {
                            break Variable::new_unchecked(name);
                        }
                    }
                })
                .clone()
        };
        walk_mut(NodeMut::Pattern(&mut pattern), &mut |slot| match slot {
            SlotMut::Variable(v) if is_variable(v) => {
                let sent = rename(v);
                *v = sent;
            }
            SlotMut::Blank(t) => {
                if let TermPattern::BlankNode(b) = t {
                    let v = blank_variable(b);
                    if made.contains(&v) {
                        *t = TermPattern::Variable(rename(&v));
                    }
                }
            }
            SlotMut::Iri(n) if is_constant(n.as_str()) => {
                let key = n.as_str().trim_start().replace(' ', "-");
                *n = NamedNode::new_unchecked(format!("{SENT_CONSTANT}{key}"));
            }
            SlotMut::Variable(_) | SlotMut::Iri(_) => {}
        });
    }
    let local = names.into_iter().map(|(made, sent)| (sent, made)).collect();
    (pattern, Sent(local))
}

fn sent_term(term: &mut TermPattern, ground: &impl Fn(&NamedNode) -> Option<GroundTerm>) {
    match term {
        TermPattern::NamedNode(n) => {
            if let Some(value) = ground(n) {
                *term = value.into();
            }
        }
        TermPattern::Triple(t) => {
            sent_term(&mut t.subject, ground);
            sent_term(&mut t.object, ground);
        }
        _ => {}
    }
}

fn sent_expression(term: GroundTerm) -> Expression {
    match term {
        GroundTerm::NamedNode(n) => Expression::NamedNode(n),
        GroundTerm::Literal(l) => Expression::Literal(l),
        GroundTerm::Triple(t) => Expression::FunctionCall(
            Function::Triple,
            vec![
                Expression::NamedNode(t.subject),
                Expression::NamedNode(t.predicate),
                sent_expression(t.object),
            ],
        ),
    }
}

/// Each basic graph pattern, path and GRAPH of `pattern` that would match a made constant
/// itself, wrapped in `FILTER(false)`: it matches nothing, because no endpoint holds the
/// local term a made constant stands for (and [`no_term`] stands for none), and its
/// variables stay in scope, so a join, OPTIONAL or UNION around it reads as here. An
/// expression's made constant is sent as it is spelt (an IRI no endpoint is meant to hold).
fn unmatchable(pattern: &mut GraphPattern) {
    let mut stack = vec![NodeMut::Pattern(pattern)];
    while let Some(node) = stack.pop() {
        match node {
            NodeMut::Pattern(p) => {
                if matches!(
                    p,
                    GraphPattern::Bgp { .. }
                        | GraphPattern::Path { .. }
                        | GraphPattern::Graph { .. }
                ) {
                    let mut holds = false;
                    pattern_slots_mut(p, &mut |slot| {
                        if let SlotMut::Iri(n) = slot {
                            holds |= is_constant(n.as_str());
                        }
                    });
                    if holds {
                        let matched = std::mem::replace(
                            p,
                            GraphPattern::Bgp {
                                patterns: Vec::new(),
                            },
                        );
                        *p = GraphPattern::Filter {
                            expr: Expression::Literal(nrese_rdf::Literal::from(false)),
                            inner: Box::new(matched),
                        };
                        // Nothing in it can match either.
                        continue;
                    }
                }
                stack.extend(p.children_mut());
            }
            NodeMut::Expression(e) => stack.extend(e.children_mut()),
        }
    }
}

/// The made variables a [`printable`] pattern sent under other names.
#[derive(Debug, Default)]
pub struct Sent(HashMap<Variable, Variable>);

impl Sent {
    /// The variable an answer's column `sent` is here: the made one it was sent for, or
    /// itself.
    pub fn local(&self, sent: Variable) -> Variable {
        match self.0.get(&sent) {
            Some(made) => made.clone(),
            None => sent,
        }
    }
}

/// A place in the algebra that holds a variable, an IRI, or a pattern's blank node (the
/// whole term, to write a variable there).
enum SlotMut<'a> {
    Variable(&'a mut Variable),
    Iri(&'a mut NamedNode),
    Blank(&'a mut TermPattern),
}

/// Every variable and IRI of `node` and the nodes in it ([`NodeMut`]'s walk reaches every
/// pattern and expression), with a stack of its own.
fn walk_mut(node: NodeMut<'_>, f: &mut impl FnMut(SlotMut<'_>)) {
    let mut stack = vec![node];
    while let Some(node) = stack.pop() {
        match node {
            NodeMut::Pattern(p) => {
                pattern_slots_mut(p, f);
                stack.extend(p.children_mut());
            }
            NodeMut::Expression(e) => {
                match e {
                    Expression::Variable(v) | Expression::Bound(v) => f(SlotMut::Variable(v)),
                    Expression::NamedNode(n) => f(SlotMut::Iri(n)),
                    _ => {}
                }
                stack.extend(e.children_mut());
            }
        }
    }
}

/// The variables and IRIs a pattern holds itself (not those of its sub-patterns and
/// expressions, which [`GraphPattern::children_mut`] gives).
fn pattern_slots_mut(p: &mut GraphPattern, f: &mut impl FnMut(SlotMut<'_>)) {
    match p {
        GraphPattern::Bgp { patterns } => patterns.iter_mut().for_each(|t| triple_mut(t, f)),
        GraphPattern::Path {
            subject, object, ..
        } => {
            term_mut(subject, f);
            term_mut(object, f);
        }
        GraphPattern::Extend { bindings, .. } => {
            bindings
                .iter_mut()
                .for_each(|(v, _)| f(SlotMut::Variable(v)));
        }
        GraphPattern::Graph { name, .. } | GraphPattern::Service { name, .. } => named_mut(name, f),
        GraphPattern::Project { variables, .. } => {
            variables.iter_mut().for_each(|v| f(SlotMut::Variable(v)));
        }
        GraphPattern::Values {
            variables,
            bindings,
        } => {
            variables.iter_mut().for_each(|v| f(SlotMut::Variable(v)));
            bindings
                .iter_mut()
                .flatten()
                .flatten()
                .for_each(|t| ground_mut(t, f));
        }
        GraphPattern::Group {
            variables,
            aggregates,
            ..
        } => {
            variables.iter_mut().for_each(|v| f(SlotMut::Variable(v)));
            aggregates
                .iter_mut()
                .for_each(|(v, _)| f(SlotMut::Variable(v)));
        }
        GraphPattern::Join { .. }
        | GraphPattern::LeftJoin { .. }
        | GraphPattern::Lateral { .. }
        | GraphPattern::Filter { .. }
        | GraphPattern::Union { .. }
        | GraphPattern::Minus { .. }
        | GraphPattern::OrderBy { .. }
        | GraphPattern::Distinct { .. }
        | GraphPattern::Reduced { .. }
        | GraphPattern::Slice { .. } => {}
    }
}

fn triple_mut(t: &mut TriplePattern, f: &mut impl FnMut(SlotMut<'_>)) {
    term_mut(&mut t.subject, f);
    named_mut(&mut t.predicate, f);
    term_mut(&mut t.object, f);
}

fn term_mut(t: &mut TermPattern, f: &mut impl FnMut(SlotMut<'_>)) {
    if matches!(t, TermPattern::BlankNode(_)) {
        return f(SlotMut::Blank(t));
    }
    match t {
        TermPattern::Variable(v) => f(SlotMut::Variable(v)),
        TermPattern::NamedNode(n) => f(SlotMut::Iri(n)),
        TermPattern::Triple(t) => triple_mut(t, f),
        TermPattern::BlankNode(_) | TermPattern::Literal(_) => {}
    }
}

fn named_mut(n: &mut NamedNodePattern, f: &mut impl FnMut(SlotMut<'_>)) {
    match n {
        NamedNodePattern::Variable(v) => f(SlotMut::Variable(v)),
        NamedNodePattern::NamedNode(n) => f(SlotMut::Iri(n)),
    }
}

fn ground_mut(t: &mut GroundTerm, f: &mut impl FnMut(SlotMut<'_>)) {
    match t {
        GroundTerm::NamedNode(n) => f(SlotMut::Iri(n)),
        GroundTerm::Triple(t) => {
            f(SlotMut::Iri(&mut t.subject));
            f(SlotMut::Iri(&mut t.predicate));
            ground_mut(&mut t.object, f);
        }
        GroundTerm::Literal(_) => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Query, SparqlParser};

    /// Nothing that comes in as a variable name or an IRI can be one of these: the checks
    /// every outside name passes refuse them all.
    #[test]
    fn nothing_from_outside_is_spelt_like_a_made_name() {
        for v in [
            variable("exists", 0),
            variable("triple-term", 12),
            blank_variable(&BlankNode::new_unchecked("x")),
        ] {
            assert!(is_variable(&v));
            assert!(Variable::new(v.as_str()).is_err(), "{v}");
            let text = format!("SELECT * WHERE {{ {v} <urn:p> ?o }}");
            assert!(SparqlParser::new().parse_query(&text).is_err(), "{text}");
        }
        for n in [blank_constant("b7"), class_constant(7), no_term()] {
            assert!(is_constant(n.as_str()));
            assert!(NamedNode::new(n.as_str()).is_err(), "{}", n.as_str());
        }
        assert!(!is_variable(&Variable::new("_bnode_x").unwrap()));
        assert!(!is_constant("urn:nrese:substituted-blank-node:x"));
    }

    fn pattern(text: &str) -> GraphPattern {
        let Query::Select { pattern, .. } = SparqlParser::new().parse_query(text).unwrap() else {
            unreachable!("a SELECT")
        };
        pattern
    }

    #[test]
    fn a_pattern_without_made_names_is_printed_as_it_is() {
        let p = pattern("SELECT * WHERE { ?s <urn:p> ?_m0 }");
        let (printed, sent) = printable(p.clone());
        assert_eq!(printed, p);
        let own = Variable::new_unchecked("_m0");
        assert_eq!(sent.local(own.clone()), own);
    }

    /// Two made variables and a made constant beside the query's own `?_m0`: each made one
    /// prints as a variable the text parses back, apart from the query's and each other,
    /// and the answer's columns read back as the made ones.
    #[test]
    fn made_names_print_apart_from_the_pattern_s_own() {
        let p = GraphPattern::Bgp {
            patterns: vec![
                TriplePattern {
                    subject: blank_variable(&BlankNode::new_unchecked("x")).into(),
                    predicate: NamedNode::new_unchecked("urn:p").into(),
                    object: Variable::new_unchecked("_m0").into(),
                },
                TriplePattern {
                    subject: variable("triple-term", 0).into(),
                    predicate: NamedNode::new_unchecked("urn:q").into(),
                    object: NamedNode::new_unchecked("urn:o").into(),
                },
            ],
        };
        let (printed, sent) = printable(p);
        let text = format!("SELECT * WHERE {{ {printed} }}");
        // `SELECT *` reads back as the projection of the pattern.
        let GraphPattern::Project { inner, .. } = pattern(&text) else {
            panic!("{text}")
        };
        let GraphPattern::Bgp { patterns } = *inner else {
            panic!("{text}")
        };
        let names: Vec<&str> = [
            &patterns[0].subject,
            &patterns[0].object,
            &patterns[1].subject,
        ]
        .iter()
        .map(|t| match t {
            TermPattern::Variable(v) => v.as_str(),
            other => panic!("{other} in {text}"),
        })
        .collect();
        assert_eq!(names[1], "_m0", "{text}");
        assert_ne!(names[0], names[1], "{text}");
        assert_ne!(names[0], names[2], "{text}");
        assert_ne!(names[1], names[2], "{text}");
        let back = |name: &str| sent.local(Variable::new_unchecked(name));
        assert_eq!(
            back(names[0]),
            blank_variable(&BlankNode::new_unchecked("x"))
        );
        assert_eq!(back(names[1]), Variable::new_unchecked("_m0"));
        assert_eq!(back(names[2]), variable("triple-term", 0));
    }

    /// A triple pattern matching a made constant matches nothing at the endpoint: its
    /// pattern is sent under `FILTER(false)`, its variables in scope, and the constant in
    /// a spelling the text parses.
    #[test]
    fn a_pattern_matching_a_made_constant_is_sent_matching_nothing() {
        for constant in [blank_constant("b7"), no_term()] {
            let p = GraphPattern::Bgp {
                patterns: vec![TriplePattern {
                    subject: Variable::new_unchecked("s").into(),
                    predicate: NamedNode::new_unchecked("urn:p").into(),
                    object: constant.clone().into(),
                }],
            };
            let (printed, _) = printable(p);
            let text = format!("SELECT * WHERE {{ {printed} }}");
            let GraphPattern::Project { inner, variables } = pattern(&text) else {
                panic!("{text}")
            };
            assert_eq!(variables, [Variable::new_unchecked("s")], "{text}");
            let GraphPattern::Filter { expr, inner } = *inner else {
                panic!("{text}")
            };
            assert_eq!(
                expr,
                Expression::Literal(nrese_rdf::Literal::from(false)),
                "{text}"
            );
            assert!(matches!(*inner, GraphPattern::Bgp { .. }), "{text}");
        }
    }
}
