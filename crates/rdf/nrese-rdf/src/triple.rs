//! Triples and quads, owned and borrowed. They print as N-Triples / N-Quads statements
//! without the final ` .`.

use std::fmt;

use crate::term::{
    GraphName, GraphNameRef, NamedNode, NamedNodeRef, NamedOrBlankNode, NamedOrBlankNodeRef, Term,
    TermRef,
};

/// An RDF triple (RDF 1.1 Concepts §3.1).
#[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Debug)]
pub struct Triple {
    pub subject: NamedOrBlankNode,
    pub predicate: NamedNode,
    pub object: Term,
}

/// A borrowed [`Triple`].
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Debug)]
pub struct TripleRef<'a> {
    pub subject: NamedOrBlankNodeRef<'a>,
    pub predicate: NamedNodeRef<'a>,
    pub object: TermRef<'a>,
}

/// A triple in a graph of a dataset (RDF 1.1 Concepts §4).
#[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Debug)]
pub struct Quad {
    pub subject: NamedOrBlankNode,
    pub predicate: NamedNode,
    pub object: Term,
    pub graph_name: GraphName,
}

/// A borrowed [`Quad`].
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Debug)]
pub struct QuadRef<'a> {
    pub subject: NamedOrBlankNodeRef<'a>,
    pub predicate: NamedNodeRef<'a>,
    pub object: TermRef<'a>,
    pub graph_name: GraphNameRef<'a>,
}

impl Triple {
    pub fn new(
        subject: impl Into<NamedOrBlankNode>,
        predicate: impl Into<NamedNode>,
        object: impl Into<Term>,
    ) -> Self {
        Self {
            subject: subject.into(),
            predicate: predicate.into(),
            object: object.into(),
        }
    }

    pub fn in_graph(self, graph_name: impl Into<GraphName>) -> Quad {
        Quad {
            subject: self.subject,
            predicate: self.predicate,
            object: self.object,
            graph_name: graph_name.into(),
        }
    }

    pub fn as_ref(&self) -> TripleRef<'_> {
        TripleRef {
            subject: self.subject.as_ref(),
            predicate: self.predicate.as_ref(),
            object: self.object.as_ref(),
        }
    }
}

impl<'a> TripleRef<'a> {
    pub fn new(
        subject: impl Into<NamedOrBlankNodeRef<'a>>,
        predicate: impl Into<NamedNodeRef<'a>>,
        object: impl Into<TermRef<'a>>,
    ) -> Self {
        Self {
            subject: subject.into(),
            predicate: predicate.into(),
            object: object.into(),
        }
    }

    pub fn in_graph(self, graph_name: impl Into<GraphNameRef<'a>>) -> QuadRef<'a> {
        QuadRef {
            subject: self.subject,
            predicate: self.predicate,
            object: self.object,
            graph_name: graph_name.into(),
        }
    }

    pub fn into_owned(self) -> Triple {
        Triple {
            subject: self.subject.into_owned(),
            predicate: self.predicate.into_owned(),
            object: self.object.into_owned(),
        }
    }
}

impl Quad {
    pub fn new(
        subject: impl Into<NamedOrBlankNode>,
        predicate: impl Into<NamedNode>,
        object: impl Into<Term>,
        graph_name: impl Into<GraphName>,
    ) -> Self {
        Self {
            subject: subject.into(),
            predicate: predicate.into(),
            object: object.into(),
            graph_name: graph_name.into(),
        }
    }

    pub fn as_ref(&self) -> QuadRef<'_> {
        QuadRef {
            subject: self.subject.as_ref(),
            predicate: self.predicate.as_ref(),
            object: self.object.as_ref(),
            graph_name: self.graph_name.as_ref(),
        }
    }
}

impl<'a> QuadRef<'a> {
    pub fn new(
        subject: impl Into<NamedOrBlankNodeRef<'a>>,
        predicate: impl Into<NamedNodeRef<'a>>,
        object: impl Into<TermRef<'a>>,
        graph_name: impl Into<GraphNameRef<'a>>,
    ) -> Self {
        Self {
            subject: subject.into(),
            predicate: predicate.into(),
            object: object.into(),
            graph_name: graph_name.into(),
        }
    }

    pub fn into_owned(self) -> Quad {
        Quad {
            subject: self.subject.into_owned(),
            predicate: self.predicate.into_owned(),
            object: self.object.into_owned(),
            graph_name: self.graph_name.into_owned(),
        }
    }
}

impl From<Quad> for Triple {
    fn from(quad: Quad) -> Self {
        Self {
            subject: quad.subject,
            predicate: quad.predicate,
            object: quad.object,
        }
    }
}

impl<'a> From<QuadRef<'a>> for TripleRef<'a> {
    fn from(quad: QuadRef<'a>) -> Self {
        Self {
            subject: quad.subject,
            predicate: quad.predicate,
            object: quad.object,
        }
    }
}

impl<'a> From<&'a Triple> for TripleRef<'a> {
    fn from(triple: &'a Triple) -> Self {
        triple.as_ref()
    }
}

impl From<TripleRef<'_>> for Triple {
    fn from(triple: TripleRef<'_>) -> Self {
        triple.into_owned()
    }
}

impl<'a> From<&'a Quad> for QuadRef<'a> {
    fn from(quad: &'a Quad) -> Self {
        quad.as_ref()
    }
}

impl From<QuadRef<'_>> for Quad {
    fn from(quad: QuadRef<'_>) -> Self {
        quad.into_owned()
    }
}

impl fmt::Display for TripleRef<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} {} {}", self.subject, self.predicate, self.object)
    }
}

impl fmt::Display for Triple {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.as_ref().fmt(f)
    }
}

impl fmt::Display for QuadRef<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} {} {}", self.subject, self.predicate, self.object)?;
        if !self.graph_name.is_default_graph() {
            write!(f, " {}", self.graph_name)?;
        }
        Ok(())
    }
}

impl fmt::Display for Quad {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.as_ref().fmt(f)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::term::{BlankNode, Literal};

    #[test]
    fn statements_print_as_n_quads() {
        let triple = Triple::new(
            BlankNode::new_unchecked("s"),
            NamedNode::new_unchecked("http://e/p"),
            Literal::new_simple_literal("o"),
        );
        assert_eq!(triple.to_string(), "_:s <http://e/p> \"o\"");
        let quad = triple
            .clone()
            .in_graph(NamedNode::new_unchecked("http://e/g"));
        assert_eq!(quad.to_string(), "_:s <http://e/p> \"o\" <http://e/g>");
        assert_eq!(Triple::from(quad.clone()), triple);
        assert_eq!(quad.as_ref().into_owned(), quad);
        assert_eq!(
            triple.in_graph(GraphName::DefaultGraph).to_string(),
            "_:s <http://e/p> \"o\""
        );
    }
}
