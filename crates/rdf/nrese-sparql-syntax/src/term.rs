//! The terms of patterns: what may stand in a triple pattern, a quad of an update, or a
//! `VALUES` row. Each prints in SPARQL syntax.

use std::fmt;

use nrese_rdf::{BlankNode, Literal, NamedNode, NamedOrBlankNode, Term, Triple, Variable};

use crate::writer::{fmt_iri, fmt_literal, fmt_term, fmt_variable};

/// A term without blank nodes or variables: a `VALUES` value, or part of `DELETE DATA`.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub enum GroundTerm {
    NamedNode(NamedNode),
    Literal(Literal),
    /// A triple term (SPARQL 1.2).
    Triple(Box<GroundTriple>),
}

impl fmt::Display for GroundTerm {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NamedNode(n) => fmt_iri(n, f),
            Self::Literal(l) => fmt_literal(l, f),
            Self::Triple(t) => write!(f, "<<( {t} )>>"),
        }
    }
}

impl From<NamedNode> for GroundTerm {
    fn from(node: NamedNode) -> Self {
        Self::NamedNode(node)
    }
}

impl From<Literal> for GroundTerm {
    fn from(literal: Literal) -> Self {
        Self::Literal(literal)
    }
}

impl From<GroundTriple> for GroundTerm {
    fn from(triple: GroundTriple) -> Self {
        Self::Triple(Box::new(triple))
    }
}

impl From<GroundTerm> for Term {
    fn from(term: GroundTerm) -> Self {
        match term {
            GroundTerm::NamedNode(n) => n.into(),
            GroundTerm::Literal(l) => l.into(),
            GroundTerm::Triple(t) => Triple::from(*t).into(),
        }
    }
}

impl TryFrom<Term> for GroundTerm {
    type Error = ();

    /// Fails on blank nodes.
    fn try_from(term: Term) -> Result<Self, ()> {
        match term {
            Term::NamedNode(n) => Ok(n.into()),
            Term::Literal(l) => Ok(l.into()),
            Term::BlankNode(_) => Err(()),
            Term::Triple(t) => Ok(GroundTriple::try_from(*t)?.into()),
        }
    }
}

/// A triple without blank nodes.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct GroundTriple {
    pub subject: NamedNode,
    pub predicate: NamedNode,
    pub object: GroundTerm,
}

impl fmt::Display for GroundTriple {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} {} {}", self.subject, self.predicate, self.object)
    }
}

impl From<GroundTriple> for Triple {
    fn from(triple: GroundTriple) -> Self {
        Self::new(triple.subject, triple.predicate, Term::from(triple.object))
    }
}

impl TryFrom<Triple> for GroundTriple {
    type Error = ();

    fn try_from(triple: Triple) -> Result<Self, ()> {
        Ok(Self {
            subject: match triple.subject {
                NamedOrBlankNode::NamedNode(n) => n,
                NamedOrBlankNode::BlankNode(_) => return Err(()),
            },
            predicate: triple.predicate,
            object: triple.object.try_into()?,
        })
    }
}

/// The graph of a quad in an update: a named graph or the default graph.
#[derive(Clone, PartialEq, Eq, Hash, Debug, Default)]
pub enum GraphName {
    NamedNode(NamedNode),
    #[default]
    DefaultGraph,
}

impl fmt::Display for GraphName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NamedNode(n) => n.fmt(f),
            Self::DefaultGraph => f.write_str("DEFAULT"),
        }
    }
}

impl From<NamedNode> for GraphName {
    fn from(node: NamedNode) -> Self {
        Self::NamedNode(node)
    }
}

/// A quad of `INSERT DATA`: blank nodes allowed, variables not.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct Quad {
    pub subject: NamedOrBlankNode,
    pub predicate: NamedNode,
    pub object: Term,
    pub graph_name: GraphName,
}

impl fmt::Display for Quad {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let triple = format_args!("{} {} ", self.subject, self.predicate);
        match &self.graph_name {
            GraphName::NamedNode(g) => {
                write!(f, "GRAPH {g} {{ {triple}")?;
                fmt_term(&self.object, f)?;
                f.write_str(" }")
            }
            GraphName::DefaultGraph => {
                write!(f, "{triple}")?;
                fmt_term(&self.object, f)
            }
        }
    }
}

impl TryFrom<QuadPattern> for Quad {
    type Error = ();

    /// Fails on variables.
    fn try_from(quad: QuadPattern) -> Result<Self, ()> {
        Ok(Self {
            subject: match quad.subject {
                TermPattern::NamedNode(n) => n.into(),
                TermPattern::BlankNode(b) => b.into(),
                _ => return Err(()),
            },
            predicate: match quad.predicate {
                NamedNodePattern::NamedNode(n) => n,
                NamedNodePattern::Variable(_) => return Err(()),
            },
            object: quad.object.try_into()?,
            graph_name: match quad.graph_name {
                GraphNamePattern::NamedNode(n) => n.into(),
                GraphNamePattern::DefaultGraph => GraphName::DefaultGraph,
                GraphNamePattern::Variable(_) => return Err(()),
            },
        })
    }
}

/// A quad of `DELETE DATA`: neither blank nodes nor variables.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct GroundQuad {
    pub subject: NamedNode,
    pub predicate: NamedNode,
    pub object: GroundTerm,
    pub graph_name: GraphName,
}

impl fmt::Display for GroundQuad {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.graph_name {
            GraphName::NamedNode(g) => write!(
                f,
                "GRAPH {g} {{ {} {} {} }}",
                self.subject, self.predicate, self.object
            ),
            GraphName::DefaultGraph => {
                write!(f, "{} {} {}", self.subject, self.predicate, self.object)
            }
        }
    }
}

impl TryFrom<Quad> for GroundQuad {
    type Error = ();

    /// Fails on blank nodes.
    fn try_from(quad: Quad) -> Result<Self, ()> {
        Ok(Self {
            subject: match quad.subject {
                NamedOrBlankNode::NamedNode(n) => n,
                NamedOrBlankNode::BlankNode(_) => return Err(()),
            },
            predicate: quad.predicate,
            object: quad.object.try_into()?,
            graph_name: quad.graph_name,
        })
    }
}

/// An IRI or a variable: a predicate, or the name in `GRAPH` and `SERVICE`.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub enum NamedNodePattern {
    NamedNode(NamedNode),
    Variable(Variable),
}

impl fmt::Display for NamedNodePattern {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NamedNode(n) => fmt_iri(n, f),
            Self::Variable(v) => fmt_variable(v, f),
        }
    }
}

impl From<NamedNode> for NamedNodePattern {
    fn from(node: NamedNode) -> Self {
        Self::NamedNode(node)
    }
}

impl From<Variable> for NamedNodePattern {
    fn from(variable: Variable) -> Self {
        Self::Variable(variable)
    }
}

/// What may stand as subject or object of a triple pattern.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub enum TermPattern {
    NamedNode(NamedNode),
    BlankNode(BlankNode),
    Literal(Literal),
    /// A triple term (SPARQL 1.2).
    Triple(Box<TriplePattern>),
    Variable(Variable),
}

impl fmt::Display for TermPattern {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NamedNode(n) => fmt_iri(n, f),
            Self::BlankNode(b) => {
                f.write_str("_:")?;
                f.write_str(b.as_str())
            }
            Self::Literal(l) => fmt_literal(l, f),
            Self::Triple(t) => {
                f.write_str("<<( ")?;
                t.fmt(f)?;
                f.write_str(" )>>")
            }
            Self::Variable(v) => fmt_variable(v, f),
        }
    }
}

impl From<NamedNode> for TermPattern {
    fn from(node: NamedNode) -> Self {
        Self::NamedNode(node)
    }
}

impl From<BlankNode> for TermPattern {
    fn from(node: BlankNode) -> Self {
        Self::BlankNode(node)
    }
}

impl From<Literal> for TermPattern {
    fn from(literal: Literal) -> Self {
        Self::Literal(literal)
    }
}

impl From<Variable> for TermPattern {
    fn from(variable: Variable) -> Self {
        Self::Variable(variable)
    }
}

impl From<TriplePattern> for TermPattern {
    fn from(triple: TriplePattern) -> Self {
        Self::Triple(Box::new(triple))
    }
}

impl From<NamedNodePattern> for TermPattern {
    fn from(pattern: NamedNodePattern) -> Self {
        match pattern {
            NamedNodePattern::NamedNode(n) => n.into(),
            NamedNodePattern::Variable(v) => v.into(),
        }
    }
}

impl From<Term> for TermPattern {
    fn from(term: Term) -> Self {
        match term {
            Term::NamedNode(n) => n.into(),
            Term::BlankNode(b) => b.into(),
            Term::Literal(l) => l.into(),
            Term::Triple(t) => TriplePattern::from(*t).into(),
        }
    }
}

impl From<GroundTerm> for TermPattern {
    fn from(term: GroundTerm) -> Self {
        match term {
            GroundTerm::NamedNode(n) => n.into(),
            GroundTerm::Literal(l) => l.into(),
            GroundTerm::Triple(t) => TriplePattern::from(*t).into(),
        }
    }
}

impl TryFrom<TermPattern> for Term {
    type Error = ();

    /// Fails on variables.
    fn try_from(pattern: TermPattern) -> Result<Self, ()> {
        Ok(match pattern {
            TermPattern::NamedNode(n) => n.into(),
            TermPattern::BlankNode(b) => b.into(),
            TermPattern::Literal(l) => l.into(),
            TermPattern::Triple(t) => Triple::try_from(*t)?.into(),
            TermPattern::Variable(_) => return Err(()),
        })
    }
}

/// A term pattern without blank nodes: the subject or object of a `DELETE` template.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub enum GroundTermPattern {
    NamedNode(NamedNode),
    Literal(Literal),
    Variable(Variable),
    /// A triple term (SPARQL 1.2).
    Triple(Box<GroundTriplePattern>),
}

impl fmt::Display for GroundTermPattern {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NamedNode(n) => n.fmt(f),
            Self::Literal(l) => fmt_literal(l, f),
            Self::Variable(v) => v.fmt(f),
            Self::Triple(t) => write!(f, "<<( {t} )>>"),
        }
    }
}

impl From<NamedNode> for GroundTermPattern {
    fn from(node: NamedNode) -> Self {
        Self::NamedNode(node)
    }
}

impl From<Literal> for GroundTermPattern {
    fn from(literal: Literal) -> Self {
        Self::Literal(literal)
    }
}

impl From<Variable> for GroundTermPattern {
    fn from(variable: Variable) -> Self {
        Self::Variable(variable)
    }
}

impl From<GroundTerm> for GroundTermPattern {
    fn from(term: GroundTerm) -> Self {
        match term {
            GroundTerm::NamedNode(n) => n.into(),
            GroundTerm::Literal(l) => l.into(),
            GroundTerm::Triple(t) => Self::Triple(Box::new((*t).into())),
        }
    }
}

impl From<NamedNodePattern> for GroundTermPattern {
    fn from(pattern: NamedNodePattern) -> Self {
        match pattern {
            NamedNodePattern::NamedNode(n) => n.into(),
            NamedNodePattern::Variable(v) => v.into(),
        }
    }
}

impl TryFrom<TermPattern> for GroundTermPattern {
    type Error = ();

    /// Fails on blank nodes.
    fn try_from(pattern: TermPattern) -> Result<Self, ()> {
        Ok(match pattern {
            TermPattern::NamedNode(n) => n.into(),
            TermPattern::BlankNode(_) => return Err(()),
            TermPattern::Literal(l) => l.into(),
            TermPattern::Triple(t) => Self::Triple(Box::new((*t).try_into()?)),
            TermPattern::Variable(v) => v.into(),
        })
    }
}

/// The name of the graph a quad pattern matches or writes in.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub enum GraphNamePattern {
    NamedNode(NamedNode),
    DefaultGraph,
    Variable(Variable),
}

impl fmt::Display for GraphNamePattern {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NamedNode(n) => n.fmt(f),
            Self::DefaultGraph => f.write_str("DEFAULT"),
            Self::Variable(v) => v.fmt(f),
        }
    }
}

impl From<NamedNode> for GraphNamePattern {
    fn from(node: NamedNode) -> Self {
        Self::NamedNode(node)
    }
}

impl From<Variable> for GraphNamePattern {
    fn from(variable: Variable) -> Self {
        Self::Variable(variable)
    }
}

impl From<GraphName> for GraphNamePattern {
    fn from(name: GraphName) -> Self {
        match name {
            GraphName::NamedNode(n) => n.into(),
            GraphName::DefaultGraph => Self::DefaultGraph,
        }
    }
}

impl From<NamedNodePattern> for GraphNamePattern {
    fn from(pattern: NamedNodePattern) -> Self {
        match pattern {
            NamedNodePattern::NamedNode(n) => n.into(),
            NamedNodePattern::Variable(v) => v.into(),
        }
    }
}

/// A triple pattern.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct TriplePattern {
    pub subject: TermPattern,
    pub predicate: NamedNodePattern,
    pub object: TermPattern,
}

impl TriplePattern {
    pub fn new(
        subject: impl Into<TermPattern>,
        predicate: impl Into<NamedNodePattern>,
        object: impl Into<TermPattern>,
    ) -> Self {
        Self {
            subject: subject.into(),
            predicate: predicate.into(),
            object: object.into(),
        }
    }
}

impl fmt::Display for TriplePattern {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.subject.fmt(f)?;
        f.write_str(" ")?;
        self.predicate.fmt(f)?;
        f.write_str(" ")?;
        self.object.fmt(f)
    }
}

impl From<Triple> for TriplePattern {
    fn from(triple: Triple) -> Self {
        Self {
            subject: match triple.subject {
                NamedOrBlankNode::NamedNode(n) => n.into(),
                NamedOrBlankNode::BlankNode(b) => b.into(),
            },
            predicate: triple.predicate.into(),
            object: triple.object.into(),
        }
    }
}

impl From<GroundTriple> for TriplePattern {
    fn from(triple: GroundTriple) -> Self {
        Self {
            subject: triple.subject.into(),
            predicate: triple.predicate.into(),
            object: triple.object.into(),
        }
    }
}

impl TryFrom<TriplePattern> for Triple {
    type Error = ();

    /// Fails on variables, and on a subject that is neither an IRI nor a blank node.
    fn try_from(pattern: TriplePattern) -> Result<Self, ()> {
        Ok(Self::new(
            match pattern.subject {
                TermPattern::NamedNode(n) => NamedOrBlankNode::from(n),
                TermPattern::BlankNode(b) => b.into(),
                _ => return Err(()),
            },
            match pattern.predicate {
                NamedNodePattern::NamedNode(n) => n,
                NamedNodePattern::Variable(_) => return Err(()),
            },
            Term::try_from(pattern.object)?,
        ))
    }
}

/// A triple pattern without blank nodes.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct GroundTriplePattern {
    pub subject: GroundTermPattern,
    pub predicate: NamedNodePattern,
    pub object: GroundTermPattern,
}

impl fmt::Display for GroundTriplePattern {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} {} {}", self.subject, self.predicate, self.object)
    }
}

impl From<GroundTriple> for GroundTriplePattern {
    fn from(triple: GroundTriple) -> Self {
        Self {
            subject: triple.subject.into(),
            predicate: triple.predicate.into(),
            object: triple.object.into(),
        }
    }
}

impl TryFrom<TriplePattern> for GroundTriplePattern {
    type Error = ();

    /// Fails on blank nodes.
    fn try_from(pattern: TriplePattern) -> Result<Self, ()> {
        Ok(Self {
            subject: pattern.subject.try_into()?,
            predicate: pattern.predicate,
            object: pattern.object.try_into()?,
        })
    }
}

/// A triple pattern in a graph: an `INSERT` template's quad.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct QuadPattern {
    pub subject: TermPattern,
    pub predicate: NamedNodePattern,
    pub object: TermPattern,
    pub graph_name: GraphNamePattern,
}

impl QuadPattern {
    pub fn new(
        subject: impl Into<TermPattern>,
        predicate: impl Into<NamedNodePattern>,
        object: impl Into<TermPattern>,
        graph_name: impl Into<GraphNamePattern>,
    ) -> Self {
        Self {
            subject: subject.into(),
            predicate: predicate.into(),
            object: object.into(),
            graph_name: graph_name.into(),
        }
    }
}

impl fmt::Display for QuadPattern {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let triple = format_args!("{} {} {}", self.subject, self.predicate, self.object);
        match &self.graph_name {
            GraphNamePattern::DefaultGraph => write!(f, "{triple}"),
            graph => write!(f, "GRAPH {graph} {{ {triple} }}"),
        }
    }
}

/// A quad pattern without blank nodes: a `DELETE` template's quad.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct GroundQuadPattern {
    pub subject: GroundTermPattern,
    pub predicate: NamedNodePattern,
    pub object: GroundTermPattern,
    pub graph_name: GraphNamePattern,
}

impl fmt::Display for GroundQuadPattern {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let triple = format_args!("{} {} {}", self.subject, self.predicate, self.object);
        match &self.graph_name {
            GraphNamePattern::DefaultGraph => write!(f, "{triple}"),
            graph => write!(f, "GRAPH {graph} {{ {triple} }}"),
        }
    }
}

impl TryFrom<QuadPattern> for GroundQuadPattern {
    type Error = ();

    /// Fails on blank nodes.
    fn try_from(pattern: QuadPattern) -> Result<Self, ()> {
        Ok(Self {
            subject: pattern.subject.try_into()?,
            predicate: pattern.predicate,
            object: pattern.object.try_into()?,
            graph_name: pattern.graph_name,
        })
    }
}
