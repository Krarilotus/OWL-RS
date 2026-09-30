//! Compiled shapes: what [`compile`](crate::compile) produces and the validator runs.
//!
//! Everything is term ids of the repository's dictionary. Parameters that need more than
//! an id (regular expressions, bounds, language ranges) are prepared here, once.

use std::collections::{BTreeSet, HashMap};

use nrese_engine::TermId;
use nrese_sparql::value::Value;
use regex::Regex;

/// The namespace of the SHACL vocabulary.
pub const SH: &str = "http://www.w3.org/ns/shacl#";

/// A compiled shapes graph.
#[derive(Debug, Default)]
pub struct Shapes {
    pub(crate) shapes: Vec<Shape>,
    pub(crate) index: HashMap<TermId, ShapeRef>,
}

impl Shapes {
    /// The number of shapes, nested ones included.
    pub fn len(&self) -> usize {
        self.shapes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.shapes.is_empty()
    }

    /// The shapes validation starts from: active ones with a target.
    pub(crate) fn targeted(&self) -> impl Iterator<Item = ShapeRef> + '_ {
        (0..self.shapes.len()).filter(|&shape| {
            !self.shapes[shape].deactivated && !self.shapes[shape].targets.is_empty()
        })
    }

    /// The shape compiled from `node`, if it is one.
    pub fn shape_of(&self, node: TermId) -> Option<ShapeRef> {
        self.index.get(&node).copied()
    }
}

/// A shape's position in [`Shapes`].
pub type ShapeRef = usize;

#[derive(Debug)]
pub(crate) struct Shape {
    /// The shape's node in the shapes graph.
    pub(crate) node: TermId,
    /// `Some` for a property shape.
    pub(crate) path: Option<Path>,
    pub(crate) targets: Vec<Target>,
    pub(crate) constraints: Vec<Constraint>,
    pub(crate) severity: Severity,
    /// The `sh:message` literals.
    pub(crate) messages: Vec<TermId>,
    pub(crate) deactivated: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Severity {
    Info,
    Warning,
    Violation,
    /// Another IRI given as `sh:severity`.
    Other(TermId),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Target {
    Node(TermId),
    /// `sh:targetClass`, or the shape itself when it is also a class.
    Class(TermId),
    SubjectsOf(TermId),
    ObjectsOf(TermId),
}

/// A SHACL property path.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Path {
    Predicate(TermId),
    Inverse(Box<Path>),
    Sequence(Vec<Path>),
    Alternative(Vec<Path>),
    ZeroOrMore(Box<Path>),
    OneOrMore(Box<Path>),
    ZeroOrOne(Box<Path>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NodeKind {
    Iri,
    BlankNode,
    Literal,
    BlankNodeOrIri,
    BlankNodeOrLiteral,
    IriOrLiteral,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Bound {
    MinExclusive,
    MinInclusive,
    MaxExclusive,
    MaxInclusive,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Logical {
    And,
    Or,
    Xone,
}

#[derive(Debug)]
pub(crate) enum Constraint {
    Class(TermId),
    /// The datatype's IRI.
    Datatype(String),
    NodeKind(NodeKind),
    MinCount(u64),
    MaxCount(u64),
    Range(Bound, Value),
    MinLength(u64),
    MaxLength(u64),
    Pattern(Regex),
    /// Language ranges, for `langMatches`.
    LanguageIn(Vec<String>),
    UniqueLang,
    Equals(TermId),
    Disjoint(TermId),
    LessThan(TermId),
    LessThanOrEquals(TermId),
    Not(ShapeRef),
    Logical(Logical, Vec<ShapeRef>),
    Node(ShapeRef),
    Property(ShapeRef),
    Qualified {
        shape: ShapeRef,
        min: Option<u64>,
        max: Option<u64>,
        /// The sibling shapes a value must not conform to
        /// (`sh:qualifiedValueShapesDisjoint`); empty otherwise.
        siblings: Vec<ShapeRef>,
    },
    /// The predicates a closed shape allows.
    Closed(BTreeSet<TermId>),
    HasValue(TermId),
    In(BTreeSet<TermId>),
}

/// A constraint component, as `sh:sourceConstraintComponent` names it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Component {
    Class,
    Datatype,
    NodeKind,
    MinCount,
    MaxCount,
    MinExclusive,
    MinInclusive,
    MaxExclusive,
    MaxInclusive,
    MinLength,
    MaxLength,
    Pattern,
    LanguageIn,
    UniqueLang,
    Equals,
    Disjoint,
    LessThan,
    LessThanOrEquals,
    Not,
    And,
    Or,
    Xone,
    Node,
    QualifiedMinCount,
    QualifiedMaxCount,
    Closed,
    HasValue,
    In,
}

impl Component {
    /// The component's local name in the SHACL namespace.
    pub const fn local_name(self) -> &'static str {
        match self {
            Self::Class => "ClassConstraintComponent",
            Self::Datatype => "DatatypeConstraintComponent",
            Self::NodeKind => "NodeKindConstraintComponent",
            Self::MinCount => "MinCountConstraintComponent",
            Self::MaxCount => "MaxCountConstraintComponent",
            Self::MinExclusive => "MinExclusiveConstraintComponent",
            Self::MinInclusive => "MinInclusiveConstraintComponent",
            Self::MaxExclusive => "MaxExclusiveConstraintComponent",
            Self::MaxInclusive => "MaxInclusiveConstraintComponent",
            Self::MinLength => "MinLengthConstraintComponent",
            Self::MaxLength => "MaxLengthConstraintComponent",
            Self::Pattern => "PatternConstraintComponent",
            Self::LanguageIn => "LanguageInConstraintComponent",
            Self::UniqueLang => "UniqueLangConstraintComponent",
            Self::Equals => "EqualsConstraintComponent",
            Self::Disjoint => "DisjointConstraintComponent",
            Self::LessThan => "LessThanConstraintComponent",
            Self::LessThanOrEquals => "LessThanOrEqualsConstraintComponent",
            Self::Not => "NotConstraintComponent",
            Self::And => "AndConstraintComponent",
            Self::Or => "OrConstraintComponent",
            Self::Xone => "XoneConstraintComponent",
            Self::Node => "NodeConstraintComponent",
            Self::QualifiedMinCount => "QualifiedMinCountConstraintComponent",
            Self::QualifiedMaxCount => "QualifiedMaxCountConstraintComponent",
            Self::Closed => "ClosedConstraintComponent",
            Self::HasValue => "HasValueConstraintComponent",
            Self::In => "InConstraintComponent",
        }
    }

    /// The component's IRI.
    pub fn iri(self) -> String {
        format!("{SH}{}", self.local_name())
    }
}
