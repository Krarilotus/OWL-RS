//! The SPARQL algebra (SPARQL 1.1 §18 with the SPARQL 1.2 additions): what a query means,
//! as the parser builds it and the engine plans from it. Each type prints as SPARQL that
//! parses back to the same algebra (see [`crate::writer`]).

use std::fmt;

use nrese_rdf::{Literal, NamedNode, Variable};

use crate::term::{GroundTerm, NamedNodePattern, TermPattern, TriplePattern};
use crate::writer;

/// A property path (SPARQL 1.1 §9).
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub enum PropertyPathExpression {
    NamedNode(NamedNode),
    Reverse(Box<Self>),
    Sequence(Box<Self>, Box<Self>),
    Alternative(Box<Self>, Box<Self>),
    ZeroOrMore(Box<Self>),
    OneOrMore(Box<Self>),
    ZeroOrOne(Box<Self>),
    NegatedPropertySet(Vec<NamedNode>),
}

impl From<NamedNode> for PropertyPathExpression {
    fn from(node: NamedNode) -> Self {
        Self::NamedNode(node)
    }
}

impl fmt::Display for PropertyPathExpression {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writer::fmt_path(self, f)
    }
}

/// An expression (SPARQL 1.1 §17).
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub enum Expression {
    NamedNode(NamedNode),
    Literal(Literal),
    Variable(Variable),
    Or(Box<Self>, Box<Self>),
    And(Box<Self>, Box<Self>),
    Equal(Box<Self>, Box<Self>),
    SameTerm(Box<Self>, Box<Self>),
    Greater(Box<Self>, Box<Self>),
    GreaterOrEqual(Box<Self>, Box<Self>),
    Less(Box<Self>, Box<Self>),
    LessOrEqual(Box<Self>, Box<Self>),
    In(Box<Self>, Vec<Self>),
    Add(Box<Self>, Box<Self>),
    Subtract(Box<Self>, Box<Self>),
    Multiply(Box<Self>, Box<Self>),
    Divide(Box<Self>, Box<Self>),
    UnaryPlus(Box<Self>),
    UnaryMinus(Box<Self>),
    Not(Box<Self>),
    Exists(Box<GraphPattern>),
    Bound(Variable),
    If(Box<Self>, Box<Self>, Box<Self>),
    Coalesce(Vec<Self>),
    FunctionCall(Function, Vec<Self>),
}

impl From<NamedNode> for Expression {
    fn from(node: NamedNode) -> Self {
        Self::NamedNode(node)
    }
}

impl From<Literal> for Expression {
    fn from(literal: Literal) -> Self {
        Self::Literal(literal)
    }
}

impl From<Variable> for Expression {
    fn from(variable: Variable) -> Self {
        Self::Variable(variable)
    }
}

impl From<NamedNodePattern> for Expression {
    fn from(pattern: NamedNodePattern) -> Self {
        match pattern {
            NamedNodePattern::NamedNode(n) => n.into(),
            NamedNodePattern::Variable(v) => v.into(),
        }
    }
}

impl fmt::Display for Expression {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writer::Expr {
            expression: self,
            aggregates: &[],
        }
        .fmt(f)
    }
}

impl Expression {
    /// Calls `callback` on every variable the expression reads (`EXISTS` patterns
    /// included).
    pub fn on_used_variable<'a>(&'a self, callback: &mut impl FnMut(&'a Variable)) {
        match self {
            Self::NamedNode(_) | Self::Literal(_) => {}
            Self::Variable(v) | Self::Bound(v) => callback(v),
            Self::Or(a, b)
            | Self::And(a, b)
            | Self::Equal(a, b)
            | Self::SameTerm(a, b)
            | Self::Greater(a, b)
            | Self::GreaterOrEqual(a, b)
            | Self::Less(a, b)
            | Self::LessOrEqual(a, b)
            | Self::Add(a, b)
            | Self::Subtract(a, b)
            | Self::Multiply(a, b)
            | Self::Divide(a, b) => {
                a.on_used_variable(callback);
                b.on_used_variable(callback);
            }
            Self::UnaryPlus(e) | Self::UnaryMinus(e) | Self::Not(e) => e.on_used_variable(callback),
            Self::In(a, list) => {
                a.on_used_variable(callback);
                for e in list {
                    e.on_used_variable(callback);
                }
            }
            Self::Exists(pattern) => pattern.on_used_variable(callback),
            Self::If(a, b, c) => {
                a.on_used_variable(callback);
                b.on_used_variable(callback);
                c.on_used_variable(callback);
            }
            Self::Coalesce(list) | Self::FunctionCall(_, list) => {
                for e in list {
                    e.on_used_variable(callback);
                }
            }
        }
    }
}

/// A function of SPARQL 1.1 §17.4, of SPARQL 1.2, or of SEP-0002 (`ADJUST`); or one named
/// by an IRI.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub enum Function {
    Str,
    Lang,
    LangMatches,
    Datatype,
    Iri,
    BNode,
    Rand,
    Abs,
    Ceil,
    Floor,
    Round,
    Concat,
    SubStr,
    StrLen,
    Replace,
    UCase,
    LCase,
    EncodeForUri,
    Contains,
    StrStarts,
    StrEnds,
    StrBefore,
    StrAfter,
    Year,
    Month,
    Day,
    Hours,
    Minutes,
    Seconds,
    Timezone,
    Tz,
    Now,
    Uuid,
    StrUuid,
    Md5,
    Sha1,
    Sha256,
    Sha384,
    Sha512,
    StrLang,
    StrDt,
    IsIri,
    IsBlank,
    IsLiteral,
    IsNumeric,
    Regex,
    /// SPARQL 1.2.
    Triple,
    /// SPARQL 1.2.
    Subject,
    /// SPARQL 1.2.
    Predicate,
    /// SPARQL 1.2.
    Object,
    /// SPARQL 1.2.
    IsTriple,
    /// SPARQL 1.2.
    LangDir,
    /// SPARQL 1.2.
    HasLang,
    /// SPARQL 1.2.
    HasLangDir,
    /// SPARQL 1.2.
    StrLangDir,
    /// SEP-0002.
    Adjust,
    Custom(NamedNode),
}

impl Function {
    /// The keyword the function is called by; `None` for a custom function.
    pub fn keyword(&self) -> Option<&'static str> {
        Some(match self {
            Self::Str => "STR",
            Self::Lang => "LANG",
            Self::LangMatches => "LANGMATCHES",
            Self::Datatype => "DATATYPE",
            Self::Iri => "IRI",
            Self::BNode => "BNODE",
            Self::Rand => "RAND",
            Self::Abs => "ABS",
            Self::Ceil => "CEIL",
            Self::Floor => "FLOOR",
            Self::Round => "ROUND",
            Self::Concat => "CONCAT",
            Self::SubStr => "SUBSTR",
            Self::StrLen => "STRLEN",
            Self::Replace => "REPLACE",
            Self::UCase => "UCASE",
            Self::LCase => "LCASE",
            Self::EncodeForUri => "ENCODE_FOR_URI",
            Self::Contains => "CONTAINS",
            Self::StrStarts => "STRSTARTS",
            Self::StrEnds => "STRENDS",
            Self::StrBefore => "STRBEFORE",
            Self::StrAfter => "STRAFTER",
            Self::Year => "YEAR",
            Self::Month => "MONTH",
            Self::Day => "DAY",
            Self::Hours => "HOURS",
            Self::Minutes => "MINUTES",
            Self::Seconds => "SECONDS",
            Self::Timezone => "TIMEZONE",
            Self::Tz => "TZ",
            Self::Now => "NOW",
            Self::Uuid => "UUID",
            Self::StrUuid => "STRUUID",
            Self::Md5 => "MD5",
            Self::Sha1 => "SHA1",
            Self::Sha256 => "SHA256",
            Self::Sha384 => "SHA384",
            Self::Sha512 => "SHA512",
            Self::StrLang => "STRLANG",
            Self::StrDt => "STRDT",
            Self::IsIri => "isIRI",
            Self::IsBlank => "isBLANK",
            Self::IsLiteral => "isLITERAL",
            Self::IsNumeric => "isNUMERIC",
            Self::Regex => "REGEX",
            Self::Triple => "TRIPLE",
            Self::Subject => "SUBJECT",
            Self::Predicate => "PREDICATE",
            Self::Object => "OBJECT",
            Self::IsTriple => "isTRIPLE",
            Self::LangDir => "LANGDIR",
            Self::HasLang => "hasLANG",
            Self::HasLangDir => "hasLANGDIR",
            Self::StrLangDir => "STRLANGDIR",
            Self::Adjust => "ADJUST",
            Self::Custom(_) => return None,
        })
    }
}

impl fmt::Display for Function {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Custom(iri) => iri.fmt(f),
            other => f.write_str(other.keyword().unwrap_or_default()),
        }
    }
}

/// A graph pattern of the algebra.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub enum GraphPattern {
    /// A basic graph pattern.
    Bgp {
        patterns: Vec<TriplePattern>,
    },
    /// A property path between two terms.
    Path {
        subject: TermPattern,
        path: PropertyPathExpression,
        object: TermPattern,
    },
    Join {
        left: Box<Self>,
        right: Box<Self>,
    },
    LeftJoin {
        left: Box<Self>,
        right: Box<Self>,
        expression: Option<Expression>,
    },
    /// `LATERAL` (SEP-0006): the right side evaluated for each solution of the left.
    Lateral {
        left: Box<Self>,
        right: Box<Self>,
    },
    Filter {
        expr: Expression,
        inner: Box<Self>,
    },
    Union {
        left: Box<Self>,
        right: Box<Self>,
    },
    Graph {
        name: NamedNodePattern,
        inner: Box<Self>,
    },
    Extend {
        inner: Box<Self>,
        variable: Variable,
        expression: Expression,
    },
    Minus {
        left: Box<Self>,
        right: Box<Self>,
    },
    Values {
        variables: Vec<Variable>,
        bindings: Vec<Vec<Option<GroundTerm>>>,
    },
    OrderBy {
        inner: Box<Self>,
        expression: Vec<OrderExpression>,
    },
    Project {
        inner: Box<Self>,
        variables: Vec<Variable>,
    },
    Distinct {
        inner: Box<Self>,
    },
    Reduced {
        inner: Box<Self>,
    },
    Slice {
        inner: Box<Self>,
        start: usize,
        length: Option<usize>,
    },
    Group {
        inner: Box<Self>,
        variables: Vec<Variable>,
        aggregates: Vec<(Variable, AggregateExpression)>,
    },
    Service {
        name: NamedNodePattern,
        inner: Box<Self>,
        silent: bool,
    },
}

impl Default for GraphPattern {
    /// The empty basic graph pattern: one solution, binding nothing.
    fn default() -> Self {
        Self::Bgp {
            patterns: Vec::new(),
        }
    }
}

/// Prints the content of a group: `{ ` + this + ` }` parses back to the pattern.
impl fmt::Display for GraphPattern {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writer::fmt_group(self, f)
    }
}

impl GraphPattern {
    /// Calls `callback` on each variable in scope (SPARQL 1.1 §18.2.1), possibly more than
    /// once.
    pub fn on_in_scope_variable<'a>(&'a self, mut callback: impl FnMut(&'a Variable)) {
        self.in_scope(&mut callback);
    }

    fn in_scope<'a>(&'a self, callback: &mut impl FnMut(&'a Variable)) {
        match self {
            Self::Bgp { patterns } => {
                for pattern in patterns {
                    triple_pattern_variables(pattern, callback);
                }
            }
            Self::Path {
                subject, object, ..
            } => {
                term_pattern_variables(subject, callback);
                term_pattern_variables(object, callback);
            }
            Self::Join { left, right }
            | Self::LeftJoin { left, right, .. }
            | Self::Lateral { left, right }
            | Self::Union { left, right } => {
                left.in_scope(callback);
                right.in_scope(callback);
            }
            Self::Graph { name, inner } => {
                if let NamedNodePattern::Variable(v) = name {
                    callback(v);
                }
                inner.in_scope(callback);
            }
            Self::Extend {
                inner, variable, ..
            } => {
                callback(variable);
                inner.in_scope(callback);
            }
            Self::Minus { left, .. } => left.in_scope(callback),
            Self::Group {
                variables,
                aggregates,
                ..
            } => {
                for v in variables {
                    callback(v);
                }
                for (v, _) in aggregates {
                    callback(v);
                }
            }
            Self::Values { variables, .. } | Self::Project { variables, .. } => {
                for v in variables {
                    callback(v);
                }
            }
            Self::Service { inner, .. }
            | Self::Filter { inner, .. }
            | Self::OrderBy { inner, .. }
            | Self::Distinct { inner }
            | Self::Reduced { inner }
            | Self::Slice { inner, .. } => inner.in_scope(callback),
        }
    }

    /// Calls `callback` on every variable the pattern mentions anywhere, possibly more than
    /// once.
    pub fn on_used_variable<'a>(&'a self, callback: &mut impl FnMut(&'a Variable)) {
        match self {
            Self::Bgp { .. } | Self::Path { .. } | Self::Values { .. } => self.in_scope(callback),
            Self::Join { left, right }
            | Self::Lateral { left, right }
            | Self::Union { left, right }
            | Self::Minus { left, right } => {
                left.on_used_variable(callback);
                right.on_used_variable(callback);
            }
            Self::LeftJoin {
                left,
                right,
                expression,
            } => {
                left.on_used_variable(callback);
                right.on_used_variable(callback);
                if let Some(e) = expression {
                    e.on_used_variable(callback);
                }
            }
            Self::Filter { expr, inner } => {
                expr.on_used_variable(callback);
                inner.on_used_variable(callback);
            }
            Self::Graph { name, inner } | Self::Service { name, inner, .. } => {
                if let NamedNodePattern::Variable(v) = name {
                    callback(v);
                }
                inner.on_used_variable(callback);
            }
            Self::Extend {
                inner,
                variable,
                expression,
            } => {
                callback(variable);
                expression.on_used_variable(callback);
                inner.on_used_variable(callback);
            }
            Self::OrderBy { inner, expression } => {
                for e in expression {
                    e.expression().on_used_variable(callback);
                }
                inner.on_used_variable(callback);
            }
            Self::Project { inner, variables } => {
                for v in variables {
                    callback(v);
                }
                inner.on_used_variable(callback);
            }
            Self::Group {
                inner,
                variables,
                aggregates,
            } => {
                for v in variables {
                    callback(v);
                }
                for (v, aggregate) in aggregates {
                    callback(v);
                    if let AggregateExpression::FunctionCall { expr, .. } = aggregate {
                        expr.on_used_variable(callback);
                    }
                }
                inner.on_used_variable(callback);
            }
            Self::Distinct { inner } | Self::Reduced { inner } | Self::Slice { inner, .. } => {
                inner.on_used_variable(callback);
            }
        }
    }
}

fn triple_pattern_variables<'a>(
    pattern: &'a TriplePattern,
    callback: &mut impl FnMut(&'a Variable),
) {
    term_pattern_variables(&pattern.subject, callback);
    if let NamedNodePattern::Variable(v) = &pattern.predicate {
        callback(v);
    }
    term_pattern_variables(&pattern.object, callback);
}

fn term_pattern_variables<'a>(term: &'a TermPattern, callback: &mut impl FnMut(&'a Variable)) {
    match term {
        TermPattern::Variable(v) => callback(v),
        TermPattern::Triple(t) => triple_pattern_variables(t, callback),
        TermPattern::NamedNode(_) | TermPattern::BlankNode(_) | TermPattern::Literal(_) => {}
    }
}

/// An aggregate (SPARQL 1.1 §18.5).
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub enum AggregateExpression {
    /// `COUNT(*)`.
    CountSolutions { distinct: bool },
    FunctionCall {
        name: AggregateFunction,
        expr: Expression,
        distinct: bool,
    },
}

impl fmt::Display for AggregateExpression {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writer::fmt_aggregate(self, f)
    }
}

/// The set function of an aggregate.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub enum AggregateFunction {
    Count,
    Sum,
    Avg,
    Min,
    Max,
    GroupConcat { separator: Option<String> },
    Sample,
    Custom(NamedNode),
}

/// An `ORDER BY` key.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub enum OrderExpression {
    Asc(Expression),
    Desc(Expression),
}

impl OrderExpression {
    pub fn expression(&self) -> &Expression {
        match self {
            Self::Asc(e) | Self::Desc(e) => e,
        }
    }
}

impl fmt::Display for OrderExpression {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writer::fmt_order(self, &[], f)
    }
}

/// The dataset of a query (`FROM`, `FROM NAMED`) or an update (`USING`, `USING NAMED`,
/// `WITH`).
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct QueryDataset {
    /// The graphs merged into the default graph.
    pub default: Vec<NamedNode>,
    /// The named graphs; `None`: those of the store (only `WITH` leaves them so).
    pub named: Option<Vec<NamedNode>>,
}

impl fmt::Display for QueryDataset {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for g in &self.default {
            write!(f, " FROM {g}")?;
        }
        for g in self.named.iter().flatten() {
            write!(f, " FROM NAMED {g}")?;
        }
        Ok(())
    }
}

/// The graphs `CLEAR` and `DROP` act on.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub enum GraphTarget {
    NamedNode(NamedNode),
    DefaultGraph,
    NamedGraphs,
    AllGraphs,
}

impl From<NamedNode> for GraphTarget {
    fn from(node: NamedNode) -> Self {
        Self::NamedNode(node)
    }
}

impl fmt::Display for GraphTarget {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NamedNode(n) => write!(f, "GRAPH {n}"),
            Self::DefaultGraph => f.write_str("DEFAULT"),
            Self::NamedGraphs => f.write_str("NAMED"),
            Self::AllGraphs => f.write_str("ALL"),
        }
    }
}
