//! Actual load identities, collected internally; never a caller callback under a writer.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};

use hashbrown::HashTable;
use nrese_exec::{ChargedVec, Envelope, Reservation};
use nrese_rdf::{BlankNode, BlankNodeRef, GraphName, GraphNameRef, NamedNodeRef};
use nrese_sparql::Resources;
use parking_lot::Mutex;

use super::LoadProgress;
use crate::{StoreError, StoreResult};

#[derive(Debug, Clone, Copy)]
struct Text {
    start: usize,
    len: usize,
}

#[derive(Debug)]
struct Node {
    input: Text,
    scoped: Text,
}

#[derive(Debug, Clone, Copy)]
struct Graph {
    kind: u8,
    text: Text,
}

/// The complete source/blank-node correspondence of one successful observed load.
/// No clone or owned extraction: all borrowed reads retain their original backing/lease.
#[derive(Debug)]
pub struct LoadIdentities {
    files: ChargedVec<PathBuf, Envelope>,
    nodes: ChargedVec<Node, Envelope>,
    graphs: ChargedVec<Graph, Envelope>,
    associations: ChargedVec<(usize, usize, usize), Envelope>,
    text: ChargedVec<u8, Envelope>,
    node_index: HashTable<(u64, usize)>,
    graph_index: HashTable<(u64, usize)>,
    association_index: HashTable<(u64, usize)>,
    table_bytes: [usize; 3],
    scope: Text,
    // Path payloads and hash backing go before this reservation on every drop/unwind.
    held: Reservation<Envelope>,
}

impl LoadIdentities {
    /// Request entries in their original order, including repeated identical paths.
    pub fn files(&self) -> &[PathBuf] {
        &self.files
    }

    /// The actual loader scope, not its optional quarantine identifier.
    pub fn load_scope(&self) -> &str {
        self.string(self.scope)
    }

    pub fn node_count(&self) -> usize {
        self.nodes.len()
    }

    /// Stable capture-local node ID: actual input and actual scoped labels.
    pub fn node(&self, id: usize) -> Option<(BlankNodeRef<'_>, BlankNodeRef<'_>)> {
        self.nodes.get(id).map(|node| {
            (
                BlankNodeRef::new_unchecked(self.string(node.input)),
                BlankNodeRef::new_unchecked(self.string(node.scoped)),
            )
        })
    }

    pub fn graph_count(&self) -> usize {
        self.graphs.len()
    }

    /// Stable capture-local graph ID, after the real loader's graph scoping.
    pub fn graph(&self, id: usize) -> Option<GraphNameRef<'_>> {
        self.graphs.get(id).map(|graph| match graph.kind {
            0 => GraphNameRef::DefaultGraph,
            1 => NamedNodeRef::new_unchecked(self.string(graph.text)).into(),
            2 => BlankNodeRef::new_unchecked(self.string(graph.text)).into(),
            _ => unreachable!("only concrete loader graph kinds"),
        })
    }

    /// Distinct (zero-based file entry, node ID, effective graph ID) associations.
    /// Their order reflects parallel observation, not input order. This deduplicates
    /// source associations only; it never folds validation/report instances.
    pub fn associations(&self) -> &[(usize, usize, usize)] {
        &self.associations
    }

    fn string(&self, text: Text) -> &str {
        std::str::from_utf8(&self.text[text.start..text.start + text.len])
            .expect("copied from typed UTF-8 labels")
    }

    fn new(files: &[PathBuf], work: &Work<'_>) -> StoreResult<Self> {
        let budget = work.resources.memory.clone();
        let mut data = Self {
            files: ChargedVec::new(budget.clone()),
            nodes: ChargedVec::new(budget.clone()),
            graphs: ChargedVec::new(budget.clone()),
            associations: ChargedVec::new(budget.clone()),
            text: ChargedVec::new(budget.clone()),
            node_index: HashTable::new(),
            graph_index: HashTable::new(),
            association_index: HashTable::new(),
            table_bytes: [0; 3],
            scope: Text { start: 0, len: 0 },
            held: Reservation::new(budget),
        };
        data.files.reserve_exact(files.len())?;
        for file in files {
            work.check()?;
            let bytes = file.as_os_str().len();
            data.held.charge(bytes)?;
            let path = file.to_path_buf();
            debug_assert_eq!(path.capacity(), bytes);
            data.files.push(path)?;
            work.check()?;
        }
        Ok(data)
    }

    fn copy(&mut self, value: &str, work: &Work<'_>) -> StoreResult<Text> {
        work.check()?;
        self.text.reserve(value.len())?;
        let text = Text {
            start: self.text.len(),
            len: value.len(),
        };
        for piece in value.as_bytes().chunks(nrese_exec::POLL_UNITS as usize) {
            work.check()?;
            self.text.extend_from_slice(piece)?;
        }
        work.check()?;
        Ok(text)
    }

    fn record(
        &mut self,
        file: usize,
        input: &BlankNode,
        scoped: &BlankNode,
        graph: &GraphName,
        work: &Work<'_>,
    ) -> StoreResult<()> {
        let hash = text_hash(0, input.as_str(), work)?;
        let node = self
            .node_index
            .find(hash, |&(_, id)| {
                self.string(self.nodes[id].input) == input.as_str()
            })
            .map(|&(_, id)| id);
        work.check()?; // Hash-table probing/comparison is an opaque library boundary.
        let node = match node {
            Some(id) => id,
            None => {
                reserve_index(
                    &mut self.node_index,
                    &mut self.table_bytes[0],
                    &mut self.held,
                    work,
                )?;
                self.nodes.reserve(1)?;
                let input = self.copy(input.as_str(), work)?;
                let scoped = self.copy(scoped.as_str(), work)?;
                let id = self.nodes.len();
                self.nodes.push(Node { input, scoped })?;
                self.node_index
                    .insert_unique(hash, (hash, id), |&(hash, _)| hash);
                id
            }
        };
        let (kind, text) = match graph {
            GraphName::DefaultGraph => (0, ""),
            GraphName::NamedNode(node) => (1, node.as_str()),
            GraphName::BlankNode(node) => (2, node.as_str()),
        };
        let hash = text_hash(kind, text, work)?;
        let graph_id = self
            .graph_index
            .find(hash, |&(_, id)| {
                self.graphs[id].kind == kind && self.string(self.graphs[id].text) == text
            })
            .map(|&(_, id)| id);
        work.check()?;
        let graph_id = match graph_id {
            Some(id) => id,
            None => {
                reserve_index(
                    &mut self.graph_index,
                    &mut self.table_bytes[1],
                    &mut self.held,
                    work,
                )?;
                self.graphs.reserve(1)?;
                let text = self.copy(text, work)?;
                let id = self.graphs.len();
                self.graphs.push(Graph { kind, text })?;
                self.graph_index
                    .insert_unique(hash, (hash, id), |&(hash, _)| hash);
                id
            }
        };
        let association = (file, node, graph_id);
        let mut hasher = DefaultHasher::new();
        association.hash(&mut hasher);
        let hash = hasher.finish();
        if self
            .association_index
            .find(hash, |&(_, id)| self.associations[id] == association)
            .is_none()
        {
            reserve_index(
                &mut self.association_index,
                &mut self.table_bytes[2],
                &mut self.held,
                work,
            )?;
            self.associations.reserve(1)?;
            work.check()?;
            let id = self.associations.len();
            self.associations.push(association)?;
            self.association_index
                .insert_unique(hash, (hash, id), |&(hash, _)| hash);
        }
        work.check()
    }
}

// One concrete cached-hash/ID index layout; reuse exec's existing hashbrown bound.
// Full new backing is charged beside the old. No user hashing/drop code is invoked.
fn reserve_index(
    table: &mut HashTable<(u64, usize)>,
    bytes: &mut usize,
    held: &mut Reservation<Envelope>,
    work: &Work<'_>,
) -> StoreResult<()> {
    work.check()?;
    if table.len() < table.capacity() {
        return Ok(());
    }
    let additional = table.capacity().max(3);
    let next = table
        .len()
        .checked_add(additional)
        .filter(|&n| {
            n.checked_mul(8)
                .and_then(|n| (n / 7).checked_next_power_of_two())
                .is_some()
        })
        .ok_or_else(|| held.budget().refusal(usize::MAX))?;
    let bound = nrese_exec::map_bytes::<(u64, usize)>(next);
    std::alloc::Layout::array::<u8>(bound).map_err(|_| held.budget().refusal(usize::MAX))?;
    held.charge(bound)?;
    work.check()?;
    table.reserve(additional, |&(hash, _)| hash);
    debug_assert!(table.allocation_size() <= bound);
    // reserve has already freed its previous backing; panic retains conservative charge.
    held.release(std::mem::replace(bytes, bound));
    work.check()
}

struct Work<'a> {
    resources: &'a Resources,
    progress: &'a LoadProgress,
}

impl Work<'_> {
    fn check(&self) -> StoreResult<()> {
        if self.progress.is_cancelled() {
            return Err(StoreError::LoadCancelled);
        }
        self.resources.check().map_err(Into::into)
    }
}

fn text_hash(kind: u8, text: &str, work: &Work<'_>) -> StoreResult<u64> {
    let mut hash = DefaultHasher::new();
    hash.write_u8(kind);
    for piece in text.as_bytes().chunks(nrese_exec::POLL_UNITS as usize) {
        work.check()?;
        hash.write(piece);
    }
    Ok(hash.finish())
}

struct State {
    data: LoadIdentities,
    failure: Option<StoreError>,
}

pub(super) struct Capture<'a> {
    state: Mutex<State>,
    stopped: AtomicBool,
    work: Work<'a>,
}

impl<'a> Capture<'a> {
    pub(super) fn new(
        files: &[PathBuf],
        resources: &'a Resources,
        progress: &'a LoadProgress,
    ) -> StoreResult<Self> {
        let work = Work {
            resources,
            progress,
        };
        work.check()?;
        Ok(Self {
            state: Mutex::new(State {
                data: LoadIdentities::new(files, &work)?,
                failure: None,
            }),
            stopped: AtomicBool::new(false),
            work,
        })
    }

    fn publish(&self, state: &mut State, result: StoreResult<()>) -> Result<(), ()> {
        match result {
            Ok(()) => Ok(()),
            Err(error) => {
                debug_assert!(state.failure.is_none());
                state.failure = Some(error);
                self.stopped.store(true, Ordering::Release);
                Err(())
            }
        }
    }

    pub(super) fn checkpoint(&self) -> Result<(), ()> {
        if self.stopped.load(Ordering::Acquire) {
            return Err(());
        }
        let mut state = self.state.lock();
        if self.stopped.load(Ordering::Acquire) {
            return Err(());
        }
        self.publish(&mut state, self.work.check())
    }

    pub(super) fn set_scope(&self, scope: &str) -> Result<(), ()> {
        self.checkpoint()?;
        let mut state = self.state.lock();
        if self.stopped.load(Ordering::Acquire) {
            return Err(());
        }
        let result = state
            .data
            .copy(scope, &self.work)
            .map(|text| state.data.scope = text);
        self.publish(&mut state, result)
    }

    pub(super) fn record(
        &self,
        file: usize,
        input: &BlankNode,
        scoped: &BlankNode,
        graph: &GraphName,
    ) -> Result<(), ()> {
        if self.stopped.load(Ordering::Acquire) {
            return Err(());
        }
        let mut state = self.state.lock();
        if self.stopped.load(Ordering::Acquire) {
            return Err(());
        }
        let result = self
            .work
            .check()
            .and_then(|()| state.data.record(file, input, scoped, graph, &self.work));
        self.publish(&mut state, result)
    }

    pub(super) fn finish(
        self,
        result: StoreResult<super::BulkLoadReport>,
    ) -> StoreResult<(super::BulkLoadReport, LoadIdentities)> {
        let State { data, failure } = self.state.into_inner();
        if let Some(error) = failure {
            return Err(error);
        }
        result.map(|report| (report, data))
    }
}

#[cfg(test)]
mod tests;
