//! nrese-sparql-syntax against spargebra: every query and update of the pinned W3C SPARQL
//! 1.0, 1.1 and 1.2 suites (and the repository's own `.rq`/`.ru` files) parsed by both, their
//! algebra compared after converting spargebra's into nrese's. Without the suites in full the
//! test fails where they are required (`NRESE_W3C_REQUIRED`) and else compares the
//! repository's files alone, saying it doesn't qualify.
//!
//! What may differ, and is normalised on the algebra (never its text) before comparing:
//! - generated names of variables and blank nodes (spargebra's are random 128-bit numbers,
//!   nrese's are numbered);
//! - `-1` in an expression: nrese reads the signed literal the grammar's tokens make,
//!   spargebra a negation of `1` computed at run time;
//! - the shape of a nest of `&&` or `||` (both associative): nrese balances it, spargebra
//!   folds it left.
//!
//! Every other difference is listed. Those explained (spargebra's right-associative
//! `-` and `/`, and what each accepts or rejects) are checked to be exactly the expected
//! ones.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

use nrese_rdf::vocab::xsd;
use nrese_rdf::{BaseDirection, BlankNode, Literal, NamedNode, Variable};
use nrese_sparql_syntax::algebra::{
    AggregateExpression as A, AggregateFunction as AF, Expression as E, Function as F,
    GraphPattern as P, GraphTarget, OrderExpression as O, PropertyPathExpression as PP,
    QueryDataset,
};
use nrese_sparql_syntax::term::{
    GraphName, GraphNamePattern, GroundQuad, GroundQuadPattern, GroundTerm, GroundTermPattern,
    GroundTriple, GroundTriplePattern, NamedNodePattern, Quad, QuadPattern, TermPattern,
    TriplePattern,
};
use nrese_sparql_syntax::visit::NodeMut;
use nrese_sparql_syntax::{GraphUpdateOperation as U, Query, SparqlParser, Update};
use oxigraph_comparison::w3c;
use spargebra::algebra as sa;
use spargebra::term as st;

/// Renames spargebra's random names (and keeps the query's own).
#[derive(Default)]
struct Names {
    map: HashMap<String, String>,
}

impl Names {
    fn name(&mut self, name: &str) -> String {
        let random = name.len() >= 16 && name.bytes().all(|b| b.is_ascii_hexdigit());
        if !random {
            return name.to_owned();
        }
        let n = self.map.len();
        self.map
            .entry(name.to_owned())
            .or_insert_with(|| format!("__x{n}"))
            .clone()
    }

    fn var(&mut self, v: &oxrdf::Variable) -> Variable {
        Variable::new_unchecked(self.name(v.as_str()))
    }

    fn bnode(&mut self, b: &oxrdf::BlankNode) -> BlankNode {
        BlankNode::new_unchecked(self.name(b.as_str()))
    }
}

fn nn(n: &oxrdf::NamedNode) -> NamedNode {
    NamedNode::new_unchecked(n.as_str())
}

fn lit(l: &oxrdf::Literal) -> Literal {
    match (l.language(), l.direction()) {
        (Some(lang), Some(d)) => Literal::new_directional_language_tagged_literal_unchecked(
            l.value(),
            lang,
            match d {
                oxrdf::BaseDirection::Ltr => BaseDirection::Ltr,
                oxrdf::BaseDirection::Rtl => BaseDirection::Rtl,
            },
        ),
        (Some(lang), None) => Literal::new_language_tagged_literal_unchecked(l.value(), lang),
        _ => Literal::new_typed_literal(l.value(), nn(&l.datatype().into_owned())),
    }
}

impl Names {
    fn ground(&mut self, t: &st::GroundTerm) -> GroundTerm {
        match t {
            st::GroundTerm::NamedNode(n) => nn(n).into(),
            st::GroundTerm::Literal(l) => lit(l).into(),
            st::GroundTerm::Triple(t) => GroundTriple {
                subject: nn(&t.subject),
                predicate: nn(&t.predicate),
                object: self.ground(&t.object),
            }
            .into(),
        }
    }

    fn nnp(&mut self, p: &st::NamedNodePattern) -> NamedNodePattern {
        match p {
            st::NamedNodePattern::NamedNode(n) => nn(n).into(),
            st::NamedNodePattern::Variable(v) => self.var(v).into(),
        }
    }

    fn tp(&mut self, t: &st::TermPattern) -> TermPattern {
        match t {
            st::TermPattern::NamedNode(n) => nn(n).into(),
            st::TermPattern::BlankNode(b) => self.bnode(b).into(),
            st::TermPattern::Literal(l) => lit(l).into(),
            st::TermPattern::Triple(t) => self.triple(t).into(),
            st::TermPattern::Variable(v) => self.var(v).into(),
        }
    }

    fn triple(&mut self, t: &st::TriplePattern) -> TriplePattern {
        TriplePattern {
            subject: self.tp(&t.subject),
            predicate: self.nnp(&t.predicate),
            object: self.tp(&t.object),
        }
    }

    fn gtp(&mut self, t: &st::GroundTermPattern) -> GroundTermPattern {
        match t {
            st::GroundTermPattern::NamedNode(n) => nn(n).into(),
            st::GroundTermPattern::Literal(l) => lit(l).into(),
            st::GroundTermPattern::Variable(v) => self.var(v).into(),
            st::GroundTermPattern::Triple(t) => {
                GroundTermPattern::Triple(Box::new(GroundTriplePattern {
                    subject: self.gtp(&t.subject),
                    predicate: self.nnp(&t.predicate),
                    object: self.gtp(&t.object),
                }))
            }
        }
    }

    fn gnp(&mut self, g: &st::GraphNamePattern) -> GraphNamePattern {
        match g {
            st::GraphNamePattern::NamedNode(n) => nn(n).into(),
            st::GraphNamePattern::DefaultGraph => GraphNamePattern::DefaultGraph,
            st::GraphNamePattern::Variable(v) => self.var(v).into(),
        }
    }

    fn path(&mut self, p: &sa::PropertyPathExpression) -> PP {
        use sa::PropertyPathExpression as S;
        match p {
            S::NamedNode(n) => PP::NamedNode(nn(n)),
            S::Reverse(p) => PP::Reverse(Box::new(self.path(p))),
            // Binary in spargebra, n-ary and flattened in NRESE (ADR-0012).
            S::Sequence(a, b) => PP::sequence(vec![self.path(a), self.path(b)]),
            S::Alternative(a, b) => PP::alternative(vec![self.path(a), self.path(b)]),
            S::ZeroOrMore(p) => PP::ZeroOrMore(Box::new(self.path(p))),
            S::OneOrMore(p) => PP::OneOrMore(Box::new(self.path(p))),
            S::ZeroOrOne(p) => PP::ZeroOrOne(Box::new(self.path(p))),
            S::NegatedPropertySet(set) => PP::NegatedPropertySet(set.iter().map(nn).collect()),
        }
    }

    fn function(&mut self, f: &sa::Function) -> F {
        use sa::Function as S;
        match f {
            S::Str => F::Str,
            S::Lang => F::Lang,
            S::LangMatches => F::LangMatches,
            S::Datatype => F::Datatype,
            S::Iri => F::Iri,
            S::BNode => F::BNode,
            S::Rand => F::Rand,
            S::Abs => F::Abs,
            S::Ceil => F::Ceil,
            S::Floor => F::Floor,
            S::Round => F::Round,
            S::Concat => F::Concat,
            S::SubStr => F::SubStr,
            S::StrLen => F::StrLen,
            S::Replace => F::Replace,
            S::UCase => F::UCase,
            S::LCase => F::LCase,
            S::EncodeForUri => F::EncodeForUri,
            S::Contains => F::Contains,
            S::StrStarts => F::StrStarts,
            S::StrEnds => F::StrEnds,
            S::StrBefore => F::StrBefore,
            S::StrAfter => F::StrAfter,
            S::Year => F::Year,
            S::Month => F::Month,
            S::Day => F::Day,
            S::Hours => F::Hours,
            S::Minutes => F::Minutes,
            S::Seconds => F::Seconds,
            S::Timezone => F::Timezone,
            S::Tz => F::Tz,
            S::Now => F::Now,
            S::Uuid => F::Uuid,
            S::StrUuid => F::StrUuid,
            S::Md5 => F::Md5,
            S::Sha1 => F::Sha1,
            S::Sha256 => F::Sha256,
            S::Sha384 => F::Sha384,
            S::Sha512 => F::Sha512,
            S::StrLang => F::StrLang,
            S::StrDt => F::StrDt,
            S::IsIri => F::IsIri,
            S::IsBlank => F::IsBlank,
            S::IsLiteral => F::IsLiteral,
            S::IsNumeric => F::IsNumeric,
            S::Regex => F::Regex,
            S::Triple => F::Triple,
            S::Subject => F::Subject,
            S::Predicate => F::Predicate,
            S::Object => F::Object,
            S::IsTriple => F::IsTriple,
            S::LangDir => F::LangDir,
            S::HasLang => F::HasLang,
            S::HasLangDir => F::HasLangDir,
            S::StrLangDir => F::StrLangDir,
            S::Adjust => F::Adjust,
            S::Custom(n) => F::Custom(nn(n)),
        }
    }

    fn expr(&mut self, e: &sa::Expression) -> E {
        use sa::Expression as S;
        let b = |n: &mut Self, e: &sa::Expression| Box::new(n.expr(e));
        match e {
            S::NamedNode(n) => E::NamedNode(nn(n)),
            S::Literal(l) => E::Literal(lit(l)),
            S::Variable(v) => E::Variable(self.var(v)),
            S::Or(x, y) => E::Or(b(self, x), b(self, y)),
            S::And(x, y) => E::And(b(self, x), b(self, y)),
            S::Equal(x, y) => E::Equal(b(self, x), b(self, y)),
            S::SameTerm(x, y) => E::SameTerm(b(self, x), b(self, y)),
            S::Greater(x, y) => E::Greater(b(self, x), b(self, y)),
            S::GreaterOrEqual(x, y) => E::GreaterOrEqual(b(self, x), b(self, y)),
            S::Less(x, y) => E::Less(b(self, x), b(self, y)),
            S::LessOrEqual(x, y) => E::LessOrEqual(b(self, x), b(self, y)),
            S::In(x, l) => E::In(b(self, x), l.iter().map(|e| self.expr(e)).collect()),
            S::Add(x, y) => E::Add(b(self, x), b(self, y)),
            S::Subtract(x, y) => E::Subtract(b(self, x), b(self, y)),
            S::Multiply(x, y) => E::Multiply(b(self, x), b(self, y)),
            S::Divide(x, y) => E::Divide(b(self, x), b(self, y)),
            S::UnaryPlus(x) => E::UnaryPlus(b(self, x)),
            S::UnaryMinus(x) => E::UnaryMinus(b(self, x)),
            S::Not(x) => E::Not(b(self, x)),
            S::Exists(p) => E::Exists(Box::new(self.pattern(p))),
            S::Bound(v) => E::Bound(self.var(v)),
            S::If(x, y, z) => E::If(b(self, x), b(self, y), b(self, z)),
            S::Coalesce(l) => E::Coalesce(l.iter().map(|e| self.expr(e)).collect()),
            S::FunctionCall(f, l) => {
                E::FunctionCall(self.function(f), l.iter().map(|e| self.expr(e)).collect())
            }
        }
    }

    fn aggregate(&mut self, a: &sa::AggregateExpression) -> A {
        match a {
            sa::AggregateExpression::CountSolutions { distinct } => A::CountSolutions {
                distinct: *distinct,
            },
            sa::AggregateExpression::FunctionCall {
                name,
                expr,
                distinct,
            } => A::FunctionCall {
                name: match name {
                    sa::AggregateFunction::Count => AF::Count,
                    sa::AggregateFunction::Sum => AF::Sum,
                    sa::AggregateFunction::Avg => AF::Avg,
                    sa::AggregateFunction::Min => AF::Min,
                    sa::AggregateFunction::Max => AF::Max,
                    sa::AggregateFunction::GroupConcat { separator } => AF::GroupConcat {
                        separator: separator.clone(),
                    },
                    sa::AggregateFunction::Sample => AF::Sample,
                    sa::AggregateFunction::Custom(n) => AF::Custom(nn(n)),
                },
                expr: self.expr(expr),
                distinct: *distinct,
            },
        }
    }

    fn pattern(&mut self, p: &sa::GraphPattern) -> P {
        use sa::GraphPattern as S;
        let b = |n: &mut Self, p: &sa::GraphPattern| Box::new(n.pattern(p));
        match p {
            S::Bgp { patterns } => P::Bgp {
                patterns: patterns.iter().map(|t| self.triple(t)).collect(),
            },
            S::Path {
                subject,
                path,
                object,
            } => P::Path {
                subject: self.tp(subject),
                path: self.path(path),
                object: self.tp(object),
            },
            S::Join { left, right } => P::join_all(vec![self.pattern(left), self.pattern(right)]),
            S::LeftJoin {
                left,
                right,
                expression,
            } => P::LeftJoin {
                left: b(self, left),
                right: b(self, right),
                expression: expression.as_ref().map(|e| self.expr(e)),
            },
            S::Lateral { left, right } => P::Lateral {
                left: b(self, left),
                right: b(self, right),
            },
            S::Filter { expr, inner } => P::Filter {
                expr: self.expr(expr),
                inner: b(self, inner),
            },
            S::Union { left, right } => {
                P::union_all(vec![self.pattern(left), self.pattern(right)])
            }
            S::Graph { name, inner } => P::Graph {
                name: self.nnp(name),
                inner: b(self, inner),
            },
            // A chain in spargebra, one `Extend` in NRESE (ADR-0012).
            S::Extend {
                inner,
                variable,
                expression,
            } => {
                let inner = self.pattern(inner);
                inner.extend(self.var(variable), self.expr(expression))
            }
            S::Minus { left, right } => P::Minus {
                left: b(self, left),
                right: b(self, right),
            },
            S::Values {
                variables,
                bindings,
            } => P::Values {
                variables: variables.iter().map(|v| self.var(v)).collect(),
                bindings: bindings
                    .iter()
                    .map(|row| {
                        row.iter()
                            .map(|t| t.as_ref().map(|t| self.ground(t)))
                            .collect()
                    })
                    .collect(),
            },
            S::OrderBy { inner, expression } => P::OrderBy {
                inner: b(self, inner),
                expression: expression
                    .iter()
                    .map(|o| match o {
                        sa::OrderExpression::Asc(e) => O::Asc(self.expr(e)),
                        sa::OrderExpression::Desc(e) => O::Desc(self.expr(e)),
                    })
                    .collect(),
            },
            S::Project { inner, variables } => P::Project {
                inner: b(self, inner),
                variables: variables.iter().map(|v| self.var(v)).collect(),
            },
            S::Distinct { inner } => P::Distinct {
                inner: b(self, inner),
            },
            S::Reduced { inner } => P::Reduced {
                inner: b(self, inner),
            },
            S::Slice {
                inner,
                start,
                length,
            } => P::Slice {
                inner: b(self, inner),
                start: *start,
                length: *length,
            },
            S::Group {
                inner,
                variables,
                aggregates,
            } => P::Group {
                inner: b(self, inner),
                variables: variables.iter().map(|v| self.var(v)).collect(),
                aggregates: aggregates
                    .iter()
                    .map(|(v, a)| (self.var(v), self.aggregate(a)))
                    .collect(),
            },
            S::Service {
                name,
                inner,
                silent,
            } => P::Service {
                name: self.nnp(name),
                inner: b(self, inner),
                silent: *silent,
            },
        }
    }

    fn dataset(&mut self, d: Option<&sa::QueryDataset>) -> Option<QueryDataset> {
        d.map(|d| QueryDataset {
            default: d.default.iter().map(nn).collect(),
            named: d.named.as_ref().map(|n| n.iter().map(nn).collect()),
        })
    }

    fn query(&mut self, q: &spargebra::Query) -> Query {
        use spargebra::Query as S;
        let base = |b: &Option<oxiri::Iri<String>>| {
            b.as_ref()
                .map(|b| nrese_rdf::Iri::parse_unchecked(b.as_str().to_owned()))
        };
        match q {
            S::Select {
                dataset,
                pattern,
                base_iri,
            } => Query::Select {
                dataset: self.dataset(dataset.as_ref()),
                pattern: self.pattern(pattern),
                base_iri: base(base_iri),
            },
            S::Construct {
                template,
                dataset,
                pattern,
                base_iri,
            } => Query::Construct {
                template: template.iter().map(|t| self.triple(t)).collect(),
                dataset: self.dataset(dataset.as_ref()),
                pattern: self.pattern(pattern),
                base_iri: base(base_iri),
            },
            S::Describe {
                dataset,
                pattern,
                base_iri,
            } => Query::Describe {
                dataset: self.dataset(dataset.as_ref()),
                pattern: self.pattern(pattern),
                base_iri: base(base_iri),
            },
            S::Ask {
                dataset,
                pattern,
                base_iri,
            } => Query::Ask {
                dataset: self.dataset(dataset.as_ref()),
                pattern: self.pattern(pattern),
                base_iri: base(base_iri),
            },
        }
    }

    fn graph_name(&mut self, g: &st::GraphName) -> GraphName {
        match g {
            st::GraphName::NamedNode(n) => nn(n).into(),
            st::GraphName::DefaultGraph => GraphName::DefaultGraph,
        }
    }

    fn target(&mut self, t: &sa::GraphTarget) -> GraphTarget {
        match t {
            sa::GraphTarget::NamedNode(n) => nn(n).into(),
            sa::GraphTarget::DefaultGraph => GraphTarget::DefaultGraph,
            sa::GraphTarget::NamedGraphs => GraphTarget::NamedGraphs,
            sa::GraphTarget::AllGraphs => GraphTarget::AllGraphs,
        }
    }

    fn term(&mut self, t: &oxrdf::Term) -> nrese_rdf::Term {
        match t {
            oxrdf::Term::NamedNode(n) => nn(n).into(),
            oxrdf::Term::BlankNode(b) => self.bnode(b).into(),
            oxrdf::Term::Literal(l) => lit(l).into(),
            oxrdf::Term::Triple(t) => nrese_rdf::Triple::new(
                match &t.subject {
                    oxrdf::NamedOrBlankNode::NamedNode(n) => {
                        nrese_rdf::NamedOrBlankNode::from(nn(n))
                    }
                    oxrdf::NamedOrBlankNode::BlankNode(b) => self.bnode(b).into(),
                },
                nn(&t.predicate),
                self.term(&t.object),
            )
            .into(),
        }
    }

    fn update(&mut self, u: &spargebra::Update) -> Update {
        let operations = u
            .operations
            .iter()
            .map(|op| match op {
                spargebra::GraphUpdateOperation::InsertData { data } => U::InsertData {
                    data: data
                        .iter()
                        .map(|q| Quad {
                            subject: match &q.subject {
                                oxrdf::NamedOrBlankNode::NamedNode(n) => nn(n).into(),
                                oxrdf::NamedOrBlankNode::BlankNode(b) => self.bnode(b).into(),
                            },
                            predicate: nn(&q.predicate),
                            object: self.term(&q.object),
                            graph_name: self.graph_name(&q.graph_name),
                        })
                        .collect(),
                },
                spargebra::GraphUpdateOperation::DeleteData { data } => U::DeleteData {
                    data: data
                        .iter()
                        .map(|q| GroundQuad {
                            subject: nn(&q.subject),
                            predicate: nn(&q.predicate),
                            object: self.ground(&q.object),
                            graph_name: self.graph_name(&q.graph_name),
                        })
                        .collect(),
                },
                spargebra::GraphUpdateOperation::DeleteInsert {
                    delete,
                    insert,
                    using,
                    pattern,
                } => U::DeleteInsert {
                    delete: delete
                        .iter()
                        .map(|q| GroundQuadPattern {
                            subject: self.gtp(&q.subject),
                            predicate: self.nnp(&q.predicate),
                            object: self.gtp(&q.object),
                            graph_name: self.gnp(&q.graph_name),
                        })
                        .collect(),
                    insert: insert
                        .iter()
                        .map(|q| QuadPattern {
                            subject: self.tp(&q.subject),
                            predicate: self.nnp(&q.predicate),
                            object: self.tp(&q.object),
                            graph_name: self.gnp(&q.graph_name),
                        })
                        .collect(),
                    using: self.dataset(using.as_ref()),
                    pattern: Box::new(self.pattern(pattern)),
                },
                spargebra::GraphUpdateOperation::Load {
                    silent,
                    source,
                    destination,
                } => U::Load {
                    silent: *silent,
                    source: nn(source),
                    destination: self.graph_name(destination),
                },
                spargebra::GraphUpdateOperation::Clear { silent, graph } => U::Clear {
                    silent: *silent,
                    graph: self.target(graph),
                },
                spargebra::GraphUpdateOperation::Create { silent, graph } => U::Create {
                    silent: *silent,
                    graph: nn(graph),
                },
                spargebra::GraphUpdateOperation::Drop { silent, graph } => U::Drop {
                    silent: *silent,
                    graph: self.target(graph),
                },
            })
            .collect();
        Update {
            base_iri: u
                .base_iri
                .as_ref()
                .map(|b| nrese_rdf::Iri::parse_unchecked(b.as_str().to_owned())),
            operations,
        }
    }
}

/// The names a parser made up, renamed `__0`, `__1`, … in order of first appearance:
/// nrese's (`__agg0`, `__b1`, …; a query using such a name of its own gets a longer prefix,
/// parser/mod.rs) and spargebra's random ones as [`Names`] renames them (`__x0`, …). A
/// query's own name starting with `__` is renamed with them, alike on both sides. Variables
/// and blank nodes alone: an IRI or a literal is no name (Sol, syntax-five r1: the literal
/// `"__agg0"` was renamed with the aggregate's variable, and `"__a"` compared equal to `"__b"`).
#[derive(Default)]
struct Generated(HashMap<String, usize>);

impl Generated {
    fn name(&mut self, name: &str) -> Option<String> {
        let n = self.0.len();
        name.starts_with("__")
            .then(|| format!("__{}", self.0.entry(name.to_owned()).or_insert(n)))
    }

    fn variable(&mut self, variable: &mut Variable) {
        if let Some(name) = self.name(variable.as_str()) {
            *variable = Variable::new_unchecked(name);
        }
    }

    fn blank_node(&mut self, node: &mut BlankNode) {
        if let Some(name) = self.name(node.as_str()) {
            *node = BlankNode::new_unchecked(name);
        }
    }

    fn term(&mut self, term: &mut TermPattern) {
        match term {
            TermPattern::Variable(v) => self.variable(v),
            TermPattern::BlankNode(b) => self.blank_node(b),
            TermPattern::Triple(t) => self.triple(t),
            TermPattern::NamedNode(_) | TermPattern::Literal(_) => {}
        }
    }

    fn predicate(&mut self, predicate: &mut NamedNodePattern) {
        match predicate {
            NamedNodePattern::Variable(v) => self.variable(v),
            NamedNodePattern::NamedNode(_) => {}
        }
    }

    fn graph(&mut self, graph: &mut GraphNamePattern) {
        match graph {
            GraphNamePattern::Variable(v) => self.variable(v),
            GraphNamePattern::NamedNode(_) | GraphNamePattern::DefaultGraph => {}
        }
    }

    fn triple(&mut self, triple: &mut TriplePattern) {
        self.term(&mut triple.subject);
        self.predicate(&mut triple.predicate);
        self.term(&mut triple.object);
    }

    fn ground(&mut self, term: &mut GroundTermPattern) {
        match term {
            GroundTermPattern::Variable(v) => self.variable(v),
            GroundTermPattern::Triple(t) => {
                self.ground(&mut t.subject);
                self.predicate(&mut t.predicate);
                self.ground(&mut t.object);
            }
            GroundTermPattern::NamedNode(_) | GroundTermPattern::Literal(_) => {}
        }
    }

    fn data(&mut self, term: &mut nrese_rdf::Term) {
        match term {
            nrese_rdf::Term::BlankNode(b) => self.blank_node(b),
            nrese_rdf::Term::Triple(t) => {
                self.subject(&mut t.subject);
                self.data(&mut t.object);
            }
            nrese_rdf::Term::NamedNode(_) | nrese_rdf::Term::Literal(_) => {}
        }
    }

    fn subject(&mut self, subject: &mut nrese_rdf::NamedOrBlankNode) {
        match subject {
            nrese_rdf::NamedOrBlankNode::BlankNode(b) => self.blank_node(b),
            nrese_rdf::NamedOrBlankNode::NamedNode(_) => {}
        }
    }

    /// The names a node holds itself, then its children's (`visit.rs`).
    fn node(&mut self, node: NodeMut<'_>) {
        match node {
            NodeMut::Pattern(pattern) => {
                match pattern {
                    P::Bgp { patterns } => patterns.iter_mut().for_each(|t| self.triple(t)),
                    P::Path {
                        subject, object, ..
                    } => {
                        self.term(subject);
                        self.term(object);
                    }
                    P::Graph { name, .. } | P::Service { name, .. } => self.predicate(name),
                    P::Extend { bindings, .. } => {
                        bindings.iter_mut().for_each(|(v, _)| self.variable(v))
                    }
                    P::Values { variables, .. } | P::Project { variables, .. } => {
                        variables.iter_mut().for_each(|v| self.variable(v))
                    }
                    P::Group {
                        variables,
                        aggregates,
                        ..
                    } => {
                        variables.iter_mut().for_each(|v| self.variable(v));
                        aggregates.iter_mut().for_each(|(v, _)| self.variable(v));
                    }
                    P::Join { .. }
                    | P::LeftJoin { .. }
                    | P::Lateral { .. }
                    | P::Filter { .. }
                    | P::Union { .. }
                    | P::Minus { .. }
                    | P::OrderBy { .. }
                    | P::Distinct { .. }
                    | P::Reduced { .. }
                    | P::Slice { .. } => {}
                }
                pattern
                    .children_mut()
                    .into_iter()
                    .for_each(|n| self.node(n));
            }
            NodeMut::Expression(expression) => {
                if let E::Variable(v) | E::Bound(v) = expression {
                    self.variable(v);
                }
                expression
                    .children_mut()
                    .into_iter()
                    .for_each(|n| self.node(n));
            }
        }
    }
}

/// `-1` and `+1` of a number literal as the signed literal, once the operand is folded
/// itself: nrese reads the literal the grammar's tokens make, spargebra a negation of `1`
/// computed at run time. Over a literal alone (Sol, syntax-five r1: a base IRI spelling
/// `UnaryMinus(Literal(…integer))` panicked, `Unary_` and `Unary` compared equal).
fn fold_sign(expression: &mut E) {
    let (sign, operand) = match expression {
        E::UnaryMinus(operand) => ('-', operand),
        E::UnaryPlus(operand) => ('+', operand),
        _ => return,
    };
    if let E::Literal(number) = &**operand
        && [xsd::INTEGER, xsd::DECIMAL, xsd::DOUBLE].contains(&number.datatype())
    {
        let signed = format!("{sign}{}", number.value());
        *expression = E::Literal(Literal::new_typed_literal(signed, number.datatype()));
    }
}

/// `&&` and `||` as the n-ary operators they are: each nest of one of them is built again
/// from its operands, in order, as `Expression::balanced` builds it; and a sign over a number
/// literal folded ([`fold_sign`]). Both operators are associative in SPARQL's logic of true,
/// false and error; nrese builds them as balanced trees (ADR-0012) where spargebra folds them
/// left, and parentheses group them either way: the tree's shape is no difference. On the
/// algebra itself, so nothing else (a base IRI's text) is taken for an operator (SYNTAX5-F1,
/// Sol: two distinct bases compared equal as text).
fn associate(node: NodeMut<'_>) {
    match node {
        NodeMut::Pattern(pattern) => pattern.children_mut().into_iter().for_each(associate),
        NodeMut::Expression(expression) if matches!(expression, E::And(..) | E::Or(..)) => {
            let and = matches!(expression, E::And(..));
            let placeholder = E::Variable(Variable::new_unchecked("nest"));
            let mut operands = Vec::new();
            nest(
                std::mem::replace(expression, placeholder),
                and,
                &mut operands,
            );
            for operand in &mut operands {
                associate(NodeMut::Expression(operand));
            }
            let join = if and { E::And } else { E::Or };
            *expression = E::balanced(operands, join).expect("&& and || have two operands");
        }
        NodeMut::Expression(expression) => {
            expression.children_mut().into_iter().for_each(associate);
            fold_sign(expression);
        }
    }
}

/// The operands of a nest of `&&` (`and`) or `||`, in order.
fn nest(expression: E, and: bool, operands: &mut Vec<E>) {
    match (expression, and) {
        (E::And(a, b), true) | (E::Or(a, b), false) => {
            nest(*a, and, operands);
            nest(*b, and, operands);
        }
        (other, _) => operands.push(other),
    }
}

/// A query as the test compares it: `&&` and `||` associated and signs folded alike, the
/// generated names renamed, then rendered.
fn canonical(mut query: Query) -> String {
    let mut names = Generated::default();
    match &mut query {
        Query::Construct {
            template, pattern, ..
        } => {
            template.iter_mut().for_each(|t| names.triple(t));
            associate(NodeMut::Pattern(pattern));
            names.node(NodeMut::Pattern(pattern));
        }
        Query::Select { pattern, .. }
        | Query::Describe { pattern, .. }
        | Query::Ask { pattern, .. } => {
            associate(NodeMut::Pattern(pattern));
            names.node(NodeMut::Pattern(pattern));
        }
    }
    format!("{query:?}")
}

/// An update as the test compares it, as [`canonical`] a query.
fn canonical_update(mut update: Update) -> String {
    let mut names = Generated::default();
    for operation in &mut update.operations {
        match operation {
            U::InsertData { data } => {
                for quad in data {
                    names.subject(&mut quad.subject);
                    names.data(&mut quad.object);
                }
            }
            U::DeleteInsert {
                delete,
                insert,
                pattern,
                ..
            } => {
                for quad in delete {
                    names.ground(&mut quad.subject);
                    names.predicate(&mut quad.predicate);
                    names.ground(&mut quad.object);
                    names.graph(&mut quad.graph_name);
                }
                for quad in insert {
                    names.term(&mut quad.subject);
                    names.predicate(&mut quad.predicate);
                    names.term(&mut quad.object);
                    names.graph(&mut quad.graph_name);
                }
                associate(NodeMut::Pattern(pattern));
                names.node(NodeMut::Pattern(pattern));
            }
            U::DeleteData { .. }
            | U::Load { .. }
            | U::Clear { .. }
            | U::Create { .. }
            | U::Drop { .. } => {}
        }
    }
    format!("{update:?}")
}

/// The repository's own queries and updates (`.rq`, `.ru` under `crates`, `benches` and
/// `docs`): its name relative to the repository, its path. A directory that can't be read
/// fails the test.
fn repository() -> Vec<(String, PathBuf)> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap();
    let mut files = Vec::new();
    let mut stack = vec![root.join("crates"), root.join("benches"), root.join("docs")];
    while let Some(dir) = stack.pop() {
        let entries = std::fs::read_dir(&dir).unwrap_or_else(|e| panic!("{}: {e}", dir.display()));
        for entry in entries {
            let path = entry
                .unwrap_or_else(|e| panic!("{}: {e}", dir.display()))
                .path();
            let name = path.file_name().unwrap().to_string_lossy().into_owned();
            if path.is_dir() {
                if name != "target" && !name.starts_with('.') {
                    stack.push(path);
                }
            } else if name.ends_with(".rq") || name.ends_with(".ru") {
                let relative = path.strip_prefix(&root).unwrap();
                files.push((relative.to_string_lossy().replace('\\', "/"), path));
            }
        }
    }
    files.sort();
    files
}

/// Where two normalised algebras part: after how many characters, and each from shortly
/// before that point.
fn divergence(ours: &str, theirs: &str) -> String {
    let same = ours
        .chars()
        .zip(theirs.chars())
        .take_while(|(a, b)| a == b)
        .count();
    let start = same.saturating_sub(120);
    let show = |algebra: &str| algebra.chars().skip(start).take(600).collect::<String>();
    format!(
        "  they part after {same} characters\n  nrese:     ...{}\n  spargebra: ...{}",
        show(ours),
        show(theirs)
    )
}

fn file_iri(path: &Path) -> String {
    // An absolute path as given (w3c.rs and repository() make them from canonical roots): the
    // file itself may be gone, its bytes being the pinned ones.
    let path = path.to_string_lossy().replace('\\', "/");
    let path = path.trim_start_matches("//?/");
    if path.starts_with('/') {
        format!("file://{path}")
    } else {
        format!("file:///{path}")
    }
}

#[test]
fn every_query_parses_to_the_same_algebra() {
    // The pinned W3C suites in full, or (where they aren't required) the repository's own
    // files alone, said to qualify nothing.
    let suites = w3c::admitted(w3c::sparql(&["rq", "ru"])).unwrap_or_default();
    let own = repository();
    eprintln!(
        "W3C files: {} ({}); the repository's own: {}",
        suites.len(),
        w3c::counts(&suites),
        own.len()
    );
    let inputs = suites
        .iter()
        .map(|file| (file.name.clone(), file.path.clone(), file.bytes.clone()))
        .chain(own.into_iter().map(|(name, path)| {
            let bytes = std::fs::read(&path).unwrap_or_else(|e| panic!("{name}: {e}"));
            (name, path, bytes)
        }));
    let mut outcome: BTreeMap<&str, Vec<String>> = BTreeMap::new();
    // The two normalised algebras of each file whose algebra differs, for the report.
    let mut algebras: BTreeMap<String, (String, String)> = BTreeMap::new();
    for (name, path, bytes) in inputs {
        // Neither parser reads bytes: a file that isn't UTF-8 is counted, not compared.
        let Ok(text) = String::from_utf8(bytes) else {
            outcome.entry("not UTF-8").or_default().push(name);
            continue;
        };
        let path = path.as_path();
        let base = file_iri(path);
        let update = path.extension().is_some_and(|e| e == "ru");
        let ours = SparqlParser::new().with_base_iri(base.as_str()).unwrap();
        let theirs = spargebra::SparqlParser::new()
            .with_base_iri(base.as_str())
            .unwrap();
        let (ours, theirs): (Result<String, String>, Result<String, String>) = if update {
            (
                ours.parse_update(&text)
                    .map(canonical_update)
                    .map_err(|e| e.to_string()),
                theirs
                    .parse_update(&text)
                    .map(|u| canonical_update(Names::default().update(&u)))
                    .map_err(|e| e.to_string()),
            )
        } else {
            (
                ours.parse_query(&text)
                    .map(canonical)
                    .map_err(|e| e.to_string()),
                theirs
                    .parse_query(&text)
                    .map(|q| canonical(Names::default().query(&q)))
                    .map_err(|e| e.to_string()),
            )
        };
        let key = match (ours, theirs) {
            (Ok(a), Ok(b)) => {
                if a == b {
                    "same algebra"
                } else {
                    algebras.insert(name.clone(), (a, b));
                    "different algebra"
                }
            }
            (Err(_), Err(_)) => "both reject",
            (Ok(_), Err(_)) => "only nrese accepts",
            (Err(_), Ok(_)) => "only spargebra accepts",
        };
        outcome.entry(key).or_default().push(name);
    }
    for (key, files) in &outcome {
        eprintln!("{key}: {}", files.len());
        if *key != "same algebra" && *key != "both reject" {
            for f in files {
                eprintln!("    {f}");
            }
        }
    }
    // A difference is explained only by an entry naming its file and this outcome; and with
    // the suites read, every entry is seen with its outcome (none stands for a file that
    // now agrees, or differs otherwise).
    let explained = |file: &str, key: &str| explains(EXPLAINED, file, key);
    let unexplained: Vec<String> = outcome
        .iter()
        .filter(|(k, _)| !matches!(**k, "same algebra" | "both reject" | "not UTF-8"))
        .flat_map(|(key, files)| files.iter().map(move |file| (key, file)))
        .filter(|(key, file)| !explained(file, key))
        .map(|(key, file)| match algebras.get(file.as_str()) {
            Some((ours, theirs)) => format!("{file}: {key}\n{}", divergence(ours, theirs)),
            None => format!("{file}: {key}"),
        })
        .collect();
    let unseen: Vec<String> = if suites.is_empty() {
        Vec::new()
    } else {
        not_seen(&outcome, EXPLAINED)
    };
    assert!(
        unexplained.is_empty() && unseen.is_empty(),
        "unexplained differences:\n{}\nexplained but not seen so: {unseen:#?}",
        unexplained.join("\n")
    );
}

/// Differences, each with the outcome it has and its reason: in each, nrese follows the
/// specification.
const EXPLAINED: &[(&str, &str, &str)] = &[
    (
        "sparql10/expr-ops/query-add-literals.rq",
        "different algebra",
        "spargebra reads `a + b + c` as `a + (b + c)`; the grammar's rule 116 associates to the left",
    ),
    (
        "sparql10/i18n/normalization-02.rq",
        "different algebra",
        "an absolute IRI with dot segments: nrese resolves it as RFC 3986 section 5.2.2 says \
         (and as nrese-rdf-io does for data), spargebra (oxiri) keeps the segments",
    ),
    (
        "sparql10/expr-builtin/case-insensitive-booleans.rq",
        "only nrese accepts",
        "TRUE and FALSE: keywords are case-insensitive (SPARQL 1.1 section 19.3), spargebra rejects them",
    ),
    (
        "sparql12/grouping/select-variable-reuse.rq",
        "only nrese accepts",
        "W3C SPARQL 1.2 positive test: a select expression may use an earlier one's variable",
    ),
    (
        "sparql10/syntax-sparql3/syn-bad-26.rq",
        "only spargebra accepts",
        "W3C negative test: by the longest-token rule `<?a&&?b>` is an IRI, not `<` and `&&`",
    ),
    (
        "sparql12/syntax/nested-aggregate-functions.rq",
        "only spargebra accepts",
        "W3C negative test: an aggregate's argument holds no aggregate",
    ),
    (
        "sparql12/syntax-triple-terms-negative/tripleterm-subject-03.rq",
        "only spargebra accepts",
        "W3C negative test: a triple term in an expression has no triple term as subject",
    ),
    (
        "sparql12/syntax-triple-terms-negative/tripleterm-subject-06.rq",
        "only spargebra accepts",
        "W3C negative test: a triple term in an expression has no literal as subject",
    ),
];

/// The difference listed for `query-add-literals.rq`, where it changes a value: spargebra
/// reads `10 - 5 - 2` as `10 - (5 - 2)` (7), nrese as `(10 - 5) - 2` (3).
#[test]
fn subtraction_associates_to_the_left_in_nrese_only() {
    let text = "SELECT (10 - 5 - 2 AS ?x) {}";
    let theirs = spargebra::SparqlParser::new().parse_query(text).unwrap();
    let ours = SparqlParser::new().parse_query(text).unwrap();
    let theirs = format!("{:?}", Names::default().query(&theirs));
    let ours = format!("{ours:?}");
    // spargebra: the outer subtraction's left operand is the literal 10, its right `5 - 2`.
    assert!(!theirs.contains("Subtract(Subtract("), "{theirs}");
    assert!(theirs.contains("Subtract(Literal(Literal { value: \"10\""), "{theirs}");
    // nrese: the left operand of the outer subtraction is `10 - 5`.
    assert!(ours.contains("Subtract(Subtract(Literal(Literal { value: \"10\""), "{ours}");
}

/// SYNTAX5-F1 (Sol, PR #61 fix check 2): `&&` and `||` were reassociated in the algebra's
/// text, a base IRI's included, so two distinct bases (which `IRI("leaf")` resolves against)
/// compared equal, and a base spelling `And()` panicked. The nesting of one of them is no
/// difference; bases, terms, mixed operators, negation, scope and operand order are.
#[test]
fn and_and_or_differ_in_nothing_but_their_nesting() {
    let query = |text: &str| canonical(SparqlParser::new().parse_query(text).unwrap());
    // The base alone tells the queries apart.
    let base = |iri: &str| query(&format!("BASE <http://example.org/{iri}/> SELECT * {{}}"));
    assert_ne!(
        base("And(aaaa,And(bbbb,cccc))"),
        base("And(And(aaaa,bbbb),cccc)"),
        "SPEC two base IRIs differ, whatever they spell"
    );
    base("And()"); // read as no operator

    let filter = |e: &str| format!("SELECT * {{ ?a ?b ?c FILTER({e}) }}");
    let same = [
        (filter("?a && (?b && ?c)"), filter("(?a && ?b) && ?c")),
        (
            filter("?a || ?b || ?c || ?d || ?e"),
            filter("((?a || ?b) || (?c || ?d)) || ?e"),
        ),
        (
            "SELECT * { ?a ?b ?c FILTER EXISTS { ?a ?b ?c FILTER(?a && (?b && ?c)) } }".into(),
            "SELECT * { ?a ?b ?c FILTER EXISTS { ?a ?b ?c FILTER((?a && ?b) && ?c) } }".into(),
        ),
        (
            "SELECT * { ?a ?b ?c } ORDER BY (?a && (?b && ?c))".into(),
            "SELECT * { ?a ?b ?c } ORDER BY ((?a && ?b) && ?c)".into(),
        ),
        (
            "SELECT (SUM(IF(?a || (?b || ?c), 1, 0)) AS ?n) { ?a ?b ?c }".into(),
            "SELECT (SUM(IF((?a || ?b) || ?c, 1, 0)) AS ?n) { ?a ?b ?c }".into(),
        ),
        (
            "SELECT * { ?a ?b ?c BIND(?a || (?b || ?c) AS ?d) }".into(),
            "SELECT * { ?a ?b ?c BIND((?a || ?b) || ?c AS ?d) }".into(),
        ),
        (
            "SELECT * { ?a ?b ?c OPTIONAL { ?a ?b ?d FILTER(?a && (?b && ?d)) } }".into(),
            "SELECT * { ?a ?b ?c OPTIONAL { ?a ?b ?d FILTER((?a && ?b) && ?d) } }".into(),
        ),
    ];
    for (a, b) in &same {
        assert_eq!(
            query(a),
            query(b),
            "SPEC only the nesting differs: {a} | {b}"
        );
    }
    let update = |text: &str| canonical_update(SparqlParser::new().parse_update(text).unwrap());
    assert_eq!(
        update("DELETE { ?a ?b ?c } WHERE { ?a ?b ?c FILTER(?a && (?b && ?c)) }"),
        update("DELETE { ?a ?b ?c } WHERE { ?a ?b ?c FILTER((?a && ?b) && ?c) }"),
        "SPEC an update's pattern alike"
    );
    let distinct = [
        (filter("?a && ?b"), filter("?a && ?c")),
        (filter("?a && (?b || ?c)"), filter("(?a && ?b) || ?c")),
        (filter("!(?a && ?b)"), filter("!?a && ?b")),
        (filter("?a && ?b"), filter("?b && ?a")),
        (
            filter("?a = \"And(x,And(y,z))\""),
            filter("?a = \"And(And(x,y),z)\""),
        ),
        (
            "SELECT * { ?a ?b ?c OPTIONAL { ?a ?b ?d FILTER(?a && ?d) } }".into(),
            "SELECT * { ?a ?b ?c OPTIONAL { ?a ?b ?d } FILTER(?a && ?d) }".into(),
        ),
    ];
    for (a, b) in &distinct {
        assert_ne!(
            query(a),
            query(b),
            "SPEC more than the nesting differs: {a} | {b}"
        );
    }
}

/// Sol, syntax-five r1 (S1): signs were folded in the algebra's text, so a base IRI spelling
/// `UnaryMinus(Literal(…integer))` panicked and two bases apart by `Unary_` compared equal.
/// A sign folds over a number literal and nothing else.
#[test]
fn a_sign_folds_over_a_number_literal_alone() {
    let query = |text: &str| canonical(SparqlParser::new().parse_query(text).unwrap());
    let base = |iri: &str| query(&format!("BASE <http://example.org/{iri}/> SELECT * {{}}"));
    assert_ne!(
        base("Unary_"),
        base("Unary"),
        "SPEC two base IRIs differ, whatever they spell"
    );
    let update = concat!(
        "BASE <http://example.org/UnaryMinus(Literal(http://www.w3.org/2001/XMLSchema#integer))> ",
        "INSERT DATA { <urn:s> <urn:p> 1 }"
    );
    canonical_update(SparqlParser::new().parse_update(update).unwrap()); // read as no sign
    let text = "SELECT (-1 AS ?a) {}";
    let theirs = spargebra::SparqlParser::new().parse_query(text).unwrap();
    assert_eq!(
        query(text),
        canonical(Names::default().query(&theirs)),
        "SPEC -1 is the signed literal on both sides"
    );
    assert_ne!(
        query("SELECT (-\"1\" AS ?a) {}"),
        query("SELECT (\"-1\" AS ?a) {}"),
        "SPEC a sign over a string stays"
    );
}

/// Sol, syntax-five r1 (S2): generated names were renamed in the algebra's text, so a literal
/// starting with `__` was renamed as one: `"__agg0"` beside an aggregate made a difference
/// where there is none, and `"__a"` compared equal to `"__b"`. Names are variables and blank
/// nodes.
#[test]
fn generated_names_are_variables_and_blank_nodes_alone() {
    let ours = |text: &str| canonical(SparqlParser::new().parse_query(text).unwrap());
    let theirs = |text: &str| {
        let query = spargebra::SparqlParser::new().parse_query(text).unwrap();
        canonical(Names::default().query(&query))
    };
    let text = "SELECT (COUNT(*) AS ?n) WHERE { ?s ?p \"__agg0\" }";
    assert_eq!(ours(text), theirs(text), "SPEC a literal is no name");
    let text = "SELECT * { ?s ?p [] }";
    assert_eq!(
        ours(text),
        theirs(text),
        "SPEC generated blank nodes renamed alike"
    );
    assert_ne!(
        ours("SELECT * { ?a ?b ?c FILTER(?a = \"__a\") }"),
        ours("SELECT * { ?a ?b ?c FILTER(?a = \"__b\") }"),
        "SPEC two literals differ, whatever they spell"
    );
}

/// Whether an entry of `list` explains `file`'s difference `key`: one naming the file and
/// that outcome (D-106: the file's name alone explained any outcome it came to have).
fn explains(list: &[(&str, &str, &str)], file: &str, key: &str) -> bool {
    list.iter().any(|(f, k, _)| *f == file && *k == key)
}

/// The entries of `list` the outcome doesn't show, each as `file: outcome`: an entry stands
/// for a difference that is there, as it is.
fn not_seen(outcome: &BTreeMap<&str, Vec<String>>, list: &[(&str, &str, &str)]) -> Vec<String> {
    list.iter()
        .filter(|(file, key, _)| {
            !outcome
                .get(key)
                .is_some_and(|files| files.iter().any(|f| f == file))
        })
        .map(|(file, key, _)| format!("{file}: {key}"))
        .collect()
}

#[test]
fn an_explanation_holds_for_its_file_and_outcome_alone() {
    // D-106 (Sol, PR #61 r1: a witness the existing differences don't mask): an explained file
    // whose outcome changed is a difference again, and its entry stands unseen.
    let list = [("a.rq", "only nrese accepts", "why")];
    let changed = BTreeMap::from([("different algebra", vec!["a.rq".to_owned()])]);
    assert!(
        !explains(&list, "a.rq", "different algebra"),
        "SPEC the file's name alone explains nothing"
    );
    assert_eq!(
        not_seen(&changed, &list),
        ["a.rq: only nrese accepts"],
        "SPEC the entry isn't seen so"
    );
    let kept = BTreeMap::from([("only nrese accepts", vec!["a.rq".to_owned()])]);
    assert!(explains(&list, "a.rq", "only nrese accepts"));
    assert!(not_seen(&kept, &list).is_empty());
}
