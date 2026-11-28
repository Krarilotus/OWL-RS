use std::borrow::Cow;
use std::sync::atomic::{AtomicBool, Ordering};

use nrese_engine::{EncodedQuad, QuadPattern, ReadModel, Snapshot, TermId};
use nrese_rdf::{Quad, Term, TermRef};
use nrese_sparql::{CancellationToken, QueryEvaluationError, ReadView, Resources};

use super::observe::Observation;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Stop {
    Before,
    After,
    Envelope(usize),
}

impl Stop {
    pub(super) fn resources(self) -> Resources {
        match self {
            Self::Before | Self::After => Resources {
                cancellation: Some(CancellationToken::new()),
                ..Resources::default()
            },
            Self::Envelope(bytes) => Resources {
                memory: nrese_exec::Envelope::new(Some(bytes), None),
                ..Resources::default()
            },
        }
    }
}

#[derive(Debug)]
pub(super) enum Outcome {
    Report(Observation),
    Cancelled { before: bool, after: bool },
    MemoryLimit,
}

pub(super) struct TripView<'v> {
    inner: &'v Snapshot,
    token: Option<&'v CancellationToken>,
    tripped: AtomicBool,
}

impl<'v> TripView<'v> {
    pub(super) fn new(inner: &'v Snapshot, token: Option<&'v CancellationToken>) -> Self {
        Self {
            inner,
            token,
            tripped: AtomicBool::new(false),
        }
    }

    pub(super) fn tripped(&self) -> bool {
        self.tripped.load(Ordering::Relaxed)
    }
}

impl<'v> ReadView for TripView<'v> {
    fn quads_for_pattern_in<'a>(
        &'a self,
        model: ReadModel,
        pattern: &QuadPattern,
    ) -> impl Iterator<Item = EncodedQuad> + use<'a, 'v> {
        if let Some(token) = self.token
            && !self.tripped.swap(true, Ordering::Relaxed)
        {
            token.cancel();
        }
        self.inner.quads_for_pattern_in(model, pattern)
    }

    fn named_graphs<'a>(&'a self) -> impl Iterator<Item = TermId> + use<'a, 'v> {
        ReadView::named_graphs(self.inner)
    }

    fn contains_named_graph(&self, graph: TermId) -> bool {
        ReadView::contains_named_graph(self.inner, graph)
    }

    fn lookup(&self, term: TermRef<'_>) -> Option<TermId> {
        ReadView::lookup(self.inner, term)
    }

    fn decode(&self, id: TermId) -> Option<Term> {
        ReadView::decode(self.inner, id)
    }

    fn decode_quad(&self, quad: EncodedQuad) -> Option<Quad> {
        ReadView::decode_quad(self.inner, quad)
    }

    fn snapshot(&self) -> Option<&Snapshot> {
        Some(self.inner)
    }

    fn evaluation_snapshot(&self) -> Result<Cow<'_, Snapshot>, QueryEvaluationError> {
        ReadView::evaluation_snapshot(self.inner)
    }
}
