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
//! The classes are built at the first use and cached while the snapshot is the latest
//! one queried.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use nrese_engine::{GraphSelector, QuadPattern, ReadModel, Snapshot};
use nrese_rdf::NamedNodeRef;

use super::{Context, GraphScope};

const SAME_AS: &str = "http://www.w3.org/2002/07/owl#sameAs";

/// Each identity's representative (terms outside every class are absent).
pub(super) type Representatives = HashMap<u64, u64>;

static CACHE: Mutex<Option<(Snapshot, Arc<Representatives>)>> = Mutex::new(None);

fn build(snapshot: &Snapshot) -> Representatives {
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
    let Some(same_as) = snapshot.lookup(NamedNodeRef::new_unchecked(SAME_AS).into()) else {
        return out;
    };
    let pattern = QuadPattern {
        subject: None,
        predicate: Some(same_as),
        object: None,
        graph: GraphSelector::Any,
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

impl Context<'_> {
    /// The representatives, if the data is closed under `sameAs` for what this query
    /// reads: the store says so, the query reads the default graph, and the default
    /// graph is everything (the merge of all graphs, or no named graph holds data; a
    /// fact asserted only in a named graph has its identity variants inferred in the
    /// default graph, but not itself).
    pub(super) fn representatives(&self) -> Option<Arc<Representatives>> {
        if !self.equality_closed || self.as_written {
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
        let mut cache = CACHE
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some((snapshot, representatives)) = cache.as_ref()
            && snapshot.same_version(self.snapshot)
        {
            return Some(Arc::clone(representatives));
        }
        let representatives = Arc::new(build(self.snapshot));
        *cache = Some((self.snapshot.clone(), Arc::clone(&representatives)));
        Some(representatives)
    }
}

/// `id`'s representative.
pub(super) fn representative(representatives: &Representatives, id: u64) -> u64 {
    representatives.get(&id).copied().unwrap_or(id)
}
