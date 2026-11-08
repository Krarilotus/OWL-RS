//! Queries and updates: what [`crate::SparqlParser`] returns.

use std::fmt;

use nrese_rdf::{Iri, NamedNode};

use crate::algebra::{GraphPattern, GraphTarget, QueryDataset};
use crate::parser::{SparqlParser, SparqlSyntaxError};
use crate::term::{GraphName, GroundQuad, GroundQuadPattern, Quad, QuadPattern, TriplePattern};
use crate::writer;

/// A SPARQL query: its form, dataset and pattern.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub enum Query {
    Select {
        dataset: Option<QueryDataset>,
        pattern: GraphPattern,
        base_iri: Option<Iri<String>>,
    },
    Construct {
        template: Vec<TriplePattern>,
        dataset: Option<QueryDataset>,
        pattern: GraphPattern,
        base_iri: Option<Iri<String>>,
    },
    Describe {
        dataset: Option<QueryDataset>,
        pattern: GraphPattern,
        base_iri: Option<Iri<String>>,
    },
    Ask {
        dataset: Option<QueryDataset>,
        pattern: GraphPattern,
        base_iri: Option<Iri<String>>,
    },
}

impl Query {
    /// Parses with the default options and an optional base IRI.
    pub fn parse(query: &str, base_iri: Option<&str>) -> Result<Self, SparqlSyntaxError> {
        let mut parser = SparqlParser::new();
        if let Some(base) = base_iri {
            parser = parser.with_base_iri(base)?;
        }
        parser.parse_query(query)
    }

    pub fn dataset(&self) -> Option<&QueryDataset> {
        match self {
            Self::Select { dataset, .. }
            | Self::Construct { dataset, .. }
            | Self::Describe { dataset, .. }
            | Self::Ask { dataset, .. } => dataset.as_ref(),
        }
    }

    pub fn dataset_mut(&mut self) -> Option<&mut QueryDataset> {
        match self {
            Self::Select { dataset, .. }
            | Self::Construct { dataset, .. }
            | Self::Describe { dataset, .. }
            | Self::Ask { dataset, .. } => dataset.as_mut(),
        }
    }

    pub fn pattern(&self) -> &GraphPattern {
        match self {
            Self::Select { pattern, .. }
            | Self::Construct { pattern, .. }
            | Self::Describe { pattern, .. }
            | Self::Ask { pattern, .. } => pattern,
        }
    }

    pub fn base_iri(&self) -> Option<&Iri<String>> {
        match self {
            Self::Select { base_iri, .. }
            | Self::Construct { base_iri, .. }
            | Self::Describe { base_iri, .. }
            | Self::Ask { base_iri, .. } => base_iri.as_ref(),
        }
    }
}

/// Prints SPARQL that parses back to the same query.
impl fmt::Display for Query {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writer::fmt_query(self, f)
    }
}

/// A SPARQL update: a sequence of operations.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct Update {
    pub base_iri: Option<Iri<String>>,
    pub operations: Vec<GraphUpdateOperation>,
}

impl Update {
    /// Parses with the default options and an optional base IRI.
    pub fn parse(update: &str, base_iri: Option<&str>) -> Result<Self, SparqlSyntaxError> {
        let mut parser = SparqlParser::new();
        if let Some(base) = base_iri {
            parser = parser.with_base_iri(base)?;
        }
        parser.parse_update(update)
    }
}

/// Prints SPARQL that parses back to the same update.
impl fmt::Display for Update {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writer::fmt_update(self, f)
    }
}

/// One operation of an update. `ADD`, `MOVE` and `COPY` stay one operation each
/// ([`Self::Transfer`]): which graphs their names denote, and so whether a transfer is one of a
/// graph to itself, is for the store to say, not the syntax.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub enum GraphUpdateOperation {
    InsertData {
        data: Vec<Quad>,
    },
    DeleteData {
        data: Vec<GroundQuad>,
    },
    DeleteInsert {
        delete: Vec<GroundQuadPattern>,
        insert: Vec<QuadPattern>,
        using: Option<QueryDataset>,
        pattern: Box<GraphPattern>,
    },
    Load {
        silent: bool,
        source: NamedNode,
        destination: GraphName,
    },
    Clear {
        silent: bool,
        graph: GraphTarget,
    },
    Create {
        silent: bool,
        graph: NamedNode,
    },
    Drop {
        silent: bool,
        graph: GraphTarget,
    },
    /// `ADD`, `MOVE` or `COPY` (SPARQL 1.1 Update §3.2.3–3.2.5): every statement the Graph Store
    /// holds in `from` inserted into `to`, which `MOVE` and `COPY` empty first and `MOVE` then
    /// empties `from`. Its input is that graph as stored, whatever a `WHERE` clause would read:
    /// not a union default graph, inferred statements or a protocol dataset (SYN-54). Kept
    /// whole until the names are resolved: a transfer between two names of one graph changes
    /// nothing (§3.2.4–3.2.5), and only the store knows which names those are (review r1 of
    /// SYN-54 slice 2, S2-F1).
    Transfer {
        op: TransferOp,
        silent: bool,
        from: GraphName,
        to: GraphName,
    },
}

/// Which transfer a [`GraphUpdateOperation::Transfer`] is.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum TransferOp {
    /// Keeps what `to` held.
    Add,
    /// Empties `to` first, then `from`.
    Move,
    /// Empties `to` first.
    Copy,
}

impl TransferOp {
    /// Its keyword.
    pub fn keyword(self) -> &'static str {
        match self {
            Self::Add => "ADD",
            Self::Move => "MOVE",
            Self::Copy => "COPY",
        }
    }
}

impl fmt::Display for GraphUpdateOperation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writer::fmt_operation(self, f)
    }
}
