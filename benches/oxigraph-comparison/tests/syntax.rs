//! nrese-sparql-syntax against spargebra: every query and update of the W3C SPARQL 1.0,
//! 1.1 and 1.2 suites (and the repository's own `.rq`/`.ru` files) parsed by both, their
//! algebra compared after converting spargebra's into nrese's.
//!
//! What may differ, and is normalised before comparing:
//! - generated names (spargebra's are random 128-bit numbers, nrese's are numbered);
//! - `-1` in an expression: nrese reads the signed literal the grammar's tokens make,
//!   spargebra a negation of `1` computed at run time.
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
use nrese_sparql_syntax::{GraphUpdateOperation as U, Query, SparqlParser, Update};
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
            S::Sequence(a, b) => PP::Sequence(Box::new(self.path(a)), Box::new(self.path(b))),
            S::Alternative(a, b) => PP::Alternative(Box::new(self.path(a)), Box::new(self.path(b))),
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
            S::Join { left, right } => P::Join {
                left: b(self, left),
                right: b(self, right),
            },
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
            S::Union { left, right } => P::Union {
                left: b(self, left),
                right: b(self, right),
            },
            S::Graph { name, inner } => P::Graph {
                name: self.nnp(name),
                inner: b(self, inner),
            },
            S::Extend {
                inner,
                variable,
                expression,
            } => P::Extend {
                inner: b(self, inner),
                variable: self.var(variable),
                expression: self.expr(expression),
            },
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

/// A `Debug` rendering with generated names (`__agg0`, `__x3`, …) replaced by their order
/// of first appearance, and `-(1)` of a number as the signed literal.
fn normalised(debug: &str) -> String {
    let mut names: HashMap<String, usize> = HashMap::new();
    let mut out = String::with_capacity(debug.len());
    let mut rest = debug;
    while let Some(i) = rest.find("\"__") {
        out.push_str(&rest[..=i]);
        let after = &rest[i + 1..];
        let end = after.find('"').unwrap_or(after.len());
        let name = &after[..end];
        let n = names.len();
        let id = *names.entry(name.to_owned()).or_insert(n);
        out.push_str(&format!("#gen{id}"));
        rest = &after[end..];
    }
    out.push_str(rest);
    out
}

/// `UnaryMinus(1)` and `UnaryPlus(1)` of a number literal as the signed literal.
fn fold_signs(debug: String) -> String {
    let mut out = debug;
    for (op, sign) in [("UnaryMinus(Literal(", "-"), ("UnaryPlus(Literal(", "+")] {
        while let Some(i) = out.find(op) {
            // UnaryMinus(Literal(Literal { value: "1", kind: Typed(…) }))
            let start = i + op.len();
            let Some(value_at) = out[start..].find("value: \"").map(|v| start + v + 8) else {
                break;
            };
            let Some(close) = find_close(&out, i + op.len() - 1) else {
                break;
            };
            let inner = out[i + op.len() - "Literal(".len()..close].to_owned();
            let numeric = [xsd::INTEGER, xsd::DECIMAL, xsd::DOUBLE]
                .iter()
                .any(|dt| inner.contains(dt.as_str()));
            if !numeric {
                // Not a number: leave it, marked so the loop moves on.
                out.replace_range(i..i + 5, "Unary_");
                continue;
            }
            let mut literal = inner;
            let v = value_at - (i + op.len() - "Literal(".len());
            literal.insert_str(v, sign);
            out.replace_range(i..=close, &literal);
        }
    }
    out.replace("Unary_", "Unary")
}

/// The index of the `)` closing the `(` at `open`.
fn find_close(s: &str, open: usize) -> Option<usize> {
    let mut depth = 0;
    let mut in_string = false;
    let mut escaped = false;
    for (i, c) in s[open..].char_indices() {
        if in_string {
            match (escaped, c) {
                (true, _) => escaped = false,
                (false, '\\') => escaped = true,
                (false, '"') => in_string = false,
                _ => {}
            }
            continue;
        }
        match c {
            '"' => in_string = true,
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 {
                    return Some(open + i);
                }
            }
            _ => {}
        }
    }
    None
}

fn corpus() -> Vec<PathBuf> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let mut files = Vec::new();
    let mut stack = vec![
        root.join(".cache/rdf-tests/sparql"),
        root.join("crates"),
        root.join("benches"),
        root.join("docs"),
    ];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let name = path.file_name().unwrap().to_string_lossy().into_owned();
            if path.is_dir() {
                if name != "target" && !name.starts_with('.') || name == ".cache" {
                    stack.push(path);
                }
            } else if name.ends_with(".rq") || name.ends_with(".ru") {
                files.push(path);
            }
        }
    }
    files.sort();
    files
}

fn file_iri(path: &Path) -> String {
    let path = path
        .canonicalize()
        .unwrap()
        .to_string_lossy()
        .replace('\\', "/");
    let path = path.trim_start_matches("//?/");
    if path.starts_with('/') {
        format!("file://{path}")
    } else {
        format!("file:///{path}")
    }
}

#[test]
fn every_query_parses_to_the_same_algebra() {
    let files = corpus();
    if files.len() < 100 {
        eprintln!("skipped: no W3C suites (scripts/fetch-w3c-tests.sh)");
        return;
    }
    let mut outcome: BTreeMap<&str, Vec<String>> = BTreeMap::new();
    for path in &files {
        let Ok(text) = std::fs::read_to_string(path) else {
            continue;
        };
        let base = file_iri(path);
        let update = path.extension().is_some_and(|e| e == "ru");
        let name = path.to_string_lossy().replace('\\', "/");
        let name = name.split("/sparql/").last().unwrap_or(&name).to_owned();
        let ours = SparqlParser::new().with_base_iri(base.as_str()).unwrap();
        let theirs = spargebra::SparqlParser::new()
            .with_base_iri(base.as_str())
            .unwrap();
        let (ours, theirs): (Result<String, String>, Result<String, String>) = if update {
            (
                ours.parse_update(&text)
                    .map(|u| format!("{u:?}"))
                    .map_err(|e| e.to_string()),
                theirs
                    .parse_update(&text)
                    .map(|u| format!("{:?}", Names::default().update(&u)))
                    .map_err(|e| e.to_string()),
            )
        } else {
            (
                ours.parse_query(&text)
                    .map(|q| format!("{q:?}"))
                    .map_err(|e| e.to_string()),
                theirs
                    .parse_query(&text)
                    .map(|q| format!("{:?}", Names::default().query(&q)))
                    .map_err(|e| e.to_string()),
            )
        };
        let key = match (ours, theirs) {
            (Ok(a), Ok(b)) => {
                if normalised(&fold_signs(a)) == normalised(&fold_signs(b)) {
                    "same algebra"
                } else {
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
    let unexplained: Vec<&String> = outcome
        .iter()
        .filter(|(k, _)| **k != "same algebra" && **k != "both reject")
        .flat_map(|(_, files)| files)
        .filter(|f| !EXPLAINED.iter().any(|(e, _)| f.ends_with(e)))
        .collect();
    assert!(
        unexplained.is_empty(),
        "unexplained differences: {unexplained:#?}"
    );
}

/// Differences with their reasons: in each, nrese follows the specification.
const EXPLAINED: &[(&str, &str)] = &[
    (
        "sparql10/expr-ops/query-add-literals.rq",
        "spargebra reads `a + b + c` as `a + (b + c)`; the grammar's rule 116 associates to the left",
    ),
    (
        "sparql10/i18n/normalization-02.rq",
        "an absolute IRI with dot segments: nrese resolves it as RFC 3986 section 5.2.2 says \
         (and as nrese-rdf-io does for data), spargebra (oxiri) keeps the segments",
    ),
    (
        "sparql10/expr-builtin/case-insensitive-booleans.rq",
        "TRUE and FALSE: keywords are case-insensitive (SPARQL 1.1 section 19.3), spargebra rejects them",
    ),
    (
        "sparql12/grouping/select-variable-reuse.rq",
        "W3C SPARQL 1.2 positive test: a select expression may use an earlier one's variable",
    ),
    (
        "sparql10/syntax-sparql3/syn-bad-26.rq",
        "W3C negative test: by the longest-token rule `<?a&&?b>` is an IRI, not `<` and `&&`",
    ),
    (
        "sparql12/syntax/nested-aggregate-functions.rq",
        "W3C negative test: an aggregate's argument holds no aggregate",
    ),
    (
        "sparql12/syntax-triple-terms-negative/tripleterm-subject-03.rq",
        "W3C negative test: a triple term in an expression has no triple term as subject",
    ),
    (
        "sparql12/syntax-triple-terms-negative/tripleterm-subject-06.rq",
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
