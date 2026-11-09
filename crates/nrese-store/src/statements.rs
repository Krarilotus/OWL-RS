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

use std::sync::Arc;

use nrese_engine::{GraphSelector, QuadPattern, ReadModel, TermId, Transaction};
use nrese_exec::{Budget, BudgetExceeded, ChargedVec, Reservation};
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

impl StatementOp {
    /// The heap bytes it holds beside its own slot, by capacity: what a holder that keeps it
    /// charges ([`crate::sessions`]).
    pub fn heap_bytes(&self) -> usize {
        match self {
            Self::Add { data, contexts } | Self::RemoveData { data, contexts } => {
                data.payload.capacity()
                    + data.base_iri.as_ref().map_or(0, String::capacity)
                    + graphs_bytes(contexts)
            }
            Self::RemoveMatching(pattern) => {
                pattern.subject.as_ref().map_or(0, Term::heap_bytes)
                    + pattern.predicate.as_ref().map_or(0, NamedNode::heap_bytes)
                    + pattern.object.as_ref().map_or(0, Term::heap_bytes)
                    + graphs_bytes(&pattern.contexts)
            }
            Self::Update(update) => update.heap_bytes(),
        }
    }
}

/// The heap bytes of a list of graphs: its slots and their names.
fn graphs_bytes(graphs: &Vec<GraphName>) -> usize {
    graphs.capacity() * std::mem::size_of::<GraphName>()
        + graphs.iter().map(GraphName::heap_bytes).sum::<usize>()
}

/// A request's operations, shared without copying: a client transaction's reads and its
/// commit hold the one history its session keeps ([`crate::sessions`]), and with it the
/// charge of what the history holds, returned when its last holder lets it go.
#[derive(Debug, Clone, Default)]
pub struct StatementOps(Arc<Held>);

#[derive(Debug)]
enum Held {
    /// One request's own operations, uncharged (they live as long as the request).
    Plain(Vec<StatementOp>),
    /// Kept between requests: their slots charged by the vector, the bytes they hold by
    /// `nested`, both on the keeper's budget.
    Kept {
        ops: ChargedVec<StatementOp>,
        nested: Reservation,
    },
}

impl Default for Held {
    fn default() -> Self {
        Self::Plain(Vec::new())
    }
}

impl std::ops::Deref for StatementOps {
    type Target = [StatementOp];

    fn deref(&self) -> &[StatementOp] {
        match &*self.0 {
            Held::Plain(ops) => ops,
            Held::Kept { ops, .. } => ops,
        }
    }
}

impl From<Vec<StatementOp>> for StatementOps {
    fn from(ops: Vec<StatementOp>) -> Self {
        Self(Arc::new(Held::Plain(ops)))
    }
}

/// An append the budget had no room for, and the operations it would have appended.
pub(crate) type Refused = (BudgetExceeded, Vec<StatementOp>);

impl StatementOps {
    /// `more` appended, everything kept charged to `budget` first (the one budget these were
    /// kept on, if they were): in place where nobody else holds these, else as a copy
    /// charged anew, so that a reader holding these keeps them with their own charge. A
    /// refusal changes nothing and hands `more` back.
    pub(crate) fn append_within(
        &mut self,
        more: Vec<StatementOp>,
        budget: &Arc<Budget>,
    ) -> Result<(), Refused> {
        let bytes: usize = more.iter().map(StatementOp::heap_bytes).sum();
        if let Some(Held::Kept { ops, nested }) = Arc::get_mut(&mut self.0) {
            if let Err(refused) = nested.charge(bytes) {
                return Err((refused, more));
            }
            if let Err(refused) = ops.reserve(more.len()) {
                nested.release(bytes);
                return Err((refused, more));
            }
            push_reserved(ops, more);
            return Ok(());
        }
        let old: &[StatementOp] = self;
        let mut nested = Reservation::new(Arc::clone(budget));
        if let Err(refused) =
            nested.charge(old.iter().map(StatementOp::heap_bytes).sum::<usize>() + bytes)
        {
            return Err((refused, more));
        }
        let mut ops = match ChargedVec::with_capacity(Arc::clone(budget), old.len() + more.len()) {
            Ok(ops) => ops,
            Err(refused) => return Err((refused, more)),
        };
        push_reserved(&mut ops, old.iter().cloned());
        push_reserved(&mut ops, more);
        self.0 = Arc::new(Held::Kept { ops, nested });
        Ok(())
    }
}

/// `items` pushed onto `ops`, which has the room for them reserved.
fn push_reserved(ops: &mut ChargedVec<StatementOp>, items: impl IntoIterator<Item = StatementOp>) {
    // No room to reserve beyond the length, so nothing to refuse; a push past the room
    // panics before it allocates.
    let filled = ops.fill(0, |room| items.into_iter().for_each(|op| room.push(op)));
    debug_assert!(filled.is_ok());
}

/// Writes applied in order in one transaction: an RDF4J transaction's operations, or one
/// request to `/statements`.
#[derive(Debug, Clone, Default)]
pub struct StatementsRequest {
    pub ops: StatementOps,
    /// The session ([`crate::sessions`]) the operations were collected in, if any: reads on
    /// them keep the data they see there until the session or the store changes.
    pub session: Option<String>,
}

impl StatementsRequest {
    pub fn new(ops: impl Into<StatementOps>) -> Self {
        Self {
            ops: ops.into(),
            session: None,
        }
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
    for op in request.ops.iter() {
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
