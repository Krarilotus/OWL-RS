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
    /// The shapes graph's name, if it is a named graph: `$shapesGraph` in SHACL-SPARQL.
    pub(crate) graph: Option<TermId>,
    /// The `sh:entailment` values of the shapes graph (SHACL §1.5): regimes the data must be
    /// validated under. None is supported, so each is a failure of every validation.
    pub(crate) entailments: Vec<TermId>,
}

impl Shapes {
    /// The number of shapes, nested ones included.
    pub fn len(&self) -> usize {
        self.shapes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.shapes.is_empty()
    }

    /// Whether validating against these shapes can report nothing at all: no shape, and no
    /// `sh:entailment`, which is a failure of every validation even without shapes.
    pub fn is_vacuous(&self) -> bool {
        self.shapes.is_empty() && self.entailments.is_empty()
    }

    /// Whether any shape, nested ones included, has a SHACL-SPARQL constraint (`sh:sparql`
    /// or a SPARQL-based constraint component): validating it evaluates queries over the
    /// view's [`nrese_sparql::ReadView::evaluation_snapshot`], not through its quad reads,
    /// the only read of this crate that does. Deactivated shapes count: it tells what the
    /// shapes graph declares, so `true` where no query may run is the safe side.
    pub fn uses_sparql(&self) -> bool {
        self.shapes.iter().any(|shape| {
            shape
                .constraints
                .iter()
                .any(|constraint| matches!(constraint, Constraint::Sparql(_)))
        })
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
    Nested(NestedConstraint),
    /// The predicates a closed shape allows.
    Closed(BTreeSet<TermId>),
    HasValue(TermId),
    In(BTreeSet<TermId>),
    /// SHACL-SPARQL: an `sh:sparql` constraint, or a SPARQL-based constraint component's
    /// validator with the shape's parameter values.
    Sparql(Box<SparqlCheck>),
}

#[derive(Debug, Clone, Copy)]
pub(crate) enum Nesting {
    Not,
    Logical(Logical),
    Node,
    Property,
    Qualified { min: Option<u64>, max: Option<u64> },
}

#[derive(Debug)]
pub(crate) struct NestedConstraint {
    pub(crate) kind: Nesting,
    /// The single shape of a direct reference, or the primary qualified value shape.
    /// Unused for a logical list, whose references all live in `others`.
    first: ShapeRef,
    /// The logical list members, or the disjoint qualified siblings.
    others: Vec<ShapeRef>,
}

impl NestedConstraint {
    pub(crate) fn single(kind: Nesting, shape: ShapeRef) -> Self {
        Self {
            kind,
            first: shape,
            others: Vec::new(),
        }
    }

    pub(crate) fn logical(kind: Logical, members: Vec<ShapeRef>) -> Self {
        Self {
            kind: Nesting::Logical(kind),
            first: 0,
            others: members,
        }
    }

    pub(crate) fn qualified(
        shape: ShapeRef,
        min: Option<u64>,
        max: Option<u64>,
        siblings: Vec<ShapeRef>,
    ) -> Self {
        Self {
            kind: Nesting::Qualified { min, max },
            first: shape,
            others: siblings,
        }
    }

    /// References in validation order: the primary qualified shape before its siblings.
    pub(crate) fn references(&self) -> impl Iterator<Item = ShapeRef> + '_ {
        self.has_first()
            .then_some(self.first)
            .into_iter()
            .chain(self.others.iter().copied())
    }

    /// Random access in the same order, for a suspended validation.
    pub(crate) fn reference(&self, member: usize) -> Option<ShapeRef> {
        if self.has_first() {
            if member == 0 {
                return Some(self.first);
            }
            self.others.get(member - 1).copied()
        } else {
            self.others.get(member).copied()
        }
    }

    /// References in creation order: the compiler encounters qualified siblings first.
    pub(crate) fn creation_reference(&mut self, member: usize) -> Option<&mut ShapeRef> {
        if member == self.others.len() && self.has_first() {
            Some(&mut self.first)
        } else {
            self.others.get_mut(member)
        }
    }

    fn has_first(&self) -> bool {
        !matches!(self.kind, Nesting::Logical(_))
    }

    /// Whether the conservative applicability check depends on the referenced shapes.
    pub(crate) fn depends_on_members(&self) -> bool {
        match self.kind {
            Nesting::Node | Nesting::Property | Nesting::Logical(_) => true,
            Nesting::Not | Nesting::Qualified { .. } => false,
        }
    }
}

/// A SPARQL query that checks a focus node (SHACL §5, §6).
#[derive(Debug)]
pub(crate) struct SparqlCheck {
    /// `sh:sourceConstraint`: the `sh:sparql` value. None for a SPARQL-based constraint
    /// component's: its validator is shared by every shape that uses the component, and no
    /// constraint of one.
    pub(crate) source: Option<TermId>,
    /// `sh:SPARQLConstraintComponent`, or the custom component.
    pub(crate) component: Component,
    /// The query, with its prefixes and `$PATH` put in.
    pub(crate) query: nrese_sparql_syntax::Query,
    /// An ASK validator: run per value node, with `$value`; a value fails when it answers
    /// false. Otherwise a SELECT: each solution is a result.
    pub(crate) ask: bool,
    /// The component's parameters: the variable each is pre-bound as, and the shape's values
    /// for it. The component constrains the shape once per combination of these values
    /// (SHACL §6.2), each run in turn ([`SparqlCheck::each_combination`]); they aren't
    /// multiplied out, which would grow as a power of the shapes graph.
    pub(crate) parameters: Vec<(nrese_rdf::Variable, Vec<TermId>)>,
    /// The `sh:message`s of the constraint or validator (else the component's).
    pub(crate) messages: Vec<TermId>,
}

impl SparqlCheck {
    /// Calls `each` with every combination of the parameters' values, one value per
    /// parameter, the first parameter's changing slowest, until `each` breaks. Without
    /// parameters (`sh:sparql`) that is one combination, the empty one; a parameter without
    /// values leaves none.
    pub(crate) fn each_combination(
        &self,
        mut each: impl FnMut(&[(nrese_rdf::Variable, TermId)]) -> std::ops::ControlFlow<()>,
    ) {
        if self.parameters.iter().any(|(_, values)| values.is_empty()) {
            return;
        }
        let mut at = vec![0; self.parameters.len()];
        let mut chosen: Vec<(nrese_rdf::Variable, TermId)> = self
            .parameters
            .iter()
            .map(|(variable, values)| (variable.clone(), values[0]))
            .collect();
        loop {
            if each(&chosen).is_break() {
                return;
            }
            // The next combination: the last parameter that has a next value takes it, and
            // every parameter after it starts over.
            let mut parameter = self.parameters.len();
            loop {
                if parameter == 0 {
                    return;
                }
                parameter -= 1;
                let values = &self.parameters[parameter].1;
                at[parameter] += 1;
                if at[parameter] < values.len() {
                    chosen[parameter].1 = values[at[parameter]];
                    break;
                }
                at[parameter] = 0;
                chosen[parameter].1 = values[0];
            }
        }
    }
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
    /// `sh:sparql` constraints.
    Sparql,
    /// A SPARQL-based constraint component of the shapes graph: its node.
    Custom(TermId),
}

impl Component {
    /// The component's local name in the SHACL namespace (a custom component's IRI is the
    /// shapes graph's: [`Self::iri`] can't name it).
    pub const fn local_name(self) -> &'static str {
        match self {
            Self::Sparql => "SPARQLConstraintComponent",
            Self::Custom(_) => "ConstraintComponent",
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
