//! RDF4J's statement operations, what its REST protocol's `/statements`, `/size`,
//! `/contexts` and transactions ask of a repository: reads by pattern, and writes applied
//! in order in one transaction.
//!
//! A pattern's `contexts` follow RDF4J's `context` parameter: none means every graph,
//! otherwise the listed graphs, the default graph among them as [`GraphName::DefaultGraph`]
//! (RDF4J's `null`). Writes with contexts put each statement into each of them, whatever
//! graph the payload names; without, into the graph the payload names (the default graph
//! for triples).
//!
//! Graph-level access control comes with the read ([`crate::ReadContext`]) or the write
//! ([`crate::Requester`]): a pattern matches in the readable graphs only (the others are
//! absent), and a write fails as a whole if it would add or remove a statement in a graph
//! its requester may not write, whether the statement is there or not.

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
    /// The engine's patterns for this one in `view`, in the graphs `scope` reads: one per
    /// context (or one for every graph); a pattern with a term the view doesn't know
    /// matches nothing and is left out.
    fn patterns(&self, view: &impl ReadView, scope: &crate::ReadScope) -> Vec<QuadPattern> {
        let access = scope.access();
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
        let mut graphs: Vec<TermId> = match (access, self.contexts.is_empty()) {
            (None, true) => return vec![pattern(GraphSelector::Any)],
            (Some(_), true) => view.named_graphs().chain([TermId::DEFAULT_GRAPH]).collect(),
            (_, false) => self
                .contexts
                .iter()
                .filter_map(|context| match context {
                    GraphName::DefaultGraph => Some(TermId::DEFAULT_GRAPH),
                    GraphName::NamedNode(n) => view.lookup(n.as_ref().into()),
                    GraphName::BlankNode(b) => view.lookup(b.as_ref().into()),
                })
                .collect(),
        };
        if let Some(access) = access {
            graphs.retain(|&graph| access.allows_id(view, graph));
        }
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
    /// The session ([`crate::sessions`]) the operations were collected in, if any: reads on
    /// them keep the data they see there until the session or the store changes.
    pub session: Option<String>,
}

impl StatementsRequest {
    pub fn new(ops: Vec<StatementOp>) -> Self {
        Self { ops, session: None }
    }
}

/// Applies the operations to `tx` for `requester`, the updates through `update` (the
/// pipeline's SPARQL update path, with its dataset and cancellation).
pub(crate) fn apply_statements(
    tx: &mut Transaction<'_>,
    request: &StatementsRequest,
    requester: &crate::Requester,
    update: &mut dyn FnMut(&mut Transaction<'_>, &SparqlUpdateRequest) -> StoreResult<()>,
) -> StoreResult<()> {
    for op in &request.ops {
        match op {
            StatementOp::Add { data, contexts } => {
                for quad in data.quads(contexts)? {
                    requester.write.check(&quad.graph_name)?;
                    tx.insert(quad.as_ref());
                }
            }
            StatementOp::RemoveData { data, contexts } => {
                for quad in data.quads(contexts)? {
                    requester.write.check(&quad.graph_name)?;
                    tx.remove(quad.as_ref());
                }
            }
            StatementOp::RemoveMatching(pattern) => {
                // The removal matches in the graphs the requester reads.
                let patterns = pattern.patterns(&*tx, &requester.read);
                if let Some(writable) = requester.write.access() {
                    // Every graph the removal would change must be writable.
                    let mut allowed = std::collections::HashSet::new();
                    for engine_pattern in &patterns {
                        for quad in tx.quads_for_pattern_in(ReadModel::Asserted, engine_pattern) {
                            if allowed.contains(&quad.graph) {
                                continue;
                            }
                            if !writable.allows_id(&*tx, quad.graph) {
                                let graph = match tx.decode(quad.graph) {
                                    _ if quad.graph == TermId::DEFAULT_GRAPH => {
                                        GraphName::DefaultGraph
                                    }
                                    Some(Term::NamedNode(node)) => GraphName::NamedNode(node),
                                    Some(Term::BlankNode(node)) => GraphName::BlankNode(node),
                                    _ => GraphName::DefaultGraph,
                                };
                                return requester.write.check(&graph);
                            }
                            allowed.insert(quad.graph);
                        }
                    }
                }
                for engine_pattern in patterns {
                    tx.remove_matching(&engine_pattern);
                }
            }
            StatementOp::Update(request) => update(tx, request)?,
        }
    }
    Ok(())
}

/// The statements matching `pattern` in `view` under `model` in the graphs `scope`
/// reads, decoded.
pub(crate) fn read_statements(
    view: &impl ReadView,
    model: ReadModel,
    pattern: &StatementPattern,
    scope: &crate::ReadScope,
) -> StoreResult<Vec<Quad>> {
    let mut quads = Vec::new();
    for engine_pattern in pattern.patterns(view, scope) {
        for quad in decoded_quads(view, model, &engine_pattern) {
            quads.push(quad?);
        }
    }
    Ok(quads)
}

/// The statements matching `pattern` written to `out` in `format` as they are read: with
/// their graphs in N-Quads, TriG and Binary RDF, as triples otherwise. Returns how many.
pub(crate) fn write_statements(
    view: &impl ReadView,
    model: ReadModel,
    pattern: &StatementPattern,
    scope: &crate::ReadScope,
    format: GraphResultFormat,
    cancel: &nrese_sparql::CancellationToken,
    out: impl std::io::Write,
) -> StoreResult<u64> {
    let quads_kept = matches!(
        format,
        GraphResultFormat::NQuads | GraphResultFormat::TriG | GraphResultFormat::BinaryRdf
    );
    let mut writer = nrese_rdf_io::RdfSerializer::from_format(format.rdf_format()).for_writer(out);
    let mut written = 0;
    for engine_pattern in pattern.patterns(view, scope) {
        for quad in decoded_quads(view, model, &engine_pattern) {
            if cancel.is_cancelled() {
                return Err(nrese_sparql::QueryEvaluationError::Cancelled.into());
            }
            let quad = quad?;
            match quads_kept {
                true => writer.serialize_quad(&quad)?,
                false => writer.serialize_triple(&nrese_rdf::Triple::new(
                    quad.subject,
                    quad.predicate,
                    quad.object,
                ))?,
            }
            written += 1;
        }
    }
    writer.finish()?;
    Ok(written)
}

/// How many statements match `pattern`.
pub(crate) fn count_statements(
    view: &impl ReadView,
    model: ReadModel,
    pattern: &StatementPattern,
    scope: &crate::ReadScope,
) -> u64 {
    pattern
        .patterns(view, scope)
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
