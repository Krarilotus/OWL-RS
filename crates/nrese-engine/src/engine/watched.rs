//! A store-owned observation of one asserted named graph, carried by its versions.

use std::ops::{Deref, DerefMut};
use std::sync::{Arc, OnceLock};

use nrese_rdf::NamedNode;

use super::{Certificate, Engine, Shared, VersionData};
use crate::durability::codec::CommitRecord;
use crate::index::IndexVersion;
use crate::{EngineError, EngineResult, QuadPattern, TermId};

/// One dataset version. Published versions are immutable and snapshots share them by
/// `Arc`. Its asserted and inferred stacks are disjoint.
#[derive(Debug)]
pub(crate) struct Version {
    data: VersionData,
    watched: Option<Arc<Watched>>,
}

/// Every version construction chooses what it knows about the watched graph.
pub(crate) enum Watch<'a> {
    Keep(&'a Version),
    Renew {
        enabled: bool,
        value: Option<Certificate>,
    },
}

impl Watch<'_> {
    pub(crate) fn unwatched() -> Self {
        Self::Renew {
            enabled: false,
            value: None,
        }
    }

    pub(crate) fn renew(value: Option<Certificate>) -> Self {
        Self::Renew {
            enabled: true,
            value,
        }
    }
}

impl Version {
    pub(crate) fn new(watch: Watch<'_>, data: VersionData) -> Self {
        let watched = match watch {
            Watch::Keep(base) => base.watched.clone(),
            Watch::Renew { enabled, value } => enabled.then(|| {
                Arc::new(Watched {
                    changed_at: data.revision,
                    kept: value.map_or_else(OnceLock::new, OnceLock::from),
                })
            }),
        };
        Self { data, watched }
    }

    pub(super) fn watched(&self) -> Option<WatchedRef<'_>> {
        self.watched.as_deref().map(WatchedRef)
    }
}

impl Deref for Version {
    type Target = VersionData;

    fn deref(&self) -> &Self::Target {
        &self.data
    }
}

impl DerefMut for Version {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.data
    }
}

#[derive(Debug)]
struct Watched {
    changed_at: u64,
    kept: OnceLock<Certificate>,
}

/// A published snapshot's cell for its watched graph.
#[derive(Clone, Copy)]
pub struct WatchedRef<'a>(&'a Watched);

impl<'a> WatchedRef<'a> {
    /// The revision that last changed the graph, or the revision at registration.
    pub fn changed_at(self) -> u64 {
        self.0.changed_at
    }

    /// The value set for these asserted statements, if one is ready.
    pub fn kept(self) -> Option<&'a Certificate> {
        self.0.kept.get()
    }

    /// Sets a value for these asserted statements. The first fill wins.
    pub fn keep(self, value: Certificate) -> bool {
        self.0.kept.set(value).is_ok()
    }
}

pub(super) struct WatchedGraph {
    iri: NamedNode,
    id: OnceLock<TermId>,
}

impl Engine {
    /// Registers the asserted named graph to watch, once, before the caller's first write.
    /// The current (possibly recovered) revision starts with an empty cell.
    pub fn watch_graph(&self, iri: NamedNode) -> EngineResult<()> {
        let _slot = super::admit(&self.inner.writer, "writer")?;
        let shared = &self.inner.shared;
        let id = shared.dictionary.lookup(iri.as_ref().into());
        let graph = WatchedGraph {
            iri,
            id: id.map_or_else(OnceLock::new, OnceLock::from),
        };
        shared.watched_graph.set(graph).map_err(|_| {
            EngineError::Configuration("the engine already watches a graph".to_owned())
        })?;
        shared
            .versions
            .publish(|current| Version::new(Watch::renew(None), current.copy_data()));
        Ok(())
    }
}

impl Shared {
    pub(super) fn watched_id(&self) -> Option<TermId> {
        let graph = self.watched_graph.get()?;
        graph.id.get().copied().or_else(|| {
            // A graph with statements already has an id. Watching it never interns it.
            let id = self.dictionary.lookup(graph.iri.as_ref().into())?;
            let _ = graph.id.set(id);
            Some(id)
        })
    }

    /// Decides the cell before readers can see the bulk publication. At this point even
    /// the streamed and spilled paths have a complete asserted index to compare.
    pub(super) fn watch_after_bulk<'a>(
        &self,
        base: &'a Version,
        asserted: &IndexVersion,
        identity: super::AssertedIdentity,
    ) -> Watch<'a> {
        if identity != base.asserted_content
            && let Some(graph) = self.watched_id()
        {
            let pattern = QuadPattern::in_graph(graph);
            if !base.asserted.scan(&pattern).eq(asserted.scan(&pattern)) {
                return Watch::renew(None);
            }
        }
        Watch::Keep(base)
    }
}

/// Whether the record needs the quad layout and whether it changes the watched graph.
/// While the layout is still the default graph's, one pass answers both questions.
pub(super) fn record_changes(
    record: &CommitRecord,
    graph: Option<TermId>,
    default_layout: bool,
) -> (bool, bool) {
    let Some(graph) = graph else {
        return (default_layout && record.names_a_graph(), false);
    };
    let mut quads = record.inserts.iter().chain(&record.deletes);
    if !default_layout {
        return (false, quads.any(|quad| quad.graph == graph));
    }
    let mut named = false;
    for quad in quads {
        if quad.graph == graph {
            return (true, true);
        }
        named |= !quad.graph.is_default_graph();
    }
    (named, false)
}
