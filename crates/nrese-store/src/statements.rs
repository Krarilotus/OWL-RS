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
use nrese_rdf::{GraphName, GraphNameRef, NamedNode, Quad, QuadRef, Term, TermRef};
use nrese_sparql::ReadView;

use crate::error::StoreResult;
use crate::query::GraphResultFormat;
use crate::rdf_io::{BlankNodes, each_payload_quad};
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
    /// The payload's statements placed per `contexts` (module docs), each to `each` as it
    /// is read: once per context if any are given.
    fn each_quad(
        &self,
        contexts: &[GraphName],
        blank_nodes: BlankNodes,
        mut each: impl FnMut(QuadRef<'_>) -> StoreResult<()>,
    ) -> StoreResult<()> {
        each_payload_quad(
            self.format.rdf_format(),
            self.base_iri.as_deref(),
            &self.payload,
            blank_nodes,
            |quad| {
                if contexts.is_empty() {
                    return each(quad);
                }
                contexts.iter().try_for_each(|graph| {
                    each(QuadRef::new(
                        quad.subject,
                        quad.predicate,
                        quad.object,
                        graph.as_ref(),
                    ))
                })
            },
        )
    }

    /// Whether the payload is RDF in its format: read to its end, nothing kept.
    fn check(&self, blank_nodes: BlankNodes) -> StoreResult<()> {
        self.each_quad(&[], blank_nodes, |_| Ok(()))
    }
}

impl StatementOp {
    /// An RDF4J transaction's `DELETE` of `data` ([`StatementOp::RemovePatterns`]): the
    /// payload is read once now, so one that isn't RDF is refused when it is sent (as
    /// RDF4J's server refuses it), and kept as it came for when it is applied.
    pub fn remove_patterns(
        data: RdfPayload,
        contexts: Vec<GraphName>,
        preserve_blank_nodes: bool,
    ) -> StoreResult<Self> {
        data.check(blank_nodes(preserve_blank_nodes))?;
        Ok(Self::RemovePatterns {
            data,
            contexts,
            preserve_blank_nodes,
        })
    }
}

/// Fresh blank nodes, or (`preserveNodeId`) the payload's labels, which name the store's.
fn blank_nodes(preserve: bool) -> BlankNodes {
    match preserve {
        true => BlankNodes::AsWritten,
        false => BlankNodes::Fresh,
    }
}

/// `sesame:wildcard`: in a transaction's `DELETE` payload, any value at that position.
const WILDCARD: &str = "http://www.openrdf.org/schema/sesame#wildcard";
/// `rdf4j:nil`: in a `DELETE` payload's graph position, the default graph.
const NIL: &str = "http://rdf4j.org/schema/rdf4j#nil";

/// The pattern of a statement of an RDF4J transaction's `DELETE` payload
/// ([`StatementOp::RemovePatterns`]).
fn removal_pattern(quad: QuadRef<'_>, contexts: &[GraphName]) -> StatementPattern {
    let wildcard =
        |term: TermRef<'_>| matches!(term, TermRef::NamedNode(node) if node.as_str() == WILDCARD);
    let subject: TermRef<'_> = quad.subject.into();
    StatementPattern {
        subject: (!wildcard(subject)).then(|| subject.into_owned()),
        predicate: (quad.predicate.as_str() != WILDCARD).then(|| quad.predicate.into_owned()),
        object: (!wildcard(quad.object)).then(|| quad.object.into_owned()),
        contexts: match quad.graph_name {
            GraphNameRef::DefaultGraph => contexts.to_vec(),
            GraphNameRef::NamedNode(node) if node.as_str() == NIL => vec![GraphName::DefaultGraph],
            graph => vec![graph.into_owned()],
        },
    }
}

/// Fails unless the write may change `graph` (no copy where every graph is writable).
fn check_write(requester: &crate::Requester, graph: GraphNameRef<'_>) -> StoreResult<()> {
    match requester.write.access() {
        Some(_) => requester.write.check(&graph.into_owned()),
        None => Ok(()),
    }
}

/// Removes what `pattern` matches in the graphs the requester reads; every graph it would
/// change must be writable.
fn remove_matching(
    tx: &mut Transaction<'_>,
    pattern: &StatementPattern,
    requester: &crate::Requester,
) -> StoreResult<()> {
    let patterns = pattern.patterns(&*tx, &requester.read);
    if let Some(writable) = requester.write.access() {
        let mut allowed = std::collections::HashSet::new();
        for engine_pattern in &patterns {
            for quad in tx.quads_for_pattern_in(ReadModel::Asserted, engine_pattern) {
                if allowed.contains(&quad.graph) {
                    continue;
                }
                if !writable.allows_id(&*tx, quad.graph) {
                    let graph = crate::mutation::command::graph_name(tx, quad.graph);
                    return requester.write.check(&graph);
                }
                allowed.insert(quad.graph);
            }
        }
    }
    for engine_pattern in patterns {
        tx.remove_matching(&engine_pattern);
    }
    Ok(())
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
    /// An RDF4J transaction's `DELETE` (made with [`StatementOp::remove_patterns`]): each
    /// statement of the payload a pattern, as RDF4J's server reads it (its client removes
    /// this way, `clear()` too). `sesame:wildcard` matches any value; a statement without a
    /// graph removes from every graph (or from `contexts`), one in `rdf4j:nil` from the
    /// default graph; with `preserve_blank_nodes` (`preserveNodeId`), blank nodes refer to
    /// the store's. The payload is kept as it came and read again when applied: never as
    /// a pattern per statement.
    RemovePatterns {
        data: RdfPayload,
        contexts: Vec<GraphName>,
        preserve_blank_nodes: bool,
    },
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
        // Each statement of a payload as it is read; one that fails part-way fails the
        // request, whose transaction is then discarded with all it changed.
        match op {
            StatementOp::Add { data, contexts } => {
                data.each_quad(contexts, BlankNodes::Fresh, |quad| {
                    check_write(requester, quad.graph_name)?;
                    tx.insert(quad);
                    Ok(())
                })?;
            }
            StatementOp::RemoveData { data, contexts } => {
                data.each_quad(contexts, BlankNodes::Fresh, |quad| {
                    check_write(requester, quad.graph_name)?;
                    tx.remove(quad);
                    Ok(())
                })?;
            }
            StatementOp::RemoveMatching(pattern) => remove_matching(tx, pattern, requester)?,
            StatementOp::RemovePatterns {
                data,
                contexts,
                preserve_blank_nodes,
            } => {
                data.each_quad(&[], blank_nodes(*preserve_blank_nodes), |quad| {
                    remove_matching(tx, &removal_pattern(quad, contexts), requester)
                })?;
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
    let patterns = pattern.patterns(view, scope);
    let format = (format.rdf_format(), quads_kept);
    crate::rdf_io::write_quads(view, model, &patterns, format, cancel, out).map(|(n, _)| n)
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
