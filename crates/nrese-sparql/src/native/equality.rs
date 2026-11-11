//! Identity classes of data closed under `owl:sameAs` (work package W4, query side).
//!
//! When the store's inferred stack is current under a ruleset with the equality rules
//! ([`crate::QueryOptions::equality_closed`]), every fact about one identity holds for the
//! others, and the `sameAs` relation itself is complete: every identity of a class states
//! `sameAs` for every other. A term's representative is then the least of its `sameAs`
//! partners, read from one scan. Operators that can't tell identities apart (a join on a
//! key feeding only duplicate-insensitive aggregates) work on representatives
//! ([`super::sets`]).
//!
//! A published snapshot's classes (one with a [`SharedState`]) are built at their first use
//! and kept, process-wide, while it is the latest published snapshot queried; any snapshot of
//! the same version reads them. An unpublished snapshot's (a transaction's pending state, a
//! private or an added-to view) are built for its query alone, charged to the query's
//! budget, and kept nowhere else (the dictionary lifecycle contract's S3).

use std::cell::OnceCell;
use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};

use nrese_engine::{GraphSelector, QuadPattern, ReadModel, SharedState, Snapshot};
use nrese_exec::Reservation;
use nrese_rdf::NamedNodeRef;

use super::{Context, GraphScope};

const SAME_AS: &str = "http://www.w3.org/2002/07/owl#sameAs";

/// Each identity's representative (terms outside every class are absent).
pub(super) type Representatives = HashMap<u64, u64>;

/// The latest published snapshot queried and its representatives: written by [`admit`] alone.
static CACHE: Mutex<Option<(Snapshot, Arc<Representatives>)>> = Mutex::new(None);

/// What a query's own representatives hold besides their table: the `Arc`'s counts and the map.
const SHARED: usize = 2 * size_of::<usize>() + size_of::<Representatives>();

/// A query's representatives ([`Context::representatives`]), found at their first use: the
/// published entry's, or built for the query alone and held with their charge to its budget;
/// `None` where the budget had no room for them.
#[derive(Default)]
pub(super) struct Found(OnceCell<Option<(Arc<Representatives>, Option<Reservation>)>>);

#[cfg(test)]
thread_local! {
    /// Builds begun on this thread, and those its request's budget refused: what the guards
    /// (`equality_tests`) read to know a query reached the classes. A query is evaluated on
    /// its caller's thread.
    static BUILDS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    static REFUSED: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Builds begun on this thread ([`BUILDS`]).
#[cfg(test)]
pub(super) fn builds() -> usize {
    BUILDS.with(std::cell::Cell::get)
}

/// Builds on this thread its request's budget refused ([`REFUSED`]).
#[cfg(test)]
pub(super) fn refused() -> usize {
    REFUSED.with(std::cell::Cell::get)
}

/// Whether the process-wide entry is for `snapshot`'s version.
#[cfg(test)]
pub(super) fn kept_for(snapshot: &Snapshot) -> bool {
    CACHE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .as_ref()
        .is_some_and(|(kept, _)| kept.same_version(snapshot))
}

/// Whether the process-wide entry, if any, was made from a published snapshot.
#[cfg(test)]
pub(super) fn kept_is_published() -> bool {
    CACHE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .as_ref()
        .is_none_or(|(kept, _)| kept.shared_state().is_some())
}

/// The statements that state `sameAs`, where the dictionary has it.
fn same_as(snapshot: &Snapshot) -> Option<QuadPattern> {
    let same_as = snapshot.lookup(NamedNodeRef::new_unchecked(SAME_AS).into())?;
    Some(QuadPattern {
        subject: None,
        predicate: Some(same_as),
        object: None,
        graph: GraphSelector::Any,
    })
}

/// The most entries [`build`] makes of `snapshot`: each class's members, or two per `sameAs`
/// statement, counted exactly (without classes, reads don't expand).
fn bound(snapshot: &Snapshot) -> usize {
    if let Some(classes) = snapshot.equality_classes() {
        return classes.iter().map(|(_, members)| members.len()).sum();
    }
    let Some(pattern) = same_as(snapshot) else {
        return 0;
    };
    let statements = snapshot.count_in(ReadModel::Materialised, &pattern);
    debug_assert!(statements.exact().is_some(), "a count without expansion");
    usize::try_from(statements.at_least()).map_or(usize::MAX, |n| n.saturating_mul(2))
}

fn build(snapshot: &Snapshot) -> Representatives {
    #[cfg(test)]
    BUILDS.with(|builds| builds.set(builds.get() + 1));
    let mut out = HashMap::new();
    // A store kept over representatives knows its classes; its `sameAs` relation, read
    // expanded, would list every pair of each class.
    if let Some(classes) = snapshot.equality_classes() {
        for (representative, members) in classes.iter() {
            for &member in members {
                out.insert(member, representative);
            }
        }
        return out;
    }
    let Some(pattern) = same_as(snapshot) else {
        return out;
    };
    for quad in snapshot.quads_for_pattern_in(ReadModel::Materialised, &pattern) {
        let (s, o) = (quad.subject.raw(), quad.object.raw());
        if s == o {
            continue;
        }
        let least = s.min(o);
        for term in [s, o] {
            let entry = out.entry(term).or_insert(term);
            *entry = (*entry).min(least);
        }
    }
    out
}

/// `state`'s representatives admitted to the process-wide entry, replacing what it held: the
/// entry's only writer, for published state alone. They are built from the token's own
/// snapshot and kept with it.
fn admit(
    cache: &mut Option<(Snapshot, Arc<Representatives>)>,
    state: &SharedState<'_>,
) -> Arc<Representatives> {
    let snapshot = state.snapshot();
    let representatives = Arc::new(build(snapshot));
    *cache = Some((snapshot.clone(), Arc::clone(&representatives)));
    representatives
}

impl Context<'_> {
    /// The representatives, if the data is closed under `sameAs` for what this query
    /// reads: the store says so, the query reads the default graph, and the default
    /// graph is everything (the merge of all graphs, or no named graph holds data; a
    /// fact asserted only in a named graph has its identity variants inferred in the
    /// default graph, but not itself).
    pub(super) fn representatives(&self) -> Option<Arc<Representatives>> {
        if !self.equality_closed || self.as_written || self.model != ReadModel::Materialised {
            return None;
        }
        let scope_ok = match &*self.graph.borrow() {
            GraphScope::Union => self.merge_set.is_none(),
            GraphScope::Default => self.snapshot.named_graphs().next().is_none(),
            _ => false,
        };
        if !scope_ok {
            return None;
        }
        self.representatives
            .0
            .get_or_init(|| self.find_representatives())
            .as_ref()
            .map(|(representatives, _)| Arc::clone(representatives))
    }

    /// The published entry's representatives for this query's version; else, for a published
    /// snapshot, its own admitted; else built for this query alone.
    fn find_representatives(&self) -> Option<(Arc<Representatives>, Option<Reservation>)> {
        let mut cache = CACHE.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some((kept, representatives)) = cache.as_ref()
            && kept.same_version(&self.snapshot)
        {
            return Some((Arc::clone(representatives), None));
        }
        if let Some(state) = self.snapshot.shared_state() {
            return Some((admit(&mut cache, &state), None));
        }
        drop(cache);
        // Unpublished: built for this query alone and kept nowhere else (S3), what they hold
        // charged to the query's budget before it is allocated. Where the budget has no room
        // for them, the query goes on without: they only let a join run on one identity per
        // class where its aggregates can't tell, and the join on every identity gives the same
        // answer (`notes/2026-11-08-dict3-4-equality-design.md`, D1).
        // One charge before anything is made: the most the map's tables hold while it grows to
        // its bound, its last table beside the one it replaces; trimmed to what it holds.
        let mut held = Reservation::new(Arc::clone(&self.budget));
        let table = super::spatial::map_bytes::<u64, u64>;
        let growing = table(bound(&self.snapshot)).saturating_mul(2);
        match held.charge(SHARED.saturating_add(growing)) {
            Ok(()) => {
                let representatives = build(&self.snapshot);
                let holds = SHARED + table(representatives.capacity());
                held.release(held.bytes().saturating_sub(holds));
                Some((Arc::new(representatives), Some(held)))
            }
            Err(_) => {
                #[cfg(test)]
                REFUSED.with(|refused| refused.set(refused.get() + 1));
                None
            }
        }
    }
}

/// `id`'s representative.
pub(super) fn representative(representatives: &Representatives, id: u64) -> u64 {
    representatives.get(&id).copied().unwrap_or(id)
}
