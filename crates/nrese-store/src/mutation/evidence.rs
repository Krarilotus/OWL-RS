//! What a gate's rejection tells the committer (Q07).
//!
//! A committer that may read every graph is told all the gate found. One that may read only
//! some graphs is told what its own view shows: the statements asserted in graphs it may
//! read, its own request, and that the commit was rejected (the global gates' accept/reject
//! answer: docs/http-api.md, D-1). Its evidence is found in that view, never cut out of
//! evidence found over every graph: which gate rejected, which violation or justification
//! a search over every graph picks, how many there are, which engine decided and how long
//! it took all depend on statements it may not read. So it is told the first reason its view
//! shows by itself, in the gates' order (OWL 2 DL, SHACL), whichever gate rejected, or
//! [`nothing_shown`]. (The OWL 2 RL reason of its view isn't looked for yet: finding it
//! needs a ground program of its view, bounded and charged: Q07 S1b.) Two states it can't tell apart give it the same rejection, but for
//! what the operation's budget leaves and the order of term ids (D-1).
//!
//! Every [`MutationReject`] is made here: its [`Projected`] field has no other constructor,
//! so a rejection path that skips the view doesn't compile.

use nrese_engine::{EncodedQuad, QuadPattern, ReadModel, Snapshot, TermId, Transaction};
use nrese_rdf::{Quad, Term, TermRef};
use nrese_reasoner::{RejectEvidence, RejectExplanation};
use nrese_sparql::{GraphAccess, QueryEvaluationError, ReadView};
use std::borrow::Cow;

use super::MutationReject;
use super::attribution::RejectAttribution;
use crate::error::StoreError;

/// Proof that a [`MutationReject`] was made by [`EvidenceView::reject`].
#[derive(Debug, Clone)]
pub struct Projected(());

/// A rejection's parts, before [`EvidenceView::reject`] makes them one.
pub(crate) struct Draft {
    pub detail: String,
    pub explanation: Option<RejectExplanation>,
    pub attribution: Option<RejectAttribution>,
}

/// Ends every restricted committer's rejection.
pub(crate) const ONLY_READABLE: &str = "Only statements in graphs you can read are shown.";

/// The one problem told of a shapes graph the committer may not read
/// ([`EvidenceView::shapes_error`]).
pub(crate) const SHAPES_UNREADABLE: &str =
    "the shapes graph has problems; it is a graph you can't read, so they aren't shown";

/// The committer's view of a transaction's state, for its rejection.
pub(crate) struct EvidenceView<'v, 'e> {
    /// `None`: it may read every graph.
    access: Option<&'v GraphAccess>,
    tx: &'v Transaction<'e>,
}

impl<'v, 'e> EvidenceView<'v, 'e> {
    pub fn new(read: &'v crate::ReadScope, tx: &'v Transaction<'e>) -> Self {
        Self {
            access: read.access().map(|access| &**access),
            tx,
        }
    }

    /// Whether the committer may read only some graphs.
    pub fn restricted(&self) -> bool {
        self.access.is_some()
    }

    pub fn tx(&self) -> &'v Transaction<'e> {
        self.tx
    }

    /// Whether the committer may read `graph`.
    pub fn graph(&self, graph: TermId) -> bool {
        self.access
            .is_none_or(|access| access.allows_id(self.tx, graph))
    }

    /// `view` as the committer sees it: only the statements asserted in graphs it may read.
    /// `None` for a committer that may read every graph.
    pub fn readable<'w, V: ReadView>(&self, view: &'w V) -> Option<Readable<'w, V>>
    where
        'v: 'w,
    {
        self.access.map(|access| Readable { view, access })
    }

    /// `error` as the committer may be told it. The problems of a shapes graph it may not
    /// read ([`StoreError::ShaclShapes`]: which shape, which list, which term) tell of that
    /// graph's statements: such a refusal comes with one message, the same for every such
    /// graph. That it is refused is the gate's answer (D-1). Any other error, and every
    /// error for a committer that may read the shapes graph, is as it was.
    pub fn shapes_error(&self, shapes_graph: &str, error: StoreError) -> StoreError {
        match error {
            StoreError::ShaclShapes(_)
                if self
                    .access
                    .is_some_and(|access| !access.allows(shapes_graph)) =>
            {
                StoreError::ShaclShapes(vec![SHAPES_UNREADABLE.to_owned()])
            }
            error => error,
        }
    }

    /// The rejection: `full` for a committer that may read every graph, else `view`,
    /// which may use only what this view admits.
    pub fn reject(
        &self,
        full: impl FnOnce() -> Draft,
        view: impl FnOnce(&Self) -> Draft,
    ) -> MutationReject {
        let Draft {
            detail,
            explanation,
            attribution,
        } = match self.restricted() {
            false => full(),
            true => view(self),
        };
        MutationReject {
            detail,
            explanation,
            attribution,
            _projected: Projected(()),
        }
    }
}

/// A restricted rejection whose view shows no reason by itself (or none within the
/// operation's budget): the same for every such view, whichever gate rejected.
pub(crate) fn nothing_shown() -> Draft {
    Draft {
        detail: format!(
            "the mutation was rejected; the graphs you can read show no reason by themselves \
             (within the operation's budget). {ONLY_READABLE}"
        ),
        explanation: None,
        attribution: None,
    }
}

/// A restricted rejection that shows `reason`, found in the committer's view by itself,
/// with its `evidence` (statements of graphs it may read).
pub(crate) fn shown(
    reason: String,
    constraint: &str,
    focus: String,
    evidence: Vec<RejectEvidence>,
) -> Draft {
    Draft {
        detail: format!(
            "the mutation was rejected; in the graphs you can read, {reason}. {ONLY_READABLE}"
        ),
        explanation: Some(RejectExplanation {
            summary: reason,
            violated_constraint: constraint.to_owned(),
            focus_resource: focus,
            evidence,
        }),
        attribution: None,
    }
}

/// The likely commit-local trigger sentence of `attribution`, or nothing.
pub(crate) fn trigger(attribution: Option<&RejectAttribution>) -> String {
    attribution
        .and_then(RejectAttribution::likely_commit_trigger)
        .map_or_else(String::new, |(s, p, o)| {
            format!(" Likely commit-local trigger: <{s}> <{p}> <{o}>.")
        })
}

/// A [`ReadView`] as a committer that may read only some graphs sees it: the statements
/// asserted in graphs it may read (none inferred: a pending state's inferences may follow
/// from graphs it may not read). For SHACL Core, which reads through the quad methods;
/// SHACL-SPARQL evaluates over [`ReadView::evaluation_snapshot`], which isn't filtered, so
/// it is never validated through this view (`Shapes::uses_sparql`).
pub(crate) struct Readable<'w, V> {
    view: &'w V,
    access: &'w GraphAccess,
}

impl<V: ReadView> Readable<'_, V> {
    fn admits(&self, graph: TermId) -> bool {
        self.access.allows_id(self.view, graph)
    }
}

impl<'w, V: ReadView> ReadView for Readable<'w, V> {
    fn quads_for_pattern_in<'a>(
        &'a self,
        _model: ReadModel,
        pattern: &QuadPattern,
    ) -> impl Iterator<Item = EncodedQuad> + use<'a, 'w, V> {
        self.view
            .quads_for_pattern_in(ReadModel::Asserted, pattern)
            .filter(move |quad| self.admits(quad.graph))
    }

    fn named_graphs<'a>(&'a self) -> impl Iterator<Item = TermId> + use<'a, 'w, V> {
        self.view
            .named_graphs()
            .filter(move |&graph| self.admits(graph))
    }

    fn contains_named_graph(&self, graph: TermId) -> bool {
        self.admits(graph) && self.view.contains_named_graph(graph)
    }

    fn lookup(&self, term: TermRef<'_>) -> Option<TermId> {
        self.view.lookup(term)
    }

    fn decode(&self, id: TermId) -> Option<Term> {
        self.view.decode(id)
    }

    fn decode_quad(&self, quad: EncodedQuad) -> Option<Quad> {
        self.view.decode_quad(quad)
    }

    /// Unfiltered: only SHACL-SPARQL reads it, and it isn't validated through this view.
    fn evaluation_snapshot(&self) -> Result<Cow<'_, Snapshot>, QueryEvaluationError> {
        self.view.evaluation_snapshot()
    }
}
