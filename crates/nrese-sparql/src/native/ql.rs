//! The QL rewriting of a query's basic graph patterns (docs/design/ql-rewriting.md):
//! which of their variables are existential, each pattern as a conjunctive query for
//! `nrese_owl::ql`, and its rewriting back as algebra.
//!
//! Where it applies: patterns reached through joins, `OPTIONAL`, `UNION`, filters, `BIND`,
//! projections, `DISTINCT`, slices, ordering, grouping and `LATERAL`, inside `EXISTS` and
//! `NOT EXISTS`, on `MINUS`'s right side, and in `GRAPH` naming the default graph under
//! another store's name (`compat`). Not inside `GRAPH` over a named graph, which holds its
//! asserted statements only (the inferences are the default graph's), nor `SERVICE`.
//!
//! A pattern under a negation (`NOT EXISTS`, `MINUS`'s right side, `EXISTS` where its truth
//! value can be turned either way) is read as a set, its variables bound outside as answer
//! variables, the others existential. There an answer it misses can make an answer of the
//! query wrong: its incompleteness makes the query's answers `unsound`.

use std::collections::HashSet;

use nrese_engine::{GraphSelector, QuadPattern, Snapshot, TermId};
use nrese_owl::ObjProp;
use nrese_owl::ql::{
    Atom, Basic, Branch, Budget, Cq, Limits, Outcome, Part, Probe, QTerm, Tbox, rewrite_with,
};
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

/// Where an `EXISTS` stands: under no negation, under one, or where its truth value can go
/// either way (a comparison, `IF`'s condition, a function's argument, a bound value).
#[derive(Clone, Copy, PartialEq, Eq)]
enum Polarity {
    Positive,
    Negative,
    Both,
}

impl Polarity {
    fn not(self) -> Self {
        match self {
            Self::Positive => Self::Negative,
            Self::Negative => Self::Positive,
            Self::Both => Self::Both,
        }
    }
}

/// The solutions an expression is evaluated on: the variables they may bind, and those
/// some of them leave unbound.
struct Outer {
    bound: HashSet<Variable>,
    maybe: HashSet<Variable>,
}

impl Outer {
    /// The solutions of `patterns` joined.
    fn of(patterns: &[&GraphPattern]) -> Self {
        let mut bound = HashSet::new();
        let mut certain = HashSet::new();
        for pattern in patterns {
            pattern.on_in_scope_variable(|v| {
                bound.insert(v.clone());
            });
            certain.extend(super::pushdown::certain(pattern));
        }
        let maybe = bound.difference(&certain).cloned().collect();
        Self { bound, maybe }
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

/// What a rewriting may ask the data (design §3): the store's rewriting, for its cache,
/// and the options of the query, to read what it reads.
pub(crate) struct DataCheck<'a> {
    pub ql: &'a crate::ql::QlRewriting,
    pub options: crate::QueryOptions,
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
    data: Option<&DataCheck<'_>>,
) -> (GraphPattern, QlReport) {
    let mut rewriter = Rewriter {
        tbox,
        snapshot,
        limits,
        budget: Budget::new(limits.query_work),
        data,
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
    /// The steps the query's patterns have left together.
    budget: Budget,
    data: Option<&'a DataCheck<'a>>,
    rdf_type: Option<u64>,
    report: QlReport,
}

impl Rewriter<'_> {
    fn walk(&mut self, pattern: &GraphPattern, needed: &Needed, set: bool) -> GraphPattern {
        use GraphPattern as P;
        let boxed = |p: GraphPattern| Box::new(p);
        // What the rewriting doesn't enter here may miss answers (design §7).
        match pattern {
            P::Service { .. } => self.unread(Node::Pattern(pattern), "SERVICE"),
            P::Path { .. } => self.unread(Node::Pattern(pattern), "a property path"),
            P::Group { aggregates, .. } => {
                for (_, aggregate) in aggregates {
                    if let nrese_sparql_syntax::algebra::AggregateExpression::FunctionCall {
                        expr,
                        ..
                    } = aggregate
                    {
                        self.unread(Node::Expression(expr), "an aggregate's EXISTS");
                    }
                }
            }
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
                let outer = Outer::of(&[left, right]);
                P::LeftJoin {
                    expression: expression
                        .as_ref()
                        .map(|e| self.expression(e, &outer, Polarity::Positive)),
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
                }
            }
            P::Lateral { left, right } => P::Lateral {
                left: boxed(self.walk(left, &needed.with(mentioned(right)), set)),
                right: boxed(self.walk(right, &needed.with(mentioned(left)), set)),
            },
            P::Filter { expr, inner } => P::Filter {
                expr: self.expression(expr, &Outer::of(&[inner]), Polarity::Positive),
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
                expression: self.expression(expression, &Outer::of(&[inner]), Polarity::Both),
                inner: boxed(self.walk(inner, &needed.with(used(expression)), set)),
                variable: variable.clone(),
            },
            P::Minus { left, right } => {
                // The right side removes what it is compatible with: read as a set, the
                // variables it shares with the left as answer variables.
                let on_left = mentioned(left);
                let shared: HashSet<Variable> = mentioned(right)
                    .into_iter()
                    .filter(|v| on_left.contains(v))
                    .collect();
                let rewritten = self.negated(Polarity::Negative, "MINUS", |this| {
                    this.walk(right, &Needed::Only(shared), true)
                });
                P::Minus {
                    left: boxed(self.walk(left, &needed.with(mentioned(right)), set)),
                    right: boxed(rewritten),
                }
            }
            // Another store's name for the default graph: read as the default graph is.
            P::Graph {
                name: NamedNodePattern::NamedNode(name),
                inner,
            } if crate::compat::names_default_graph(name.as_str()) => P::Graph {
                name: NamedNodePattern::NamedNode(name.clone()),
                inner: boxed(self.walk(inner, needed, set)),
            },
            P::OrderBy { inner, expression } => {
                let keys: Vec<Variable> = expression
                    .iter()
                    .flat_map(|e| used(e.expression()))
                    .collect();
                let outer = Outer::of(&[inner]);
                let expression = expression
                    .iter()
                    .map(|key| {
                        use nrese_sparql_syntax::algebra::OrderExpression as O;
                        match key {
                            O::Asc(e) => O::Asc(self.expression(e, &outer, Polarity::Both)),
                            O::Desc(e) => O::Desc(self.expression(e, &outer, Polarity::Both)),
                        }
                    })
                    .collect();
                P::OrderBy {
                    inner: boxed(self.walk(inner, &needed.with(keys), set)),
                    expression,
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
                // Groups count rows: the bag form, unless no aggregate depends on how often
                // a row comes (`MIN`, `MAX`, `SAMPLE`, `DISTINCT` ones, none).
                let insensitive = aggregates.iter().all(|(_, aggregate)| {
                    use nrese_sparql_syntax::algebra::AggregateFunction as F;
                    match aggregate {
                        AggregateExpression::FunctionCall { name, distinct, .. } => {
                            *distinct || matches!(name, F::Min | F::Max | F::Sample)
                        }
                        AggregateExpression::CountSolutions { distinct } => *distinct,
                    }
                });
                P::Group {
                    inner: boxed(self.walk(
                        inner,
                        &Needed::Only(read.into_iter().collect()),
                        insensitive,
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

    /// `EXISTS` patterns in `expression` rewritten; `outer`: the solutions it reads.
    fn expression(
        &mut self,
        expression: &Expression,
        outer: &Outer,
        polarity: Polarity,
    ) -> Expression {
        use Expression as E;
        let go =
            |this: &mut Self, e: &Expression, p: Polarity| Box::new(this.expression(e, outer, p));
        let both = Polarity::Both;
        match expression {
            E::Exists(pattern) => E::Exists(Box::new(self.exists(pattern, outer, polarity))),
            E::Not(a) => E::Not(go(self, a, polarity.not())),
            E::And(a, b) => E::And(go(self, a, polarity), go(self, b, polarity)),
            E::Or(a, b) => E::Or(go(self, a, polarity), go(self, b, polarity)),
            E::If(a, b, c) => E::If(
                go(self, a, both),
                go(self, b, polarity),
                go(self, c, polarity),
            ),
            E::Coalesce(list) => E::Coalesce(list.iter().map(|e| *go(self, e, polarity)).collect()),
            E::Equal(a, b) => E::Equal(go(self, a, both), go(self, b, both)),
            E::SameTerm(a, b) => E::SameTerm(go(self, a, both), go(self, b, both)),
            E::Greater(a, b) => E::Greater(go(self, a, both), go(self, b, both)),
            E::GreaterOrEqual(a, b) => E::GreaterOrEqual(go(self, a, both), go(self, b, both)),
            E::Less(a, b) => E::Less(go(self, a, both), go(self, b, both)),
            E::LessOrEqual(a, b) => E::LessOrEqual(go(self, a, both), go(self, b, both)),
            E::Add(a, b) => E::Add(go(self, a, both), go(self, b, both)),
            E::Subtract(a, b) => E::Subtract(go(self, a, both), go(self, b, both)),
            E::Multiply(a, b) => E::Multiply(go(self, a, both), go(self, b, both)),
            E::Divide(a, b) => E::Divide(go(self, a, both), go(self, b, both)),
            E::UnaryPlus(a) => E::UnaryPlus(go(self, a, both)),
            E::UnaryMinus(a) => E::UnaryMinus(go(self, a, both)),
            E::In(a, list) => E::In(
                go(self, a, both),
                list.iter().map(|e| *go(self, e, both)).collect(),
            ),
            E::FunctionCall(f, list) => {
                E::FunctionCall(f.clone(), list.iter().map(|e| *go(self, e, both)).collect())
            }
            other => other.clone(),
        }
    }

    /// The pattern of an `EXISTS`, read as a set: its variables the outer solutions bind are
    /// answer variables (each solution substitutes its values), the others existential.
    fn exists(
        &mut self,
        pattern: &GraphPattern,
        outer: &Outer,
        polarity: Polarity,
    ) -> GraphPattern {
        let correlated: HashSet<Variable> = mentioned(pattern)
            .into_iter()
            .filter(|v| outer.bound.contains(v))
            .collect();
        self.negated(polarity, "a negated EXISTS", |this| {
            let before = this.report.patterns;
            let out = this.walk(pattern, &Needed::Only(correlated.clone()), true);
            // Where an outer solution leaves such a variable unbound, it is free inside and
            // could be an anonymous individual, which an answer variable can't.
            if this.report.patterns > before
                && let Some(v) = correlated.iter().find(|v| outer.maybe.contains(*v))
            {
                this.report.completeness.incomplete(
                    "ql",
                    format!(
                        "{v} may be unbound where an EXISTS reads it: its matches through \
                         anonymous individuals may be missing"
                    ),
                );
            }
            out
        })
    }

    /// Runs `f` on a pattern under `polarity`: under a negation (or either way) an answer
    /// the pattern misses can make one of the query's wrong, so its incompleteness makes
    /// the answers `unsound`.
    fn negated<T>(&mut self, polarity: Polarity, place: &str, f: impl FnOnce(&mut Self) -> T) -> T {
        if polarity == Polarity::Positive {
            return f(self);
        }
        let saved = std::mem::take(&mut self.report.completeness);
        let out = f(self);
        let inner = std::mem::replace(&mut self.report.completeness, saved);
        for reason in inner.reasons {
            self.report.completeness.unsound(
                reason.source,
                format!(
                    "under {place}, answers may be wrong as well as missing: {}",
                    reason.text
                ),
            );
        }
        out
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
        let (tbox, limits) = (self.tbox, self.limits);
        // Out of `self` while the data check borrows it.
        let budget = std::mem::replace(&mut self.budget, Budget::new(0));
        let outcome = rewrite_with(tbox, &cq, limits, &budget, &mut |probe| {
            self.realised(probe)
        });
        self.budget = budget;
        let rewriting = match outcome {
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
        let union = |this: &Self, branches: &mut dyn Iterator<Item = &Branch>| {
            branches
                .map(|b| this.branch(b, patterns, &names, &cq))
                .reduce(|left, right| GraphPattern::Union {
                    left: Box::new(left),
                    right: Box::new(right),
                })
        };
        if set {
            return union(self, &mut rewriting.branches.iter()).unwrap_or(original);
        }
        // What the bag form adds comes from the branches other than the pattern as written:
        // that one's rows are the materialised ones, all taken already.
        let Some(union) = union(
            self,
            &mut rewriting.branches.iter().filter(|b| !as_written(b, &cq)),
        ) else {
            return original;
        };
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

    /// Whether the data the query reads has the tree of `probe` at every individual of
    /// each concept it folds to (once per snapshot view and probe; `false` where the
    /// rewriting has no data to ask or can't write the question).
    fn realised(&mut self, probe: &Probe<'_>) -> bool {
        let Some(data) = self.data else {
            return false;
        };
        let term = |t: QTerm| -> Option<TermPattern> {
            Some(match t {
                QTerm::Var(v) => TermPattern::Variable(Variable::new_unchecked(format!("_qlr{v}"))),
                QTerm::Const(id) => match self.snapshot.decode(TermId::from_raw(id))? {
                    Term::NamedNode(n) => TermPattern::NamedNode(n),
                    Term::Literal(l) => TermPattern::Literal(l),
                    Term::BlankNode(b) => TermPattern::BlankNode(b),
                    _ => return None,
                },
            })
        };
        let property = |p: u64| match self.snapshot.decode(TermId::from_raw(p)) {
            Some(Term::NamedNode(n)) => Some(n),
            _ => None,
        };
        let rdf_type = || NamedNodePattern::NamedNode(NamedNode::new_unchecked(RDF_TYPE));
        let triple = |atom: &Atom| -> Option<TriplePattern> {
            Some(match atom {
                Atom::Class(t, class) => TriplePattern {
                    subject: term(*t)?,
                    predicate: rdf_type(),
                    object: TermPattern::NamedNode(property(*class)?),
                },
                Atom::Role(s, p, o) => TriplePattern {
                    subject: term(*s)?,
                    predicate: NamedNodePattern::NamedNode(property(*p)?),
                    object: term(*o)?,
                },
                Atom::Other(..) => return None,
            })
        };
        let Some(tree) = probe.atoms.iter().map(triple).collect::<Option<Vec<_>>>() else {
            return false;
        };
        let other = TermPattern::Variable(Variable::new_unchecked("_qlr_other"));
        let identity = self.snapshot.identity();
        // What a question reads, counted before it runs: each triple pattern's statements
        // with its constant predicate (and class).
        let model = data.options.read_model;
        let reads = |t: &TriplePattern| -> u64 {
            let id = |n: &NamedNode| self.snapshot.lookup(n.as_ref().into());
            let predicate = match &t.predicate {
                NamedNodePattern::NamedNode(p) => id(p),
                NamedNodePattern::Variable(_) => None,
            };
            let object = match (&t.predicate, &t.object) {
                (NamedNodePattern::NamedNode(p), TermPattern::NamedNode(c))
                    if p.as_str() == RDF_TYPE =>
                {
                    id(c)
                }
                _ => None,
            };
            self.snapshot.count_in(
                model,
                &QuadPattern {
                    subject: None,
                    predicate,
                    object,
                    graph: GraphSelector::Any,
                },
            )
        };
        let tree_reads: u64 = tree.iter().map(reads).sum();
        for &alternative in probe.alternatives {
            let Some(root) = term(probe.root) else {
                return false;
            };
            let stated = match alternative {
                Basic::Class(class) => property(class).map(|c| TriplePattern {
                    subject: root.clone(),
                    predicate: rdf_type(),
                    object: TermPattern::NamedNode(c),
                }),
                Basic::Exists(ObjProp::Named(p)) => property(p).map(|p| TriplePattern {
                    subject: root.clone(),
                    predicate: NamedNodePattern::NamedNode(p),
                    object: other.clone(),
                }),
                Basic::Exists(ObjProp::Inverse(p)) => property(p).map(|p| TriplePattern {
                    subject: other.clone(),
                    predicate: NamedNodePattern::NamedNode(p),
                    object: root.clone(),
                }),
                Basic::Thing | Basic::Fresh(_) => None,
            };
            let Some(stated) = stated else {
                return false;
            };
            if tree_reads.saturating_add(reads(&stated)) > self.limits.check_rows {
                return false;
            }
            // Is there an individual of the concept without the tree?
            let query = nrese_sparql_syntax::Query::Ask {
                dataset: None,
                pattern: GraphPattern::Filter {
                    expr: Expression::Not(Box::new(Expression::Exists(Box::new(
                        GraphPattern::Bgp {
                            patterns: tree.clone(),
                        },
                    )))),
                    inner: Box::new(GraphPattern::Bgp {
                        patterns: vec![stated],
                    }),
                },
                base_iri: None,
            };
            let key = query.to_string();
            let snapshot = self.snapshot;
            let asked = &mut self.report.checks;
            let budget = self.limits.checks;
            let has = data.ql.realised(identity, &key, || {
                if *asked >= budget {
                    return None;
                }
                *asked += 1;
                match crate::query::evaluate_query(snapshot, &query, &data.options) {
                    Ok(crate::results::QueryResults::Boolean(missing)) => Some(!missing),
                    // A cancelled or otherwise failed probe says nothing about the data.
                    // Leave it uncached so a later request can ask again.
                    _ => None,
                }
            });
            if !has {
                return false;
            }
        }
        self.report.realised += 1;
        true
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

/// Whether `branch` is `cq` as written: its atoms, nothing folded, merged or added.
fn as_written(branch: &Branch, cq: &Cq) -> bool {
    branch.merged.is_empty()
        && branch.parts.len() == cq.atoms.len()
        && branch
            .parts
            .iter()
            .all(|p| matches!(p, Part::Atom(a) if cq.atoms.contains(a)))
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
