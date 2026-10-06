//! The QL rewriting of a query's basic graph patterns (docs/design/ql-rewriting.md):
//! which of their variables are existential, each pattern as a conjunctive query for
//! `nrese_owl::ql`, and its rewriting back as algebra.
//!
//! Where it applies: patterns reached through joins, `OPTIONAL`, `UNION`, filters, `BIND`,
//! projections, `DISTINCT`, slices, ordering, grouping and `LATERAL`; not inside `GRAPH`,
//! `SERVICE`, `MINUS`'s right side or `EXISTS`.

use std::collections::HashSet;

use nrese_engine::{Snapshot, TermId};
use nrese_owl::ql::{Atom, Branch, Cq, Limits, Outcome, Part, QTerm, Tbox, rewrite};
use nrese_rdf::{NamedNode, NamedNodeRef, Term, Variable};
use nrese_sparql_syntax::algebra::{Expression, GraphPattern};
use nrese_sparql_syntax::term::{NamedNodePattern, TermPattern, TriplePattern};
use nrese_sparql_syntax::visit::Node;

use crate::ql::QlReport;

const RDF_TYPE: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#type";

/// The variables the context of a pattern reads: all of them, or these.
#[derive(Clone)]
enum Needed {
    All,
    Only(HashSet<Variable>),
}

impl Needed {
    fn contains(&self, v: &Variable) -> bool {
        match self {
            Self::All => true,
            Self::Only(set) => set.contains(v),
        }
    }

    fn with(&self, more: impl IntoIterator<Item = Variable>) -> Self {
        match self {
            Self::All => Self::All,
            Self::Only(set) => {
                let mut set = set.clone();
                set.extend(more);
                Self::Only(set)
            }
        }
    }
}

/// Every variable `pattern` mentions.
fn mentioned(pattern: &GraphPattern) -> Vec<Variable> {
    let mut out = Vec::new();
    pattern.on_used_variable(&mut |v| out.push(v.clone()));
    out
}

fn used(expression: &Expression) -> Vec<Variable> {
    let mut out = Vec::new();
    expression.on_used_variable(&mut |v| out.push(v.clone()));
    out
}

/// The query pattern rewritten. `all_needed`: the query returns every variable it binds
/// (SELECT's projection is in the pattern; CONSTRUCT and DESCRIBE read what they read);
/// `set`: its answer is a set (ASK, CONSTRUCT, DESCRIBE).
pub(crate) fn rewrite_query(
    pattern: &GraphPattern,
    tbox: &Tbox,
    snapshot: &Snapshot,
    limits: &Limits,
    needed: Option<Vec<Variable>>,
    set: bool,
) -> (GraphPattern, QlReport) {
    let mut rewriter = Rewriter {
        tbox,
        snapshot,
        limits,
        rdf_type: snapshot
            .lookup(NamedNodeRef::new_unchecked(RDF_TYPE).into())
            .map(TermId::raw),
        report: QlReport::default(),
    };
    let needed = match needed {
        None => Needed::All,
        Some(vars) => Needed::Only(vars.into_iter().collect()),
    };
    let out = rewriter.walk(pattern, &needed, set);
    (out, rewriter.report)
}

struct Rewriter<'a> {
    tbox: &'a Tbox,
    snapshot: &'a Snapshot,
    limits: &'a Limits,
    rdf_type: Option<u64>,
    report: QlReport,
}

impl Rewriter<'_> {
    fn walk(&mut self, pattern: &GraphPattern, needed: &Needed, set: bool) -> GraphPattern {
        use GraphPattern as P;
        let boxed = |p: GraphPattern| Box::new(p);
        // What the rewriting doesn't enter here may miss answers (design §7).
        for node in pattern.children() {
            if matches!(node, Node::Expression(_)) {
                self.unread(node, "EXISTS");
            }
        }
        match pattern {
            P::Minus { right, .. } => self.unread(Node::Pattern(right), "MINUS"),
            P::Graph { .. } => self.unread(Node::Pattern(pattern), "GRAPH"),
            P::Service { .. } => self.unread(Node::Pattern(pattern), "SERVICE"),
            P::Path { .. } => self.unread(Node::Pattern(pattern), "a property path"),
            _ => {}
        }
        match pattern {
            P::Bgp { patterns } => self.bgp(patterns, needed, set),
            P::Join { left, right } => P::Join {
                left: boxed(self.walk(left, &needed.with(mentioned(right)), set)),
                right: boxed(self.walk(right, &needed.with(mentioned(left)), set)),
            },
            P::LeftJoin {
                left,
                right,
                expression,
            } => {
                let condition: Vec<Variable> = expression.iter().flat_map(used).collect();
                P::LeftJoin {
                    left: boxed(self.walk(
                        left,
                        &needed.with(mentioned(right)).with(condition.clone()),
                        set,
                    )),
                    right: boxed(self.walk(
                        right,
                        &needed.with(mentioned(left)).with(condition),
                        set,
                    )),
                    expression: expression.clone(),
                }
            }
            P::Lateral { left, right } => P::Lateral {
                left: boxed(self.walk(left, &needed.with(mentioned(right)), set)),
                right: boxed(self.walk(right, &needed.with(mentioned(left)), set)),
            },
            P::Filter { expr, inner } => P::Filter {
                expr: expr.clone(),
                inner: boxed(self.walk(inner, &needed.with(used(expr)), set)),
            },
            P::Union { left, right } => P::Union {
                left: boxed(self.walk(left, needed, set)),
                right: boxed(self.walk(right, needed, set)),
            },
            P::Extend {
                inner,
                variable,
                expression,
            } => P::Extend {
                inner: boxed(self.walk(inner, &needed.with(used(expression)), set)),
                variable: variable.clone(),
                expression: expression.clone(),
            },
            P::Minus { left, right } => P::Minus {
                left: boxed(self.walk(left, &needed.with(mentioned(right)), set)),
                right: right.clone(),
            },
            P::OrderBy { inner, expression } => {
                let keys: Vec<Variable> = expression
                    .iter()
                    .flat_map(|e| used(e.expression()))
                    .collect();
                P::OrderBy {
                    inner: boxed(self.walk(inner, &needed.with(keys), set)),
                    expression: expression.clone(),
                }
            }
            P::Project { inner, variables } => {
                let kept = variables
                    .iter()
                    .filter(|v| needed.contains(v))
                    .cloned()
                    .collect();
                P::Project {
                    inner: boxed(self.walk(inner, &Needed::Only(kept), set)),
                    variables: variables.clone(),
                }
            }
            P::Distinct { inner } => P::Distinct {
                inner: boxed(self.walk(inner, needed, true)),
            },
            P::Reduced { inner } => P::Reduced {
                inner: boxed(self.walk(inner, needed, true)),
            },
            P::Slice {
                inner,
                start,
                length,
            } => P::Slice {
                inner: boxed(self.walk(inner, needed, set)),
                start: *start,
                length: *length,
            },
            P::Group {
                inner,
                variables,
                aggregates,
            } => {
                use nrese_sparql_syntax::algebra::AggregateExpression;
                let mut read: Vec<Variable> = variables.clone();
                for (_, aggregate) in aggregates {
                    match aggregate {
                        AggregateExpression::FunctionCall { expr, .. } => read.extend(used(expr)),
                        // `COUNT(*)` counts solutions: it reads every variable of them.
                        AggregateExpression::CountSolutions { .. } => {
                            inner.on_in_scope_variable(|v| read.push(v.clone()));
                        }
                    }
                }
                // Groups count rows: the bag form.
                P::Group {
                    inner: boxed(self.walk(
                        inner,
                        &Needed::Only(read.into_iter().collect()),
                        false,
                    )),
                    variables: variables.clone(),
                    aggregates: aggregates.clone(),
                }
            }
            P::Path { .. } | P::Graph { .. } | P::Service { .. } | P::Values { .. } => {
                pattern.clone()
            }
        }
    }

    /// A basic graph pattern, rewritten if the QL part of the schema adds to it.
    fn bgp(&mut self, patterns: &[TriplePattern], needed: &Needed, set: bool) -> GraphPattern {
        let original = GraphPattern::Bgp {
            patterns: patterns.to_vec(),
        };
        let (cq, terms) = self.query(patterns, needed);
        for (term, hazard) in self.tbox.concerns(&cq) {
            let name = |t: u64| self.name(t);
            self.report.completeness.incomplete(
                "ql",
                format!(
                    "answers on {} through anonymous individuals may be missing: {}",
                    name(term),
                    hazard.describe(&name)
                ),
            );
        }
        if patterns.iter().any(open) {
            self.report.completeness.incomplete(
                "ql",
                "a variable predicate or class: the rewriting reads constant ones only".to_owned(),
            );
        }
        let rewriting = match rewrite(self.tbox, &cq, self.limits) {
            Outcome::Unchanged => return original,
            Outcome::Exceeded(what) => {
                self.report.limits.push(what);
                self.report.completeness.incomplete("ql", format!(
                    "a basic graph pattern reached the rewriting's bound on {what} and ran as written"
                ));
                return original;
            }
            Outcome::Rewritten(r) => r,
        };
        self.report.patterns += 1;
        self.report.witnesses += rewriting.witnesses;
        self.report.branches += rewriting.branches.len();
        self.report.atoms += rewriting.size();
        // Names the new variables of each rewritten pattern apart.
        let names = Names {
            terms: &terms,
            pattern: self.report.patterns,
        };
        let union = rewriting
            .branches
            .iter()
            .map(|b| self.branch(b, patterns, &names, &cq))
            .reduce(|left, right| GraphPattern::Union {
                left: Box::new(left),
                right: Box::new(right),
            })
            .unwrap_or_else(|| original.clone());
        if set {
            return union;
        }
        // Bags: the materialised rows as they are, and once each answer only the
        // rewriting finds (design §3).
        let visible: Vec<Variable> = terms
            .iter()
            .enumerate()
            .filter(|&(v, _)| !cq.existential[v])
            .filter_map(|(_, t)| match t {
                TermPattern::Variable(v) => Some(v.clone()),
                _ => None,
            })
            .collect();
        let added = GraphPattern::Filter {
            expr: Expression::Not(Box::new(Expression::Exists(Box::new(original.clone())))),
            inner: Box::new(GraphPattern::Distinct {
                inner: Box::new(GraphPattern::Project {
                    inner: Box::new(union),
                    variables: visible,
                }),
            }),
        };
        GraphPattern::Union {
            left: Box::new(original),
            right: Box::new(added),
        }
    }

    /// A term as a reason names it.
    fn name(&self, t: u64) -> String {
        match self.snapshot.decode(TermId::from_raw(t)) {
            Some(term) => term.to_string(),
            None => format!("term {t}"),
        }
    }

    /// Notes that patterns under `node` (`place` says where) run without the rewriting,
    /// if they read a term it would change, or a variable predicate or class.
    fn unread(&mut self, node: Node<'_>, place: &str) {
        let tbox = self.tbox;
        let snapshot = self.snapshot;
        let rewritten = |n: &NamedNode| {
            snapshot
                .lookup(n.as_ref().into())
                .is_some_and(|id| tbox.rewrites_term(id.raw()))
        };
        let mut hit: Option<String> = None;
        let mut look = |n: Node<'_>| {
            match n {
                Node::Pattern(GraphPattern::Bgp { patterns }) => {
                    for t in patterns {
                        if open(t) {
                            hit.get_or_insert_with(|| "a variable predicate or class".to_owned());
                        }
                        let mut check = |n: &NamedNode| {
                            if rewritten(n) {
                                hit.get_or_insert_with(|| n.to_string());
                            }
                        };
                        if let NamedNodePattern::NamedNode(p) = &t.predicate {
                            check(p);
                            if p.as_str() == RDF_TYPE
                                && let TermPattern::NamedNode(c) = &t.object
                            {
                                check(c);
                            }
                        }
                    }
                }
                Node::Pattern(GraphPattern::Path { path, .. }) => {
                    let mut names = Vec::new();
                    match path_names(path, &mut names) {
                        None => {
                            hit.get_or_insert_with(|| "a negated property set".to_owned());
                        }
                        Some(()) => {
                            if let Some(n) = names.into_iter().find(|n| rewritten(n)) {
                                hit.get_or_insert_with(|| n.to_string());
                            }
                        }
                    }
                }
                _ => {}
            }
            false
        };
        match node {
            Node::Pattern(p) => p.find(&mut look),
            Node::Expression(e) => e.find(&mut look),
        };
        if let Some(what) = hit {
            self.report.completeness.incomplete(
                "ql",
                format!(
                    "a pattern inside {place} runs without the rewriting, which changes {what}"
                ),
            );
        }
    }

    /// The pattern as a conjunctive query, and the term each of its variables stands for.
    fn query(&self, patterns: &[TriplePattern], needed: &Needed) -> (Cq, Vec<TermPattern>) {
        let mut terms: Vec<TermPattern> = Vec::new();
        let var = |t: &TermPattern, terms: &mut Vec<TermPattern>| -> u32 {
            match terms.iter().position(|x| x == t) {
                Some(at) => at as u32,
                None => {
                    terms.push(t.clone());
                    (terms.len() - 1) as u32
                }
            }
        };
        let mut atoms = Vec::new();
        for (n, triple) in patterns.iter().enumerate() {
            let term = |t: &TermPattern, terms: &mut Vec<TermPattern>| -> Option<QTerm> {
                match t {
                    TermPattern::Variable(_) | TermPattern::BlankNode(_) => {
                        Some(QTerm::Var(var(t, terms)))
                    }
                    TermPattern::NamedNode(n) => self
                        .snapshot
                        .lookup(n.as_ref().into())
                        .map(|id| QTerm::Const(id.raw())),
                    TermPattern::Literal(l) => self
                        .snapshot
                        .lookup(l.as_ref().into())
                        .map(|id| QTerm::Const(id.raw())),
                    TermPattern::Triple(_) => None,
                }
            };
            let s = term(&triple.subject, &mut terms);
            let o = term(&triple.object, &mut terms);
            let predicate = match &triple.predicate {
                NamedNodePattern::NamedNode(p) => {
                    self.snapshot.lookup(p.as_ref().into()).map(TermId::raw)
                }
                NamedNodePattern::Variable(_) => None,
            };
            let atom = match (s, predicate, o) {
                (Some(s), Some(p), Some(QTerm::Const(class))) if Some(p) == self.rdf_type => {
                    Atom::Class(s, class)
                }
                (Some(s), Some(p), Some(o)) if Some(p) != self.rdf_type => Atom::Role(s, p, o),
                _ => {
                    let mut vars = Vec::new();
                    for t in [&triple.subject, &triple.object] {
                        if matches!(t, TermPattern::Variable(_) | TermPattern::BlankNode(_)) {
                            vars.push(var(t, &mut terms));
                        }
                    }
                    if let NamedNodePattern::Variable(v) = &triple.predicate {
                        vars.push(var(&TermPattern::Variable(v.clone()), &mut terms));
                    }
                    for t in [&triple.subject, &triple.object] {
                        if let TermPattern::Triple(inner) = t {
                            let mut inside = Vec::new();
                            GraphPattern::Bgp {
                                patterns: vec![(**inner).clone()],
                            }
                            .on_in_scope_variable(|v| inside.push(v.clone()));
                            for v in inside {
                                vars.push(var(&TermPattern::Variable(v), &mut terms));
                            }
                        }
                    }
                    Atom::Other(n, vars)
                }
            };
            atoms.push(atom);
        }
        let existential = terms
            .iter()
            .map(|t| match t {
                TermPattern::BlankNode(_) => true,
                TermPattern::Variable(v) => !needed.contains(v),
                _ => false,
            })
            .collect();
        let vars = terms.len() as u32;
        (
            Cq {
                atoms,
                vars,
                existential,
            },
            terms,
        )
    }

    /// One branch of the rewriting as algebra.
    fn branch(
        &self,
        branch: &Branch,
        patterns: &[TriplePattern],
        names: &Names<'_>,
        cq: &Cq,
    ) -> GraphPattern {
        let merged = |v: u32| branch.merged.iter().find(|(m, _)| *m == v).map(|(_, t)| *t);
        let mut triples = Vec::new();
        let mut unions = Vec::new();
        for part in &branch.parts {
            match part {
                Part::Atom(Atom::Other(n, _)) => {
                    // Kept as written, with the variables this branch made one replaced.
                    let replace = |t: &TermPattern| -> TermPattern {
                        match names.terms.iter().position(|x| x == t) {
                            Some(v) => match merged(v as u32) {
                                Some(to) => self.term(to, names),
                                None => names.term(v as u32),
                            },
                            None => t.clone(),
                        }
                    };
                    let triple = &patterns[*n];
                    triples.push(TriplePattern {
                        subject: replace(&triple.subject),
                        predicate: match &triple.predicate {
                            NamedNodePattern::Variable(v) => {
                                match replace(&TermPattern::Variable(v.clone())) {
                                    TermPattern::Variable(v) => NamedNodePattern::Variable(v),
                                    TermPattern::NamedNode(n) => NamedNodePattern::NamedNode(n),
                                    _ => triple.predicate.clone(),
                                }
                            }
                            p => p.clone(),
                        },
                        object: replace(&triple.object),
                    });
                }
                Part::Atom(atom) => triples.push(self.triple(atom, names)),
                Part::Any(alternatives) => {
                    let union = alternatives
                        .iter()
                        .map(|a| GraphPattern::Bgp {
                            patterns: vec![self.triple(a, names)],
                        })
                        .reduce(|left, right| GraphPattern::Union {
                            left: Box::new(left),
                            right: Box::new(right),
                        })
                        .expect("an alternative");
                    unions.push(union);
                }
            }
        }
        let mut pattern = GraphPattern::Bgp { patterns: triples };
        for union in unions {
            pattern = GraphPattern::Join {
                left: Box::new(pattern),
                right: Box::new(union),
            };
        }
        // The variables the context reads that this branch made one with another term.
        for &(v, to) in &branch.merged {
            if cq.existential[v as usize] {
                continue;
            }
            let TermPattern::Variable(variable) = &names.terms[v as usize] else {
                continue;
            };
            let expression = match self.term(to, names) {
                TermPattern::Variable(w) => Expression::Variable(w),
                TermPattern::NamedNode(n) => Expression::NamedNode(n),
                TermPattern::Literal(l) => Expression::Literal(l),
                _ => continue,
            };
            pattern = GraphPattern::Extend {
                inner: Box::new(pattern),
                variable: variable.clone(),
                expression,
            };
        }
        pattern
    }

    fn triple(&self, atom: &Atom, names: &Names<'_>) -> TriplePattern {
        match atom {
            Atom::Class(t, class) => TriplePattern {
                subject: self.term(*t, names),
                predicate: NamedNodePattern::NamedNode(NamedNode::new_unchecked(RDF_TYPE)),
                object: self.term(QTerm::Const(*class), names),
            },
            Atom::Role(s, p, o) => TriplePattern {
                subject: self.term(*s, names),
                predicate: match self.snapshot.decode(TermId::from_raw(*p)) {
                    Some(Term::NamedNode(n)) => NamedNodePattern::NamedNode(n),
                    _ => unreachable!("properties are IRIs the store knows"),
                },
                object: self.term(*o, names),
            },
            Atom::Other(..) => unreachable!("kept as written"),
        }
    }

    fn term(&self, t: QTerm, names: &Names<'_>) -> TermPattern {
        match t {
            QTerm::Var(v) => names.term(v),
            QTerm::Const(id) => match self.snapshot.decode(TermId::from_raw(id)) {
                Some(Term::NamedNode(n)) => TermPattern::NamedNode(n),
                Some(Term::Literal(l)) => TermPattern::Literal(l),
                Some(Term::BlankNode(b)) => TermPattern::BlankNode(b),
                _ => unreachable!("constants are terms the store knows"),
            },
        }
    }
}

/// Whether a triple pattern has a variable predicate, or a variable class under
/// `rdf:type`: atoms the rewriting doesn't read.
fn open(t: &TriplePattern) -> bool {
    match &t.predicate {
        NamedNodePattern::Variable(_) => true,
        NamedNodePattern::NamedNode(p) => {
            p.as_str() == RDF_TYPE
                && matches!(
                    t.object,
                    TermPattern::Variable(_) | TermPattern::BlankNode(_)
                )
        }
    }
}

/// The properties of a path; `None` for a negated property set (any other property).
fn path_names(
    path: &nrese_sparql_syntax::algebra::PropertyPathExpression,
    out: &mut Vec<NamedNode>,
) -> Option<()> {
    use nrese_sparql_syntax::algebra::PropertyPathExpression as E;
    match path {
        E::NamedNode(n) => out.push(n.clone()),
        E::Reverse(p) | E::ZeroOrMore(p) | E::OneOrMore(p) | E::ZeroOrOne(p) => {
            path_names(p, out)?;
        }
        E::Sequence(a, b) | E::Alternative(a, b) => {
            path_names(a, out)?;
            path_names(b, out)?;
        }
        E::NegatedPropertySet(_) => return None,
    }
    Some(())
}

/// How the variables of a rewritten pattern are written: the query's own, and new ones
/// (`_ql<pattern>_<n>`, also for the pattern's blank nodes, which the branches share).
struct Names<'a> {
    terms: &'a [TermPattern],
    pattern: usize,
}

impl Names<'_> {
    fn term(&self, v: u32) -> TermPattern {
        match self.terms.get(v as usize) {
            Some(TermPattern::Variable(variable)) => TermPattern::Variable(variable.clone()),
            _ => TermPattern::Variable(Variable::new_unchecked(format!("_ql{}_{v}", self.pattern))),
        }
    }
}
