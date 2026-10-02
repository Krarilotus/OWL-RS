//! RDF4J's statement operations, what its REST protocol's `/statements`, `/size`,
//! `/contexts` and transactions ask of a repository: reads by pattern, and writes applied
//! in order in one transaction.
//!
//! A pattern's `contexts` follow RDF4J's `context` parameter: none means every graph,
//! otherwise the listed graphs, the default graph among them as [`GraphName::DefaultGraph`]
//! (RDF4J's `null`). Writes with contexts put each statement into each of them, whatever
//! graph the payload names; without, into the graph the payload names (the default graph
//! for triples).

use nrese_engine::{GraphSelector, QuadPattern, ReadModel, TermId, Transaction};
use nrese_rdf::{GraphName, NamedNode, Quad, Term, TermRef};
use nrese_sparql::ReadView;

use crate::error::StoreResult;
use crate::query::GraphResultFormat;
use crate::rdf_io::parse_payload;
use crate::update::SparqlUpdateRequest;
use crate::view::decoded_quads;

/// Statements by subject, predicate, object and graph; `None` matches anything.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct StatementPattern {
    pub subject: Option<Term>,
    pub predicate: Option<NamedNode>,
    pub object: Option<Term>,
    /// Empty: every graph. Otherwise these graphs.
    pub contexts: Vec<GraphName>,
}

impl StatementPattern {
    /// The engine's patterns for this one in `view`: one per context (or one for every
    /// graph); a pattern with a term the view doesn't know matches nothing and is left out.
    fn patterns(&self, view: &impl ReadView) -> Vec<QuadPattern> {
        let id = |term: Option<TermRef<'_>>| -> Option<Option<TermId>> {
            match term {
                None => Some(None),
                Some(term) => view.lookup(term).map(Some),
            }
        };
        let (Some(subject), Some(predicate), Some(object)) = (
            id(self.subject.as_ref().map(Term::as_ref)),
            id(self.predicate.as_ref().map(|p| p.as_ref().into())),
            id(self.object.as_ref().map(Term::as_ref)),
        ) else {
            return Vec::new();
        };
        let pattern = |graph| QuadPattern {
            subject,
            predicate,
            object,
            graph,
        };
        if self.contexts.is_empty() {
            return vec![pattern(GraphSelector::Any)];
        }
        let mut graphs: Vec<TermId> = self
            .contexts
            .iter()
            .filter_map(|context| match context {
                GraphName::DefaultGraph => Some(TermId::DEFAULT_GRAPH),
                GraphName::NamedNode(n) => view.lookup(n.as_ref().into()),
                GraphName::BlankNode(b) => view.lookup(b.as_ref().into()),
            })
            .collect();
        graphs.sort_unstable();
        graphs.dedup();
        graphs
            .into_iter()
            .map(|graph| pattern(GraphSelector::Exact(graph)))
            .collect()
    }
}

/// An RDF document in a request.
#[derive(Debug, Clone)]
pub struct RdfPayload {
    pub payload: Vec<u8>,
    pub format: GraphResultFormat,
    pub base_iri: Option<String>,
}

impl RdfPayload {
    /// The payload's statements placed per `contexts` (module docs).
    fn quads(&self, contexts: &[GraphName]) -> StoreResult<Vec<Quad>> {
        let parsed = parse_payload(self.format, self.base_iri.as_deref(), &self.payload)?;
        if contexts.is_empty() {
            return Ok(parsed);
        }
        Ok(parsed
            .iter()
            .flat_map(|quad| {
                contexts.iter().map(|graph| {
                    Quad::new(
                        quad.subject.clone(),
                        quad.predicate.clone(),
                        quad.object.clone(),
                        graph.clone(),
                    )
                })
            })
            .collect())
    }
}

/// One write of a [`StatementsRequest`].
#[derive(Debug, Clone)]
pub enum StatementOp {
    /// Adds the payload's statements, into `contexts` if any are given.
    Add {
        data: RdfPayload,
        contexts: Vec<GraphName>,
    },
    /// Removes the payload's statements, placed as `Add` would place them. Blank nodes in
    /// the payload are new ones, so statements with blank nodes match nothing.
    RemoveData {
        data: RdfPayload,
        contexts: Vec<GraphName>,
    },
    /// Removes every statement matching the pattern.
    RemoveMatching(StatementPattern),
    /// A SPARQL update.
    Update(SparqlUpdateRequest),
}

/// Writes applied in order in one transaction: an RDF4J transaction's operations, or one
/// request to `/statements`.
#[derive(Debug, Clone, Default)]
pub struct StatementsRequest {
    pub ops: Vec<StatementOp>,
}

/// Applies the operations to `tx`, the updates through `update` (the pipeline's SPARQL
/// update path, with its dataset and cancellation).
pub(crate) fn apply_statements(
    tx: &mut Transaction<'_>,
    request: &StatementsRequest,
    update: &mut dyn FnMut(&mut Transaction<'_>, &SparqlUpdateRequest) -> StoreResult<()>,
) -> StoreResult<()> {
    for op in &request.ops {
        match op {
            StatementOp::Add { data, contexts } => {
                for quad in data.quads(contexts)? {
                    tx.insert(quad.as_ref());
                }
            }
            StatementOp::RemoveData { data, contexts } => {
                for quad in data.quads(contexts)? {
                    tx.remove(quad.as_ref());
                }
            }
            StatementOp::RemoveMatching(pattern) => {
                for engine_pattern in pattern.patterns(&*tx) {
                    tx.remove_matching(&engine_pattern);
                }
            }
            StatementOp::Update(request) => update(tx, request)?,
        }
    }
    Ok(())
}

/// The statements matching `pattern` in `view` under `model`, decoded.
pub(crate) fn read_statements(
    view: &impl ReadView,
    model: ReadModel,
    pattern: &StatementPattern,
) -> StoreResult<Vec<Quad>> {
    let mut quads = Vec::new();
    for engine_pattern in pattern.patterns(view) {
        for quad in decoded_quads(view, model, &engine_pattern) {
            quads.push(quad?);
        }
    }
    Ok(quads)
}

/// How many statements match `pattern`.
pub(crate) fn count_statements(
    view: &impl ReadView,
    model: ReadModel,
    pattern: &StatementPattern,
) -> u64 {
    pattern
        .patterns(view)
        .iter()
        .map(|p| view.quads_for_pattern_in(model, p).count() as u64)
        .sum()
}

/// The named graphs that hold statements.
pub(crate) fn contexts(view: &impl ReadView) -> Vec<Term> {
    view.named_graphs()
        .filter_map(|graph| view.decode(graph))
        .collect()
}

/// `quads` in `format`: with their graphs in N-Quads and TriG, as triples otherwise.
pub fn serialize_statements(format: GraphResultFormat, quads: Vec<Quad>) -> StoreResult<Vec<u8>> {
    match format {
        GraphResultFormat::NQuads | GraphResultFormat::TriG => {
            crate::rdf_io::serialize_quads(format.rdf_format(), quads)
        }
        _ => crate::rdf_io::serialize_triples(
            format,
            quads
                .into_iter()
                .map(|quad| nrese_rdf::Triple::new(quad.subject, quad.predicate, quad.object)),
        ),
    }
}
